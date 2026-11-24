"""Token-id dispatch discovered from a PE image, with no hardcoded offsets.

A catalog primitive is promoted only when one of three shapes resolves a
``.pdata`` function for its calibrated token id:

* a RIP-relative ``sub reg, imm`` / ``cmp`` / ``ja`` jump table whose slot
  is that id and whose target is a function start outside the dispatcher
* a repeated ``(token id, function pointer)`` record whose function is not
  shared with another token in the run
* a vtable slot whose distance from a ``mov eax, token; ret`` thunk was
  learned from two or more debug-string Execute/Evaluate functions

Anything else stays unproven. A register-relative table whose base is not a
RIP-relative LEA is not read. A shared target, a slot that is not a function
start, or two mechanisms that name different functions is a refusal.
"""

from __future__ import annotations

import bisect
import struct
from collections import defaultdict
from dataclasses import dataclass

from .pe import PEImage
from .tokens import DebugHandler, TokenTable, debug_handlers

_MAX_SPAN = 8192
_FACTORY_MIN = 8
_LOW32 = ("eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi")
_HIGH32 = ("r8d", "r9d", "r10d", "r11d", "r12d", "r13d", "r14d", "r15d")
_LOW64 = ("rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi")
_R64 = {
	"eax": "rax",
	"ecx": "rcx",
	"edx": "rdx",
	"ebx": "rbx",
	"esp": "rsp",
	"ebp": "rbp",
	"esi": "rsi",
	"edi": "rdi",
	"r8d": "r8",
	"r9d": "r9",
	"r10d": "r10",
	"r11d": "r11",
	"r12d": "r12",
	"r13d": "r13",
	"r14d": "r14",
	"r15d": "r15",
}


@dataclass(frozen=True)
class DispatchHit:
	"""One token id resolved to one function."""

	status: str
	evidence: str
	address: int
	end: int


def discover_dispatch(
	image: PEImage, table: TokenTable
) -> tuple[dict[int, DispatchHit], dict[int, str]]:
	"""Proven token handlers, and token ids that were seen and refused.

	A token present in the refusal map is absent from the hit map. Debug-string
	pins are not consulted here; the caller keeps those.
	"""
	token_ids = {token for _name, token, _rva in table.entries}
	proofs: dict[int, list[DispatchHit]] = defaultdict(list)
	refusals: dict[int, set[str]] = defaultdict(set)
	_jump_tables(image, token_ids, proofs, refusals)
	_factory_pairs(image, table, token_ids, proofs, refusals)
	_vtable_slots(image, table, token_ids, proofs, refusals)
	hits: dict[int, DispatchHit] = {}
	failed: dict[int, str] = {}
	for token in set(proofs) | set(refusals):
		if refusals.get(token):
			failed[token] = min(refusals[token])
			continue
		found = proofs[token]
		addresses = {hit.address for hit in found}
		if len(addresses) != 1:
			failed[token] = "dispatcher_ambiguous"
			continue
		pinned = [hit for hit in found if hit.status == "pinned"]
		hits[token] = pinned[0] if pinned else found[0]
	return hits, failed


def _text(image: PEImage):
	return next(section for section in image.sections if section.name == ".text")


def _code(image: PEImage) -> tuple[bytes, int]:
	section = _text(image)
	return image.data[
		section.file_offset : section.file_offset + section.file_size
	], section.rva


def _function_of(image: PEImage, va: int) -> tuple[int, int] | None:
	index = bisect.bisect_right(image.function_starts, va) - 1
	if index < 0:
		return None
	begin = image.function_starts[index]
	end = image.function_end(begin)
	if begin <= va < end:
		return begin, end
	return None


def _hit(
	image: PEImage, starts: set[int], status: str, evidence: str, address: int
) -> DispatchHit | None:
	if address not in starts:
		return None
	return DispatchHit(status, evidence, address, image.function_end(address))


def _sub_immediate(code: bytes, offset: int) -> int | None:
	"""The immediate of a ``sub r32, imm32`` at ``offset``, if that is the opcode."""
	if offset >= len(code):
		return None
	byte = code[offset]
	if byte == 0x2D and offset + 5 <= len(code):
		return struct.unpack_from("<I", code, offset + 1)[0]
	if (
		byte == 0x81
		and offset + 6 <= len(code)
		and code[offset + 1] in range(0xE8, 0xF0)
		and (offset == 0 or code[offset - 1] not in (0x40, 0x41, 0x44, 0x45))
	):
		return struct.unpack_from("<I", code, offset + 2)[0]
	if (
		byte == 0x41
		and offset + 7 <= len(code)
		and code[offset + 1] == 0x81
		and code[offset + 2] in range(0xE8, 0xF0)
	):
		return struct.unpack_from("<I", code, offset + 3)[0]
	return None


def _decode(
	code: bytes, offset: int, rva: int
) -> tuple[str, dict[str, int | str], int] | None:
	if offset >= len(code):
		return None
	byte = code[offset]
	if byte == 0x90:
		return "nop", {}, 1
	if byte == 0x2D and offset + 5 <= len(code):
		imm = struct.unpack_from("<I", code, offset + 1)[0]
		return "sub", {"reg": "eax", "imm": imm}, 5
	if byte == 0x3D and offset + 5 <= len(code):
		imm = struct.unpack_from("<I", code, offset + 1)[0]
		return "cmp", {"reg": "eax", "imm": imm}, 5
	if byte == 0x77 and offset + 2 <= len(code):
		return "ja", {}, 2
	if byte == 0x0F and offset + 6 <= len(code) and code[offset + 1] == 0x87:
		return "ja", {}, 6
	if (
		offset + 6 <= len(code)
		and byte == 0x81
		and code[offset + 1] in range(0xE8, 0xF0)
	):
		reg = _LOW32[code[offset + 1] - 0xE8]
		imm = struct.unpack_from("<I", code, offset + 2)[0]
		return "sub", {"reg": reg, "imm": imm}, 6
	if (
		offset + 7 <= len(code)
		and byte == 0x41
		and code[offset + 1] == 0x81
		and code[offset + 2] in range(0xE8, 0xF0)
	):
		reg = _HIGH32[code[offset + 2] - 0xE8]
		imm = struct.unpack_from("<I", code, offset + 3)[0]
		return "sub", {"reg": reg, "imm": imm}, 7
	if (
		byte == 0x83
		and offset + 3 <= len(code)
		and code[offset + 1] in range(0xF8, 0x100)
	):
		reg = _LOW32[code[offset + 1] - 0xF8]
		return "cmp", {"reg": reg, "imm": code[offset + 2]}, 3
	if (
		byte == 0x81
		and offset + 6 <= len(code)
		and code[offset + 1] in range(0xF8, 0x100)
	):
		reg = _LOW32[code[offset + 1] - 0xF8]
		imm = struct.unpack_from("<I", code, offset + 2)[0]
		return "cmp", {"reg": reg, "imm": imm}, 6
	if (
		offset + 3 <= len(code)
		and byte == 0x48
		and code[offset + 1] == 0x63
		and code[offset + 2] >> 6 == 3
	):
		modrm = code[offset + 2]
		return (
			"movsxd",
			{"dst": _LOW64[(modrm >> 3) & 7], "src": _LOW32[modrm & 7]},
			3,
		)
	if (
		offset + 7 <= len(code)
		and byte == 0x48
		and code[offset + 1] == 0x8D
		and code[offset + 2] & 0xC7 == 0x05
	):
		reg = _LOW64[(code[offset + 2] >> 3) & 7]
		disp = struct.unpack_from("<i", code, offset + 3)[0]
		return "lea", {"dst": reg, "target": rva + 7 + disp}, 7
	if byte == 0x8B and offset + 3 <= len(code):
		modrm = code[offset + 1]
		if modrm >> 6 == 0 and modrm & 7 == 4:
			sib = code[offset + 2]
			scale = 1 << (sib >> 6)
			index = (sib >> 3) & 7
			base = sib & 7
			if index != 4 and base != 5:
				return (
					"load",
					{
						"dst": _LOW32[(modrm >> 3) & 7],
						"base": _LOW64[base],
						"index": _LOW64[index],
						"scale": scale,
					},
					3,
				)
	if (
		offset + 3 <= len(code)
		and byte == 0x48
		and code[offset + 1] == 0x03
		and code[offset + 2] >> 6 == 3
	):
		modrm = code[offset + 2]
		return (
			"add",
			{"dst": _LOW64[(modrm >> 3) & 7], "src": _LOW64[modrm & 7]},
			3,
		)
	if byte == 0xFF and offset + 2 <= len(code):
		modrm = code[offset + 1]
		if modrm >> 6 == 3 and (modrm >> 3) & 7 == 4:
			return "jmp", {"reg": _LOW64[modrm & 7]}, 2
	return None


def _jump_shape(code: bytes, start: int, rva: int) -> dict[str, int | str] | None:
	"""A RIP-relative dword jump table, or None when the bytes are not that shape."""
	kinds: list[str] = []
	info: dict[str, dict[str, int | str]] = {}
	offset = start
	rva_at = rva
	for _ in range(12):
		decoded = _decode(code, offset, rva_at)
		if decoded is None:
			return None
		kind, fields, size = decoded
		offset += size
		rva_at += size
		if kind == "nop":
			continue
		if not kinds and kind != "sub":
			return None
		kinds.append(kind)
		info[kind] = fields
		if kind == "jmp":
			break
	accepted = (
		["sub", "cmp", "ja", "movsxd", "lea", "load", "add", "jmp"],
		["sub", "cmp", "ja", "lea", "movsxd", "load", "add", "jmp"],
	)
	if kinds not in accepted:
		return None
	sub = info["sub"]
	cmp = info["cmp"]
	movsxd = info["movsxd"]
	lea = info["lea"]
	load = info["load"]
	add = info["add"]
	jmp = info["jmp"]
	if cmp["reg"] != sub["reg"] or movsxd["src"] != sub["reg"]:
		return None
	if (
		load["scale"] != 4
		or load["base"] != lea["dst"]
		or load["index"] != movsxd["dst"]
	):
		return None
	if add["src"] != lea["dst"] or add["dst"] != _R64[str(load["dst"])]:
		return None
	if jmp["reg"] != add["dst"]:
		return None
	limit = int(cmp["imm"])
	count = limit + 1
	if not 1 <= count <= _MAX_SPAN:
		return None
	return {
		"base": int(sub["imm"]),
		"count": count,
		"table": int(lea["target"]),
		"end": rva_at,
	}


def _jump_tables(
	image: PEImage,
	token_ids: set[int],
	proofs: dict[int, list[DispatchHit]],
	refusals: dict[int, set[str]],
) -> None:
	code, text_rva = _code(image)
	starts = set(image.function_starts)
	seen: set[int] = set()
	needles = [b"\x2d"]
	needles.extend(bytes((0x81, 0xE8 + reg)) for reg in range(8))
	needles.extend(bytes((0x41, 0x81, 0xE8 + reg)) for reg in range(8))
	for needle in needles:
		position = 0
		while True:
			offset = code.find(needle, position)
			if offset < 0:
				break
			position = offset + 1
			immediate = _sub_immediate(code, offset)
			if immediate is None or immediate not in token_ids:
				continue
			rva = text_rva + offset
			if rva in seen:
				continue
			shape = _jump_shape(code, offset, rva)
			if shape is None:
				continue
			seen.add(rva)
			bounds = _function_of(image, image.image_base + rva)
			if bounds is None or image.image_base + int(shape["end"]) > bounds[1]:
				continue
			base = int(shape["base"])
			if base not in token_ids:
				continue
			count = int(shape["count"])
			table_rva = int(shape["table"])
			slots: list[int] = []
			verified = True
			for index in range(count):
				raw = image.bytes_of_rva(table_rva + index * 4, 4)
				if raw is None or len(raw) < 4:
					verified = False
					break
				target = image.image_base + table_rva + struct.unpack("<i", raw)[0]
				if target not in starts or bounds[0] <= target < bounds[1]:
					verified = False
					break
				slots.append(target)
			covered = [
				base + index for index in range(count) if base + index in token_ids
			]
			if not verified:
				for token in covered:
					refusals[token].add("dispatcher_unverified")
				continue
			owners: dict[int, list[int]] = defaultdict(list)
			for index, target in enumerate(slots):
				token = base + index
				if token in token_ids:
					owners[target].append(token)
			for target, tokens in owners.items():
				if len(tokens) != 1:
					for token in tokens:
						refusals[token].add("dispatcher_ambiguous")
					continue
				hit = _hit(image, starts, "pinned", "token_jump_table", target)
				if hit is None:
					refusals[tokens[0]].add("dispatcher_unverified")
				else:
					proofs[tokens[0]].append(hit)


def _factory_pairs(
	image: PEImage,
	table: TokenTable,
	token_ids: set[int],
	proofs: dict[int, list[DispatchHit]],
	refusals: dict[int, set[str]],
) -> None:
	starts = set(image.function_starts)
	text = _text(image)
	text_lo = image.image_base + text.rva
	text_hi = text_lo + text.virtual_size
	skip_lo = table.first_rva or 0
	skip_hi = (table.last_rva or 0) + table.stride
	deltas = (8, 16, 24, 32)
	for section in image.sections:
		if section.name in {".text", ".pdata", ".reloc", ".rsrc"}:
			continue
		blob = image.data[section.file_offset : section.file_offset + section.file_size]
		grouped: dict[int, list[tuple[int, int, int]]] = defaultdict(list)
		for offset in range(0, len(blob) - 4, 4):
			token = struct.unpack_from("<I", blob, offset)[0]
			if token not in token_ids:
				continue
			rva = section.rva + offset
			if skip_lo <= rva < skip_hi:
				continue
			for delta in deltas:
				ptr_at = offset + delta
				if ptr_at < 0 or ptr_at + 8 > len(blob) or ptr_at % 8:
					continue
				value = struct.unpack_from("<Q", blob, ptr_at)[0]
				if text_lo <= value < text_hi and value in starts:
					grouped[delta].append((rva, token, value))
		for rows in grouped.values():
			rows.sort()
			_factory_runs(image, starts, rows, proofs, refusals)


def _factory_runs(
	image: PEImage,
	starts: set[int],
	rows: list[tuple[int, int, int]],
	proofs: dict[int, list[DispatchHit]],
	refusals: dict[int, set[str]],
) -> None:
	run: list[tuple[int, int, int]] = []

	def finish() -> None:
		if len(run) < _FACTORY_MIN:
			return
		by_token: dict[int, set[int]] = defaultdict(set)
		by_fn: dict[int, set[int]] = defaultdict(set)
		for _rva, token, fn in run:
			by_token[token].add(fn)
			by_fn[fn].add(token)
		for token, fns in by_token.items():
			if len(fns) != 1 or len(by_fn[next(iter(fns))]) != 1:
				refusals[token].add("dispatcher_ambiguous")
				continue
			hit = _hit(
				image,
				starts,
				"factory_pinned",
				"token_factory_pair",
				next(iter(fns)),
			)
			if hit is None:
				refusals[token].add("dispatcher_unverified")
			else:
				proofs[token].append(hit)

	for row in rows:
		if not run:
			run = [row]
			continue
		gap = row[0] - run[-1][0]
		if len(run) == 1 and 8 <= gap <= 0x40:
			run.append(row)
			continue
		stride = run[1][0] - run[0][0] if len(run) > 1 else 0
		if stride and gap == stride:
			run.append(row)
			continue
		finish()
		run = [row]
	finish()


def _thunk_token(image: PEImage, address: int) -> int | None:
	rva = address - image.image_base
	head = image.bytes_of_rva(rva, 6)
	if head is None or len(head) < 6 or head[0] != 0xB8 or head[5] != 0xC3:
		return None
	end = image.function_end(address)
	rest = image.bytes_of_rva(rva + 6, end - address - 6) or b""
	if any(byte not in (0x90, 0xCC) for byte in rest):
		return None
	return struct.unpack("<I", head[1:5])[0]


def _qword_sites(image: PEImage, va: int) -> list[int]:
	needle = struct.pack("<Q", va)
	found: list[int] = []
	for section in image.sections:
		if section.name == ".text":
			continue
		blob = image.data[section.file_offset : section.file_offset + section.file_size]
		position = 0
		while True:
			hit = blob.find(needle, position)
			if hit < 0:
				break
			if hit % 8 == 0:
				found.append(section.file_offset + hit)
			position = hit + 1
	return found


def _vtable_slots(
	image: PEImage,
	table: TokenTable,
	token_ids: set[int],
	proofs: dict[int, list[DispatchHit]],
	refusals: dict[int, set[str]],
) -> None:
	thunks: dict[int, list[int]] = defaultdict(list)
	for start in image.function_starts:
		token = _thunk_token(image, start)
		if token is not None and token in token_ids:
			thunks[token].append(start)
	unique = {token: addrs[0] for token, addrs in thunks.items() if len(addrs) == 1}
	if len(unique) < 2:
		return
	handlers, _ambiguous = debug_handlers(image)
	_vtable_kind(image, table, handlers, unique, proofs, refusals)


def _vtable_kind(
	image: PEImage,
	table: TokenTable,
	handlers: dict[str, DebugHandler],
	thunks: dict[int, int],
	proofs: dict[int, list[DispatchHit]],
	refusals: dict[int, set[str]],
) -> None:
	anchors: list[tuple[int, int, int]] = []
	for name, handler in handlers.items():
		token = table.id_of(name)
		thunk = None if token is None else thunks.get(token)
		if thunk is None:
			continue
		thunk_sites = _qword_sites(image, thunk)
		exec_sites = _qword_sites(image, handler.address)
		if len(thunk_sites) != 1 or len(exec_sites) != 1:
			continue
		delta = exec_sites[0] - thunk_sites[0]
		if delta == 0 or delta % 8 or abs(delta) > 0x80:
			continue
		anchors.append((thunk_sites[0], delta, handler.address))
	if len(anchors) < 2:
		return
	deltas = {delta for _site, delta, _address in anchors}
	if len(deltas) != 1:
		return
	delta = next(iter(deltas))
	# Slots strictly between the thunk and the Execute pointer. Offsets outside
	# that span alias the next record once several vtables are packed together.
	low, high = sorted((0, delta))
	shared: list[int] = []
	for offset in range(low + 8, high, 8):
		values = []
		for site, _anchor_delta, _address in anchors:
			at = site + offset
			if at < 0 or at + 8 > len(image.data):
				values = []
				break
			values.append(struct.unpack_from("<Q", image.data, at)[0])
		if values and len(set(values)) == 1:
			shared.append(offset)
	if not shared:
		return
	image_starts = set(image.function_starts)
	used: dict[int, list[int]] = defaultdict(list)
	chosen: list[tuple[int, int]] = []
	for token, thunk in thunks.items():
		sites = _qword_sites(image, thunk)
		if len(sites) != 1:
			continue
		site = sites[0]
		if any(
			site + offset < 0
			or site + offset + 8 > len(image.data)
			or struct.unpack_from("<Q", image.data, site + offset)[0]
			!= struct.unpack_from("<Q", image.data, anchors[0][0] + offset)[0]
			for offset in shared
		):
			continue
		at = site + delta
		if at < 0 or at + 8 > len(image.data):
			refusals[token].add("dispatcher_unverified")
			continue
		target = struct.unpack_from("<Q", image.data, at)[0]
		if target not in image_starts:
			refusals[token].add("dispatcher_unverified")
			continue
		chosen.append((token, target))
		used[target].append(token)
	for token, target in chosen:
		if len(used[target]) != 1:
			refusals[token].add("dispatcher_ambiguous")
			continue
		hit = _hit(image, image_starts, "pinned", "token_vtable", target)
		if hit is None:
			refusals[token].add("dispatcher_unverified")
		else:
			proofs[token].append(hit)
