"""Windows PE64 metadata: sections, exact .pdata function ranges, and
keyword string references.

The macOS backend reads exact boundaries from LC_FUNCTION_STARTS; x64
Windows executables carry the same exactness in `.pdata`, where every
RUNTIME_FUNCTION publishes its function's begin and end RVA. Symbols are
absent on Windows, so handler discovery works from the other end: find the
keyword string in the data sections, find the `lea` instructions in `.text`
that reference it (RIP-relative), and read the registration call in the
containing function.
"""

from __future__ import annotations

import bisect
import hashlib
import struct
from dataclasses import dataclass
from pathlib import Path

IMAGE_DOS_SIGNATURE = 0x5A4D  # "MZ"
IMAGE_NT_SIGNATURE = 0x00004550  # "PE\0\0"
MACHINE_AMD64 = 0x8664
PE32_PLUS_MAGIC = 0x20B


@dataclass(frozen=True)
class Section:
	name: str
	"""Relative virtual address and size as loaded into memory."""
	rva: int
	virtual_size: int
	file_offset: int
	file_size: int

	def contains_rva(self, rva: int) -> bool:
		return self.rva <= rva < self.rva + max(self.virtual_size, self.file_size)

	def offset_of(self, rva: int) -> int:
		return self.file_offset + (rva - self.rva)


@dataclass(frozen=True)
class PEImage:
	path: Path
	sha256: str
	function_starts: tuple[int, ...]
	text_end: int
	image_base: int
	sections: tuple[Section, ...]
	data: bytes
	_ends: dict[int, int]
	address_bias: int = 0

	@property
	def platform(self) -> str:
		return "windows"

	@property
	def function_ends(self) -> tuple[int, ...]:
		return tuple(self.function_end(start) for start in self.function_starts)

	def function_end(self, start: int) -> int:
		end: int | None = self._ends.get(start)
		if end is not None:
			return end
		index: int = bisect.bisect_left(self.function_starts, start)
		if index == len(self.function_starts) or self.function_starts[index] != start:
			raise ValueError(f"{start:#x} is not a recorded function start")
		return (
			self.function_starts[index + 1]
			if index + 1 < len(self.function_starts)
			else self.text_end
		)

	def section_of(self, rva: int) -> Section | None:
		return next((section for section in self.sections if section.contains_rva(rva)), None)

	def bytes_of_rva(self, rva: int, size: int) -> bytes | None:
		section = self.section_of(rva)
		if section is None:
			return None
		offset = section.offset_of(rva)
		return self.data[offset : offset + size]


def _read_u32(data: bytes, offset: int) -> int:
	return struct.unpack_from("<I", data, offset)[0]


def _read_u64(data: bytes, offset: int) -> int:
	return struct.unpack_from("<Q", data, offset)[0]


def section_table(data: bytes, first_section_offset: int, count: int) -> tuple[Section, ...]:
	sections: list[Section] = []
	for index in range(count):
		offset: int = first_section_offset + index * 40
		if offset + 40 > len(data):
			raise ValueError("truncated section table")
		raw_name: bytes = data[offset : offset + 8]
		name: str = raw_name.split(b"\0", 1)[0].decode("ascii", "replace")
		virtual_size, rva, file_size, file_offset = struct.unpack_from("<IIII", data, offset + 8)
		sections.append(Section(name, rva, virtual_size, file_offset, file_size))
	return tuple(sections)


def read_binary(path: Path) -> PEImage:
	"""Exact x64 PE function boundaries from .pdata, like the Mach-O backend."""
	data: bytes = path.read_bytes()
	if len(data) < 0x40 or _read_u32(data, 0x3C) == 0:
		raise ValueError("truncated DOS header")
	pe_offset: int = _read_u32(data, 0x3C)
	if _read_u16(data, 0) != IMAGE_DOS_SIGNATURE or _read_u32(data, pe_offset) != IMAGE_NT_SIGNATURE:
		raise ValueError("not a PE executable")
	machine: int = _read_u16(data, pe_offset + 4)
	if machine != MACHINE_AMD64:
		raise ValueError("this backend supports the x86_64 PE EU4 executable")
	section_count: int = _read_u16(data, pe_offset + 6)
	optional_size: int = _read_u16(data, pe_offset + 20)
	optional_offset: int = pe_offset + 24
	if _read_u16(data, optional_offset) != PE32_PLUS_MAGIC:
		raise ValueError("expected a PE32+ optional header")
	if optional_size < 112:
		raise ValueError("truncated optional header")
	image_base: int = _read_u64(data, optional_offset + 24)
	sections = section_table(data, optional_offset + optional_size, section_count)
	text = next((s for s in sections if s.name == ".text"), None)
	pdata = next((s for s in sections if s.name == ".pdata"), None)
	if text is None or pdata is None:
		raise ValueError("executable lacks .text or .pdata")
	# RUNTIME_FUNCTION: BeginAddress, EndAddress, UnwindData (u32 RVAs).
	entries: dict[int, int] = {}
	starts: set[int] = set()
	for offset in range(
		pdata.file_offset, min(pdata.file_offset + pdata.file_size, len(data)) - 12 + 1, 12
	):
		begin, end, _ = struct.unpack_from("<III", data, offset)
		if begin == 0:
			break
		starts.add(begin)
		entries[begin] = max(entries.get(begin, 0), end)
	if not starts:
		raise ValueError("empty .pdata function table")
	ordered: tuple[int, ...] = tuple(sorted(starts))
	return PEImage(
		path.resolve(),
		hashlib.sha256(data).hexdigest(),
		ordered,
		text.rva + max(text.virtual_size, text.file_size),
		image_base,
		sections,
		data,
		entries,
	)


def _read_u16(data: bytes, offset: int) -> int:
	return struct.unpack_from("<H", data, offset)[0]


def keyword_rvas(image: PEImage, keyword: str) -> tuple[int, ...]:
	"""RVAs of the keyword as a NUL-terminated string in the data sections."""
	needle: bytes = keyword.encode("ascii") + b"\0"
	found: list[int] = []
	for section in image.sections:
		if section.name == ".text":
			continue
		haystack: bytes = image.data[section.file_offset : section.file_offset + section.file_size]
		position: int = haystack.find(needle)
		while position != -1:
			found.append(section.rva + position)
			position = haystack.find(needle, position + 1)
	return tuple(found)


def _lea_targets(image: PEImage, targets: frozenset[int]) -> tuple[int, ...]:
	"""VAs of RIP-relative `lea` instructions whose destination is in targets.

	Any REX-prefixed `lea` (`48 8D /r` with mod=00 rm=101) is 7 bytes: the
	displacement resolves exactly, so a coincidental byte match must point at
	a searched string to count.
	"""
	text = next((s for s in image.sections if s.name == ".text"), None)
	if text is None:
		return ()
	code: bytes = image.data[text.file_offset : text.file_offset + text.file_size]
	insns: list[int] = []
	for offset in range(len(code) - 7 + 1):
		if code[offset] & 0xF0 != 0x40 or not code[offset] & 0x08:
			continue
		if code[offset + 1] != 0x8D or code[offset + 2] & 0xC7 != 0x05:
			continue
		displacement: int = struct.unpack_from("<i", code, offset + 3)[0]
		target: int = text.rva + offset + 7 + displacement
		if target in targets:
			insns.append(text.rva + offset)
	return tuple(insns)


def keyword_reference_functions(image: PEImage, keyword: str) -> tuple[tuple[int, int], ...]:
	"""Functions whose code references the keyword string, begin/end pairs.

	For a stripped executable this replaces symbol lookup: the containing
	.pdata function of every referencing `lea` is a registration candidate.
	Ranges that leave the .text section (linker-split and cold functions) are
	dropped: bounded inspection requires one executable block.
	"""
	targets = frozenset(keyword_rvas(image, keyword))
	if not targets:
		return ()
	text = next((s for s in image.sections if s.name == ".text"), None)
	if text is None:
		return ()
	text_end = text.rva + max(text.virtual_size, text.file_size)
	functions: list[tuple[int, int]] = []
	for insn in _lea_targets(image, targets):
		address = insn + image.address_bias
		index: int = bisect.bisect_right(image.function_starts, address) - 1
		if index < 0:
			continue
		begin: int = image.function_starts[index]
		end = image.function_end(begin)
		if (
			begin >= text.rva + image.address_bias
			and address < end <= text_end + image.address_bias
		):
			functions.append((begin, end))
	return tuple(dict.fromkeys(functions))


_DOS_MAGIC = b"MZ"
_PE_SIGNATURE = b"PE\0\0"
_X86_64 = 0x8664
_PE32_PLUS = 0x20B
_PE32_PLUS_FIXED = 112
_EXCEPTION_DIRECTORY = 3
_DIRECTORY_COUNT_MIN = _EXCEPTION_DIRECTORY + 1
_SECTION_SIZE = 40
_RUNTIME_FUNCTION_SIZE = 12


def read_pe_image(path: Path) -> PEImage:
	"""Use PE exception-directory function entries rather than guessing ends.

	The RUNTIME_FUNCTION table the compiler emits for x64 unwinding names the
	exact begin and end of every function with unwind information, the PE
	counterpart of the Mach-O function-start table.
	"""
	data: bytes = path.read_bytes()
	if len(data) < 0x40 or data[:2] != _DOS_MAGIC:
		raise ValueError("not a PE executable")
	pe_offset: int = struct.unpack_from("<I", data, 0x3C)[0]
	if pe_offset + 24 > len(data) or data[pe_offset : pe_offset + 4] != _PE_SIGNATURE:
		raise ValueError("executable lacks a PE signature")
	machine: int = struct.unpack_from("<H", data, pe_offset + 4)[0]
	if machine != _X86_64:
		raise ValueError("this backend supports the x86_64 PE EU4 executable")
	section_count: int = struct.unpack_from("<H", data, pe_offset + 6)[0]
	optional_size: int = struct.unpack_from("<H", data, pe_offset + 20)[0]
	optional: int = pe_offset + 24
	if (
		optional_size < _PE32_PLUS_FIXED + _DIRECTORY_COUNT_MIN * 8
		or optional + optional_size > len(data)
	):
		raise ValueError("truncated optional header")
	if struct.unpack_from("<H", data, optional)[0] != _PE32_PLUS:
		raise ValueError("this backend supports the PE32+ EU4 executable")
	image_base: int = struct.unpack_from("<Q", data, optional + 24)[0]
	if (
		section_count == 0
		or optional + optional_size + section_count * _SECTION_SIZE > len(data)
	):
		raise ValueError("truncated section table")

	# name, virtual size, virtual address, raw size, raw pointer
	sections: list[tuple[bytes, int, int, int, int]] = []
	for index in range(section_count):
		offset: int = optional + optional_size + index * _SECTION_SIZE
		name: bytes = data[offset : offset + 8].rstrip(b"\0")
		virtual_size, virtual_address, raw_size, raw_pointer = struct.unpack_from(
			"<IIII", data, offset + 8
		)
		sections.append((name, virtual_size, virtual_address, raw_size, raw_pointer))

	def file_offset(rva: int, size: int) -> int:
		for _, virtual_size, virtual_address, raw_size, raw_pointer in sections:
			if virtual_address <= rva and rva + size <= virtual_address + max(
				virtual_size, raw_size
			):
				return raw_pointer + (rva - virtual_address)
		raise ValueError("directory lies outside every section")

	if not any(entry[0] == b".text" for entry in sections):
		raise ValueError("executable lacks a .text section")
	exception_rva, exception_size = struct.unpack_from(
		"<II", data, optional + _PE32_PLUS_FIXED + _EXCEPTION_DIRECTORY * 8
	)
	if exception_size == 0 or exception_size % _RUNTIME_FUNCTION_SIZE != 0:
		raise ValueError("executable lacks well-formed function entries")
	table: int = file_offset(exception_rva, exception_size)
	if table + exception_size > len(data):
		raise ValueError("function table extends beyond executable")
	starts: list[int] = []
	ends: list[int] = []
	for offset in range(table, table + exception_size, _RUNTIME_FUNCTION_SIZE):
		begin, end, _ = struct.unpack_from("<III", data, offset)
		if begin == 0 or end <= begin:
			raise ValueError("invalid function entry")
		starts.append(image_base + begin)
		ends.append(image_base + end)
	if not starts or starts != sorted(set(starts)):
		raise ValueError("function entries are not sorted by address")
	text: tuple[bytes, int, int, int, int] = next(
		entry for entry in sections if entry[0] == b".text"
	)
	return PEImage(
		path.resolve(),
		hashlib.sha256(data).hexdigest(),
		tuple(starts),
		image_base + text[2] + text[1],
		image_base,
		tuple(
			Section(name.decode("ascii", "replace"), rva, virtual_size, raw_pointer, raw_size)
			for name, virtual_size, rva, raw_size, raw_pointer in sections
		),
		data,
		dict(zip(starts, ends)),
		image_base,
	)
