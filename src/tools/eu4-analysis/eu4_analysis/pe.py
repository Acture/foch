"""PE metadata and exact function boundaries for the supported backend."""

from __future__ import annotations

import hashlib
import struct
from pathlib import Path

from .binary import BinaryImage

_DOS_MAGIC = b"MZ"
_PE_SIGNATURE = b"PE\0\0"
_X86_64 = 0x8664
_PE32_PLUS = 0x20B
_PE32_PLUS_FIXED = 112
_EXCEPTION_DIRECTORY = 3
_DIRECTORY_COUNT_MIN = _EXCEPTION_DIRECTORY + 1
_SECTION_SIZE = 40
_RUNTIME_FUNCTION_SIZE = 12


def read_pe_image(path: Path) -> BinaryImage:
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
	if struct.unpack_from("<H", data, optional)[0] != _PE32_PLUS:
		raise ValueError("this backend supports the PE32+ EU4 executable")
	if (
		optional_size < _PE32_PLUS_FIXED + _DIRECTORY_COUNT_MIN * 8
		or optional + optional_size > len(data)
	):
		raise ValueError("truncated optional header")
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
	return BinaryImage(
		path.resolve(),
		hashlib.sha256(data).hexdigest(),
		"windows",
		tuple(starts),
		image_base + text[2] + text[1],
		tuple(ends),
	)
