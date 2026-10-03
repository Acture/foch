"""Mach-O metadata and exact function boundaries for the supported backend."""

from __future__ import annotations

import bisect
import hashlib
import struct
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class BinaryImage:
	path: Path
	sha256: str
	function_starts: tuple[int, ...]
	text_end: int

	def function_end(self, start: int) -> int:
		index: int = bisect.bisect_left(self.function_starts, start)
		if index == len(self.function_starts) or self.function_starts[index] != start:
			raise ValueError(f"{start:#x} is not an LC_FUNCTION_STARTS entry")
		return (
			self.function_starts[index + 1]
			if index + 1 < len(self.function_starts)
			else self.text_end
		)


def function_starts(data: bytes, text_address: int, text_end: int) -> tuple[int, ...]:
	starts: list[int] = []
	address: int = text_address
	value: int = 0
	shift: int = 0
	for byte in data:
		value |= (byte & 0x7F) << shift
		if byte & 0x80:
			shift += 7
			if shift >= 64:
				raise ValueError("invalid function-start ULEB128")
			continue
		if value == 0:
			return tuple(starts)
		address += value
		if address >= text_end:
			raise ValueError("function start lies outside executable text")
		starts.append(address)
		value = shift = 0
	raise ValueError("unterminated function-start table")


def read_binary(path: Path) -> BinaryImage:
	"""Use Mach-O function metadata rather than guessing ends from nearby labels."""
	data: bytes = path.read_bytes()
	if len(data) < 32:
		raise ValueError("truncated Mach-O header")
	magic, cpu, _, _, command_count, command_bytes, _, _ = struct.unpack_from(
		"<8I", data
	)
	if magic != 0xFEEDFACF or cpu != 0x01000007:
		raise ValueError("this backend supports the x86_64 Mach-O EU4 executable")
	command_end: int = 32 + command_bytes
	if command_end > len(data):
		raise ValueError("truncated Mach-O load commands")
	offset: int = 32
	text_address: int | None = None
	text_end: int | None = None
	starts_data: bytes | None = None
	for _ in range(command_count):
		if offset + 8 > command_end:
			raise ValueError("truncated load command")
		command, size = struct.unpack_from("<II", data, offset)
		if size < 8 or offset + size > command_end:
			raise ValueError("invalid load-command size")
		if command == 0x19:  # LC_SEGMENT_64
			if size < 72:
				raise ValueError("truncated segment command")
			segment: bytes = data[offset + 8 : offset + 24].rstrip(b"\0")
			if segment == b"__TEXT":
				text_address = struct.unpack_from("<Q", data, offset + 24)[0]
				section_count: int = struct.unpack_from("<I", data, offset + 64)[0]
				if 72 + section_count * 80 > size:
					raise ValueError("truncated section table")
				for index in range(section_count):
					section: int = offset + 72 + index * 80
					if data[section : section + 16].rstrip(b"\0") == b"__text":
						address, length = struct.unpack_from("<QQ", data, section + 32)
						text_end = address + length
		elif command == 0x26:  # LC_FUNCTION_STARTS
			if size < 16:
				raise ValueError("truncated function-start command")
			file_offset, length = struct.unpack_from("<II", data, offset + 8)
			if file_offset + length > len(data):
				raise ValueError("function-start table extends beyond executable")
			starts_data = data[file_offset : file_offset + length]
		offset += size
	if text_address is None or text_end is None or starts_data is None:
		raise ValueError("executable lacks __text or LC_FUNCTION_STARTS")
	starts: tuple[int, ...] = function_starts(starts_data, text_address, text_end)
	if not starts:
		raise ValueError("empty function-start table")
	return BinaryImage(
		path.resolve(), hashlib.sha256(data).hexdigest(), starts, text_end
	)
