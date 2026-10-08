"""The `cwt_schema_id` that `foch` embeds, computed as its build script does."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

SCHEMA_DIR: Path = Path("src/packages/foch/vendor/cwtools-eu4-config")
"""The CWT build input of the `foch` package, relative to the repository root."""


def raise_walk_error(error: OSError) -> None:
	raise error


def schema_file_order_key(root: Path, path: Path) -> tuple[bytes, bytes]:
	"""Mirror `schema_file_order_key` in `src/game/schema/source.rs`.

	Names below `root` are ASCII case folded, joined with `/` and compared
	bytewise; names that differ only in case fall back to their exact bytes.
	"""
	exact: bytes = b"/".join(os.fsencode(name) for name in path.relative_to(root).parts)
	return exact.lower(), exact


def cwt_files(schema_root: Path) -> list[Path]:
	"""Every `.cwt` regular file below `schema_root`, in schema file order.

	Like the Rust walk, symbolic links are not followed and an unreadable
	directory fails the walk.
	"""
	if not schema_root.is_dir():
		raise ValueError(f"missing schema vendor directory: {schema_root}")
	files: list[Path] = []
	for directory, _, names in os.walk(schema_root, onerror=raise_walk_error):
		for name in names:
			path: Path = Path(directory, name)
			if path.suffix == ".cwt" and not path.is_symlink() and path.is_file():
				files.append(path)
	if not files:
		raise ValueError(f"no .cwt files found under {schema_root}")
	return sorted(files, key=lambda path: schema_file_order_key(schema_root, path))


def cwt_snapshot_hash(schema_root: Path) -> str:
	digest = hashlib.sha256()
	for path in cwt_files(schema_root):
		content: bytes = path.read_bytes().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
		digest.update(content)
	return digest.hexdigest()
