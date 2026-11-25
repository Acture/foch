"""16-bit Paradox token table discovered from a PE image.

The table is a fixed-stride run of records in a non-code section. Each
record stores a u16-range id in the bytes immediately before an identifier
C string. Nothing here hardcodes a file offset: the stride, the section and
the id width are the ones that best match that shape. A separate calibration
check against known token ids decides whether the discovered table is the
Paradox token table. A token id by itself is not a handler pin.
"""

from __future__ import annotations

import bisect
import re
import struct
from collections import Counter
from collections.abc import Mapping
from dataclasses import dataclass
from itertools import pairwise

from .pe import PEImage

# Cross-repo ids for this EU4 build. They calibrate a discovered table; they
# are not used as a search key and they are not file offsets.
GOLD_TOKENS: dict[str, int] = {
	"outgoing": 0x2AA8,
	"members": 0x2AA9,
	"army": 0x28A0,
	"regiment": 0x28A2,
	"strength": 0x28A9,
	"morale": 0x28A8,
}

# Steam Windows x64 eu4.exe this experiment reads. A mismatch is reported;
# it does not by itself move a primitive to pinned.
PINNED_EU4_SHA256 = "c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e"

_MIN_STRIDE = 0x10
_MAX_STRIDE = 0x400
_MIN_RECORDS = 8
_MIN_ID_FRACTION = 0.9
_MAX_NAME = 64
_DEBUG = re.compile(rb"C([A-Z][A-Za-z0-9]*)(Effect|Trigger)::(Execute|Evaluate)\b")
_SNAKE = re.compile(r"(?<!^)(?=[A-Z])")


@dataclass(frozen=True)
class TokenTable:
	"""One discovered id-and-name record run."""

	section: str
	stride: int
	id_width: int
	entries: tuple[tuple[str, int, int], ...]
	"""``(name, token id, name RVA)`` in file order."""

	def ids_of(self, name: str) -> tuple[int, ...]:
		return tuple(token for item, token, _rva in self.entries if item == name)

	def id_of(self, name: str) -> int | None:
		found = self.ids_of(name)
		if len(found) != 1:
			return None
		return found[0]

	@property
	def first_rva(self) -> int | None:
		if not self.entries:
			return None
		return self.entries[0][2] - self.id_width

	@property
	def last_rva(self) -> int | None:
		if not self.entries:
			return None
		return self.entries[-1][2] - self.id_width


@dataclass(frozen=True)
class Calibration:
	passed: bool
	matched: int
	total: int
	checks: tuple[tuple[str, int, int | None], ...]
	"""``(name, expected id, found id or None)``."""


@dataclass(frozen=True)
class DebugHandler:
	"""A function that loads a class debug string naming Execute or Evaluate."""

	name: str
	kind: str
	address: int
	end: int
	string: str
	string_rva: int
	lea_rva: int
	prologue: str
	prologue_hex: str


def _ident_byte(value: int) -> bool:
	return value == 95 or 48 <= value <= 57 or 65 <= value <= 90 or 97 <= value <= 122


def _ident_start(value: int) -> bool:
	return value == 95 or 65 <= value <= 90 or 97 <= value <= 122


def _identifier_offsets(data: bytes, start: int, size: int) -> list[int]:
	blob = data[start : start + size]
	found: list[int] = []
	index = 0
	limit = len(blob)
	while index < limit:
		if not _ident_start(blob[index]):
			index += 1
			continue
		if index and _ident_byte(blob[index - 1]):
			index += 1
			continue
		end = index + 1
		while end < limit and _ident_byte(blob[end]):
			end += 1
		if end < limit and blob[end] == 0 and end - index <= _MAX_NAME:
			found.append(start + index)
			index = end + 1
		else:
			index += 1
	return found


def _read_id(data: bytes, name_offset: int, width: int) -> int | None:
	field = name_offset - width
	if field < 0 or name_offset > len(data):
		return None
	if width == 2:
		value = struct.unpack_from("<H", data, field)[0]
	else:
		value = struct.unpack_from("<I", data, field)[0]
	if 0 < value < 0x10000:
		return value
	return None


def _candidate(
	data: bytes,
	section_name: str,
	section_rva: int,
	section_off: int,
	offsets: list[int],
	stride: int,
	width: int,
) -> TokenTable | None:
	residues = Counter(offset % stride for offset in offsets)
	if not residues:
		return None
	residue = residues.most_common(1)[0][0]
	aligned = [offset for offset in offsets if offset % stride == residue]
	if len(aligned) < _MIN_RECORDS:
		return None
	entries: list[tuple[str, int, int]] = []
	plausible = 0
	for offset in aligned:
		token = _read_id(data, offset, width)
		if token is None:
			continue
		plausible += 1
		end = data.find(b"\0", offset, offset + _MAX_NAME + 1)
		name = data[offset:end].decode("ascii")
		rva = section_rva + (offset - section_off)
		entries.append((name, token, rva))
	if plausible / len(aligned) < _MIN_ID_FRACTION:
		return None
	table = TokenTable(section_name, stride, width, tuple(entries))
	if not _ids_follow_nearby_slots(table):
		return None
	return table


def _ids_follow_nearby_slots(table: TokenTable) -> bool:
	"""Nearby records mostly carry ids that differ by the slot distance.

	A pointer-sized gap of unrelated strings can look aligned. The Paradox
	table is dense enough that records a few slots apart keep that distance
	in the stored id. This does not require every id to be the slot index.
	"""
	checked = 0
	matched = 0
	for (_name, left_id, left_rva), (_next, right_id, right_rva) in pairwise(
		table.entries
	):
		delta = right_rva - left_rva
		if delta % table.stride:
			continue
		steps = delta // table.stride
		if not 1 <= steps <= 8:
			continue
		checked += 1
		if right_id - left_id == steps:
			matched += 1
	if checked < _MIN_RECORDS:
		return False
	return matched / checked >= 0.5


def discover_token_table(image: PEImage) -> TokenTable | None:
	"""The strongest fixed-stride id-and-name table in a non-code section.

	Returns ``None`` when no section has a run that matches the record
	shape. Callers still have to calibrate before trusting an id.
	"""
	best: TokenTable | None = None
	for section in image.sections:
		if section.name == ".text" or section.file_size < _MIN_STRIDE:
			continue
		offsets = _identifier_offsets(
			image.data, section.file_offset, section.file_size
		)
		if len(offsets) < _MIN_RECORDS:
			continue
		gaps: Counter[int] = Counter()
		for previous, current in pairwise(offsets):
			delta = current - previous
			if _MIN_STRIDE <= delta <= _MAX_STRIDE:
				gaps[delta] += 1
		if not gaps:
			continue
		for stride, _count in gaps.most_common(3):
			if gaps[stride] < _MIN_RECORDS:
				continue
			for width in (2, 4):
				found = _candidate(
					image.data,
					section.name,
					section.rva,
					section.file_offset,
					offsets,
					stride,
					width,
				)
				if found is None:
					continue
				if best is None or len(found.entries) > len(best.entries):
					best = found
	return best


def calibrate(
	table: TokenTable | None, gold: Mapping[str, int] = GOLD_TOKENS
) -> Calibration:
	"""Whether every gold name is stored under its known token id."""
	checks: list[tuple[str, int, int | None]] = []
	matched = 0
	for name, expected in gold.items():
		found = None if table is None else table.id_of(name)
		if found == expected:
			matched += 1
		checks.append((name, expected, found))
	total = len(gold)
	return Calibration(matched == total and total > 0, matched, total, tuple(checks))


def _snake(pascal: str) -> str:
	return _SNAKE.sub("_", pascal).lower()


def _containing(image: PEImage, address: int) -> tuple[int, int] | None:
	starts = image.function_starts
	index = bisect.bisect_right(starts, address) - 1
	if index < 0:
		return None
	begin = starts[index]
	end = image.function_end(begin)
	if begin <= address < end:
		return begin, end
	return None


def _leas_to(code: bytes, text_rva: int, targets: set[int]) -> dict[int, list[int]]:
	found: dict[int, list[int]] = {target: [] for target in targets}
	start = 1
	while True:
		hit = code.find(b"\x8d", start)
		if hit < 0 or hit + 5 >= len(code):
			break
		start = hit + 1
		rex = code[hit - 1]
		if rex & 0xF0 != 0x40 or not rex & 0x08:
			continue
		if code[hit + 1] & 0xC7 != 0x05:
			continue
		displacement = struct.unpack_from("<i", code, hit + 2)[0]
		destination = text_rva + (hit - 1) + 7 + displacement
		bucket = found.get(destination)
		if bucket is not None:
			bucket.append(text_rva + hit - 1)
	return found


def _decode(code: bytes, offset: int) -> tuple[str, int] | None:
	if offset >= len(code):
		return None
	byte = code[offset]
	pushes = {
		0x50: "rax",
		0x51: "rcx",
		0x52: "rdx",
		0x53: "rbx",
		0x54: "rsp",
		0x55: "rbp",
		0x56: "rsi",
		0x57: "rdi",
	}
	if byte in pushes:
		return f"push {pushes[byte]}", 1
	if byte == 0x90:
		return "nop", 1
	if byte == 0xC3:
		return "ret", 1
	if byte == 0xCC:
		return "int3", 1
	if byte == 0x41 and offset + 1 < len(code) and 0x50 <= code[offset + 1] <= 0x57:
		name = ("r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15")[
			code[offset + 1] - 0x50
		]
		return f"push {name}", 2
	if offset + 4 < len(code) and code[offset] == 0x48 and code[offset + 1] == 0x89:
		modrm = code[offset + 2]
		if modrm in (0x5C, 0x6C, 0x74, 0x7C) and code[offset + 3] == 0x24:
			reg = {0x5C: "rbx", 0x6C: "rbp", 0x74: "rsi", 0x7C: "rdi"}[modrm]
			disp = struct.unpack_from("<b", code, offset + 4)[0]
			return f"mov qword ptr [rsp{disp:+#x}], {reg}", 5
	if (
		offset + 6 < len(code)
		and code[offset] == 0x48
		and code[offset + 1] == 0x8D
		and code[offset + 2] == 0xAC
		and code[offset + 3] == 0x24
	):
		disp = struct.unpack_from("<i", code, offset + 4)[0]
		return f"lea rbp, [rsp{disp:+#x}]", 8
	if offset + 6 < len(code) and code[offset : offset + 3] == b"\x48\x81\xec":
		imm = struct.unpack_from("<I", code, offset + 3)[0]
		return f"sub rsp, {imm:#x}", 7
	if offset + 3 < len(code) and code[offset : offset + 3] == b"\x48\x83\xec":
		return f"sub rsp, {code[offset + 3]:#x}", 4
	if offset + 2 < len(code) and code[offset] == 0x48 and code[offset + 1] == 0x8B:
		moves = {
			0xF8: "mov rdi, rax",
			0xF9: "mov rdi, rcx",
			0xFA: "mov rdi, rdx",
			0xF1: "mov rsi, rcx",
			0xF2: "mov rsi, rdx",
			0xD1: "mov rdx, rcx",
			0xC1: "mov rax, rcx",
			0xC2: "mov rax, rdx",
			0xCE: "mov rcx, rsi",
			0xCF: "mov rcx, rdi",
		}
		text = moves.get(code[offset + 2])
		if text is not None:
			return text, 3
	if (
		offset + 6 < len(code)
		and code[offset] in (0x48, 0x4C)
		and code[offset + 1] == 0x8D
	):
		modrm = code[offset + 2]
		if modrm & 0xC7 == 0x05:
			reg = (modrm >> 3) & 7
			name = ("rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi")[reg]
			if code[offset] == 0x4C:
				name = ("r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15")[reg]
			disp = struct.unpack_from("<i", code, offset + 3)[0]
			return f"lea {name}, [rip{disp:+#x}]", 7
	return None


def summarize_prologue(code: bytes, limit: int = 8) -> str:
	"""A short decoded prefix, stopping at the first unrecognized byte."""
	parts: list[str] = []
	offset = 0
	while offset < len(code) and len(parts) < limit:
		decoded = _decode(code, offset)
		if decoded is None:
			parts.append(f".byte {code[offset]:#04x}")
			break
		text, size = decoded
		parts.append(text)
		offset += size
	return "; ".join(parts)


def debug_handlers(image: PEImage) -> tuple[dict[str, DebugHandler], frozenset[str]]:
	"""Handlers named by a unique ``CNameEffect::Execute`` or trigger string.

	The second result is the set of primitive names whose debug strings
	resolved to more than one function. Those stay unpinned.
	"""
	text = next(
		(section for section in image.sections if section.name == ".text"), None
	)
	if text is None:
		return {}, frozenset()
	strings: list[tuple[int, str, str, str]] = []
	for match in _DEBUG.finditer(image.data):
		if match.start() and image.data[match.start() - 1] != 0:
			continue
		section = next(
			(
				item
				for item in image.sections
				if item.file_offset <= match.start() < item.file_offset + item.file_size
			),
			None,
		)
		if section is None or section.name == ".text":
			continue
		kind_word = match.group(2).decode()
		method = match.group(3).decode()
		if (kind_word, method) not in {("Effect", "Execute"), ("Trigger", "Evaluate")}:
			continue
		end = image.data.find(b"\0", match.start(), match.start() + 160)
		if end <= match.start():
			continue
		text_value = image.data[match.start() : end].decode("ascii", "replace")
		rva = section.rva + (match.start() - section.file_offset)
		kind = "effect" if kind_word == "Effect" else "trigger"
		strings.append((rva, _snake(match.group(1).decode()), kind, text_value))
	if not strings:
		return {}, frozenset()
	code = image.data[text.file_offset : text.file_offset + text.file_size]
	leas = _leas_to(code, text.rva, {rva for rva, *_rest in strings})
	by_name: dict[str, dict[int, DebugHandler]] = {}
	for rva, name, kind, text_value in strings:
		for lea_rva in leas.get(rva, ()):
			bounds = _containing(image, image.image_base + lea_rva)
			if bounds is None:
				continue
			begin, end = bounds
			blob = (
				image.bytes_of_rva(begin - image.image_base, min(32, end - begin))
				or b""
			)
			handler = DebugHandler(
				name,
				kind,
				begin,
				end,
				text_value,
				rva,
				lea_rva,
				summarize_prologue(blob),
				blob.hex(),
			)
			by_name.setdefault(name, {})[begin] = handler
	resolved: dict[str, DebugHandler] = {}
	ambiguous: set[str] = set()
	for name, handlers in by_name.items():
		if len(handlers) == 1:
			resolved[name] = next(iter(handlers.values()))
		else:
			ambiguous.add(name)
	return resolved, frozenset(ambiguous)
