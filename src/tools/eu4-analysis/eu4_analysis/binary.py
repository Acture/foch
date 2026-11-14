"""Executable image metadata: exact function boundaries, format-neutral."""

from __future__ import annotations

import bisect
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
	from .pe import PEImage


@dataclass(frozen=True)
class BinaryImage:
	path: Path
	sha256: str
	# The platform the binary was built for: same game version, same engine
	# source, one label per build the loading rules record provenance for.
	platform: str
	function_starts: tuple[int, ...]
	text_end: int
	# Exact end per start, as the PE exception table records it; a Mach-O
	# start table only bounds each function by the next start.
	function_ends: tuple[int, ...] | None = None

	def __post_init__(self) -> None:
		if self.function_ends is not None and len(self.function_ends) != len(
			self.function_starts
		):
			raise ValueError("function ends do not match function starts")

	def function_end(self, start: int) -> int:
		index: int = bisect.bisect_left(self.function_starts, start)
		if index == len(self.function_starts) or self.function_starts[index] != start:
			raise ValueError(f"{start:#x} is not a recorded function start")
		if self.function_ends is not None:
			return self.function_ends[index]
		return (
			self.function_starts[index + 1]
			if index + 1 < len(self.function_starts)
			else self.text_end
		)


def read_binary(path: Path) -> BinaryImage | PEImage:
	"""Read one supported executable: a PE image by its `MZ` DOS header, else Mach-O."""
	with path.open("rb") as image:
		pe: bool = image.read(2) == b"MZ"
	if pe:
		from .pe import read_pe_image

		return read_pe_image(path)
	from .macho import read_macho_image

	return read_macho_image(path)
