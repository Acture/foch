from __future__ import annotations

import struct
import tempfile
import unittest
from pathlib import Path

from eu4_analysis.pe import PEImage, keyword_reference_functions, read_binary

FILE_ALIGNMENT = 0x10
SECTION_ALIGNMENT = 0x10
IMAGE_BASE = 0x140000000


def _pad(blob: bytes, minimum: int, fill: bytes) -> bytes:
	"""Round a synthetic section up to file alignment without shrinking it."""
	size = max(len(blob), minimum)
	aligned = (size + FILE_ALIGNMENT - 1) & ~(FILE_ALIGNMENT - 1)
	return blob.ljust(aligned, fill)


TEXT_RVA = 0x1000
RDATA_RVA = 0x2000
PDATA_RVA = 0x3000


def lea_rdx(target_rva: int, insn_rva: int) -> bytes:
	"""`lea rdx, [rip + disp32]` resolving to target_rva from insn_rva."""
	disp: int = target_rva - (insn_rva + 7)
	return b"\x48\x8d\x15" + struct.pack("<i", disp)


def lea_r8(target_rva: int, insn_rva: int) -> bytes:
	"""`lea r8, [rip + disp32]`: REX.WR with reg=000."""
	disp: int = target_rva - (insn_rva + 7)
	return b"\x4c\x8d\x05" + struct.pack("<i", disp)


def build_pe(
	keywords: tuple[str, ...],
	*text_functions: tuple[int, bytes],
	extra_rdata: bytes = b"",
) -> bytes:
	"""A minimal x64 PE with .text, .rdata (keywords) and .pdata functions.

	Each text function is (begin RVA, code); .pdata records one
	RUNTIME_FUNCTION per function with a 0x20-byte end padding.
	"""
	text_blob: bytearray = bytearray()
	functions: list[tuple[int, int]] = []
	for begin, code in text_functions:
		offset: int = begin - TEXT_RVA
		text_blob.extend(b"\x90" * (offset - len(text_blob)))
		text_blob.extend(code)
		functions.append((begin, begin + len(code) + 0x10))

	rdata: bytearray = bytearray()
	keyword_rvas: dict[str, int] = {}
	for keyword in keywords:
		keyword_rvas[keyword] = RDATA_RVA + len(rdata)
		rdata.extend(keyword.encode("ascii") + b"\0")
	rdata.extend(extra_rdata)

	pdata: bytearray = bytearray()
	for begin, end in functions:
		pdata.extend(struct.pack("<III", begin, end, 0))
	pdata.extend(struct.pack("<III", 0, 0, 0))

	text_bytes = _pad(bytes(text_blob), 0x200, b"\x90")
	rdata_bytes = _pad(bytes(rdata), 0x100, b"\x00")
	pdata_bytes = _pad(bytes(pdata), 0x100, b"\x00")

	headers_size: int = 0x200
	text_offset: int = headers_size
	rdata_offset: int = text_offset + len(text_bytes)
	pdata_offset: int = rdata_offset + len(rdata_bytes)

	dos: bytearray = bytearray(0x40)
	dos[:2] = b"MZ"
	struct.pack_into("<I", dos, 0x3C, 0x80)

	pe_header: bytearray = bytearray()
	pe_header.extend(b"PE\0\0")
	pe_header.extend(struct.pack("<HHIIIHH", 0x8664, 3, 0, 0, 0, 0xF0, 0x22))
	optional: bytearray = bytearray(0xF0)
	struct.pack_into("<H", optional, 0, 0x20B)
	struct.pack_into("<Q", optional, 24, IMAGE_BASE)
	struct.pack_into("<I", optional, 60, SECTION_ALIGNMENT)  # SectionAlignment
	struct.pack_into("<I", optional, 64, FILE_ALIGNMENT)  # FileAlignment
	pe_header.extend(optional)

	def section(name: bytes, rva: int, size: int, offset: int) -> bytes:
		entry: bytearray = bytearray(40)
		entry[:8] = name.ljust(8, b"\0")
		struct.pack_into("<IIII", entry, 8, size, rva, size, offset)
		struct.pack_into(
			"<I", entry, 36, 0x60000020 if name == b".text" else 0x40000040
		)
		return bytes(entry)

	sections: bytearray = bytearray()
	# A token table is larger than the gap between the fixed RVAs. Keep the
	# small fixtures on those RVAs and slide only the later section down.
	pdata_rva = PDATA_RVA
	if TEXT_RVA + len(text_bytes) > RDATA_RVA:
		raise ValueError("synthetic .text does not fit before .rdata")
	if RDATA_RVA + len(rdata_bytes) > pdata_rva:
		pdata_rva = (RDATA_RVA + len(rdata_bytes) + SECTION_ALIGNMENT - 1) & ~(
			SECTION_ALIGNMENT - 1
		)
	sections.extend(section(b".text", TEXT_RVA, len(text_bytes), text_offset))
	sections.extend(section(b".rdata", RDATA_RVA, len(rdata_bytes), rdata_offset))
	sections.extend(section(b".pdata", pdata_rva, len(pdata_bytes), pdata_offset))

	image: bytearray = bytearray()
	image.extend(dos)
	image.extend(b"\x00" * (0x80 - len(image)))
	image.extend(pe_header)
	image.extend(sections)
	image.extend(b"\x00" * (headers_size - len(image)))
	assert len(image) == headers_size
	image.extend(text_bytes)
	image.extend(rdata_bytes)
	image.extend(pdata_bytes)
	return bytes(image)


def registry_function(
	keyword: str, keyword_rva: int, handler_rva: int
) -> tuple[int, bytes]:
	"""(begin, code) registering `keyword`: lea string, lea handler, call."""
	insn_rva: int = TEXT_RVA
	code: bytes = (
		lea_rdx(keyword_rva, insn_rva)
		+ lea_r8(handler_rva, insn_rva + 7)
		+ b"\x48\x31\xc9"  # xor rcx, rcx
		+ b"\xff\xd0"  # call rax
	)
	return insn_rva, code


class ReadPE(unittest.TestCase):
	def test_pdata_gives_exact_function_ranges(self) -> None:
		begin, code = registry_function("set_country_flag", RDATA_RVA, TEXT_RVA + 0x100)
		blob: bytes = build_pe(("set_country_flag",), (begin, code))
		with tempfile.TemporaryDirectory() as temporary:
			path: Path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertIsInstance(image, PEImage)
		self.assertEqual(image.function_starts, (IMAGE_BASE + TEXT_RVA,))
		self.assertEqual(
			image.function_end(IMAGE_BASE + TEXT_RVA),
			IMAGE_BASE + TEXT_RVA + len(code) + 0x10,
		)
		self.assertEqual(image.image_base, IMAGE_BASE)

	def test_non_pe_binaries_are_rejected(self) -> None:
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(b"\xcf\xfa\xed\xfe" + b"\x00" * 64)
			with self.assertRaises(ValueError):
				read_binary(path)


class TableHandlers(unittest.TestCase):
	def test_a_table_entry_pins_its_handler(self) -> None:
		"""The effect-table shape: {kind, name, 0, param, 0, 0, tooltip, fn}."""
		from eu4_analysis.pe import table_handlers

		# A data table entry pointing at the keyword string and the handler.
		entry = struct.pack(
			"<QQQQQQQQ",
			1,
			IMAGE_BASE + RDATA_RVA,
			0,
			IMAGE_BASE + RDATA_RVA + 0x40,
			0,
			0,
			IMAGE_BASE + RDATA_RVA + 0x80,
			IMAGE_BASE + TEXT_RVA + 0x100,
		)
		# layout: text(code at TEXT_RVA), rdata(keyword + two more strings +
		# the table at +0xC0), pdata. The handler must itself be a .pdata
		# function start, so the builder gets a second text function.
		code = b"\x90" * 0x110
		blob = build_pe(
			("set_country_flag",),
			(TEXT_RVA, code),
			(TEXT_RVA + 0x110, b"\x90" * 0x20),
			extra_rdata=entry.replace(
				struct.pack("<Q", IMAGE_BASE + TEXT_RVA + 0x100),
				struct.pack("<Q", IMAGE_BASE + TEXT_RVA + 0x110),
			),
		)
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertEqual(
			table_handlers(image, "set_country_flag"),
			(IMAGE_BASE + TEXT_RVA + 0x110,),
		)
		from eu4_analysis.pe import baseline_status

		self.assertEqual(baseline_status(image, "set_country_flag"), "pinned")

	def test_an_overlay_pointer_hit_cannot_break_the_batch(self) -> None:
		"""A trailing overlay qword is skipped; the window stays in bounds.

		The overlay names the keyword with its last eight bytes, so the
		-8..+0x38 neighborhood read used to run past the buffer and the
		struct.error aborted the whole batch. The .rdata table entry still
		pins; the overlay hit is skipped, not fatal.
		"""
		from eu4_analysis.pe import table_handlers

		entry = struct.pack(
			"<QQQQQQQQ",
			1,
			IMAGE_BASE + RDATA_RVA,
			0,
			IMAGE_BASE + RDATA_RVA + 0x40,
			0,
			0,
			IMAGE_BASE + RDATA_RVA + 0x80,
			IMAGE_BASE + TEXT_RVA + 0x110,
		)
		code = b"\x90" * 0x110
		blob = build_pe(
			("set_country_flag",),
			(TEXT_RVA, code),
			(TEXT_RVA + 0x110, b"\x90" * 0x20),
			extra_rdata=entry,
		)
		# The keyword's address qword as the very last eight bytes: the
		# window around this hit cannot be read in full.
		blob += struct.pack("<Q", IMAGE_BASE + RDATA_RVA)
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertEqual(
			table_handlers(image, "set_country_flag"),
			(IMAGE_BASE + TEXT_RVA + 0x110,),
		)

	def test_a_console_argument_hint_is_not_a_script_handler(self) -> None:
		"""``<Flag>`` at +0x40 marks a console command, not the script effect.

		EU4 1.37.5's command table uses the same handler slot as the
		synthetic effect entry, then points at an argument hint. Pinning
		that function would claim the console wrapper is the script effect.
		"""
		from eu4_analysis.pe import table_handlers

		hint = b"<Flag>\0"
		# Keywords occupy the front of .rdata; the entry follows them.
		name_at = 8  # second qword of the entry
		entry = struct.pack(
			"<QQQQQQQQ",
			1,
			IMAGE_BASE + RDATA_RVA,
			0,
			IMAGE_BASE + RDATA_RVA + 0x40,
			0,
			0,
			IMAGE_BASE + RDATA_RVA + 0x80,
			IMAGE_BASE + TEXT_RVA + 0x110,
		)
		keyword_bytes = len(b"set_country_flag\0")
		# Name sits at byte 8 of the entry; the hint qword is 0x40 past that,
		# which is 8 bytes after the 64-byte record.
		hint_rva = RDATA_RVA + keyword_bytes + len(entry) + 8 + 8
		blob = build_pe(
			("set_country_flag",),
			(TEXT_RVA, b"\x90" * 0x110),
			(TEXT_RVA + 0x110, b"\x90" * 0x20),
			extra_rdata=entry
			+ struct.pack("<Q", 1)
			+ struct.pack("<Q", IMAGE_BASE + hint_rva)
			+ hint,
		)
		self.assertEqual(name_at + 0x40, len(entry) + 8)
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertEqual(table_handlers(image, "set_country_flag"), ())
		from eu4_analysis.pe import baseline_status

		self.assertEqual(
			baseline_status(image, "set_country_flag"), "rejected_console_cmd"
		)

	def test_two_pointer_hits_naming_the_same_handler_still_pin(self) -> None:
		"""Two table entries naming the same handler agree; that is one."""
		from eu4_analysis.pe import table_handlers

		entry = struct.pack(
			"<QQQQQQQQ",
			1,
			IMAGE_BASE + RDATA_RVA,
			0,
			IMAGE_BASE + RDATA_RVA + 0x40,
			0,
			0,
			IMAGE_BASE + RDATA_RVA + 0x80,
			IMAGE_BASE + TEXT_RVA + 0x110,
		)
		second = struct.pack(
			"<QQQQQQQQ",
			1,
			IMAGE_BASE + RDATA_RVA,
			0,
			0,
			0,
			0,
			IMAGE_BASE + TEXT_RVA + 0x110,
			0,
		)
		code = b"\x90" * 0x110
		blob = build_pe(
			("set_country_flag",),
			(TEXT_RVA, code),
			(TEXT_RVA + 0x110, b"\x90" * 0x20),
			extra_rdata=entry + second,
		)
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertEqual(
			table_handlers(image, "set_country_flag"),
			(IMAGE_BASE + TEXT_RVA + 0x110,),
		)


class KeywordReferences(unittest.TestCase):
	def test_a_keyword_reference_finds_its_function(self) -> None:
		begin, code = registry_function("set_country_flag", RDATA_RVA, TEXT_RVA + 0x100)
		blob = build_pe(("set_country_flag", "other_keyword"), (begin, code))
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		functions = keyword_reference_functions(image, "set_country_flag")
		self.assertEqual(
			functions,
			((IMAGE_BASE + TEXT_RVA, IMAGE_BASE + TEXT_RVA + len(code) + 0x10),),
		)
		self.assertEqual(keyword_reference_functions(image, "absent"), ())

	def test_lea_forms_that_do_not_target_the_string_are_ignored(self) -> None:
		# A lea of some unrelated address (rdata + 0x80, inside padding).
		code: bytes = lea_rdx(RDATA_RVA + 0x80, TEXT_RVA) + b"\xff\xd0"
		blob = build_pe(("set_country_flag",), (TEXT_RVA, code))
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		self.assertEqual(keyword_reference_functions(image, "set_country_flag"), ())


if __name__ == "__main__":
	unittest.main()
