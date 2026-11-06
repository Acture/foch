"""Format dispatch for the analyzed EU4 executable.

Both backends expose exact function metadata — the macOS Mach-O image from
LC_FUNCTION_STARTS, the Windows PE image from `.pdata` — so the rest of the
tool never guesses a function boundary.
"""

from __future__ import annotations

from pathlib import Path

from . import macho, pe
from .macho import BinaryImage as BinaryImage  # the Mach-O shape is the common one

__all__ = ["BinaryImage", "read_binary"]


def read_binary(path: Path) -> BinaryImage | pe.PEImage:
	data: bytes = Path(path).read_bytes()
	if len(data) >= 2 and data[:2] == b"MZ":
		return pe.read_binary(Path(path))
	return macho.read_binary(Path(path))
