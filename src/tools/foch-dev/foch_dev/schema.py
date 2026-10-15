"""The `cwt_schema_id` that `foch` embeds, computed as its build script does."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

SCHEMA_DIR: Path = Path("src/packages/foch/vendor/cwtools-eu4-config")
"""The vendored CWT build input of the `foch` package, relative to the
repository root."""

SUPPLEMENT_DIR: Path = Path("src/packages/foch/src/game/eu4/cwt_supplement")
"""foch's own CWT rules, compiled after `SCHEMA_DIR`, relative to the
repository root."""

SCHEMA_DIRS: tuple[Path, ...] = (SCHEMA_DIR, SUPPLEMENT_DIR)
"""Every CWT build input, in the order the build script compiles them."""


def raise_walk_error(error: OSError) -> None:
	raise error


SCHEMA_ID_DOMAIN: bytes = b"foch-cwt-schema-id/v2\n"
"""Mirror of the domain tag `cwt_schema_id_from_dirs` hashes first."""

SCHEMA_DIRECTORY_BOUNDARY: bytes = (2**64 - 1).to_bytes(8, "little")
"""Mirror of the marker `cwt_schema_id_from_dirs` hashes between two
directories."""


def schema_file_name(root: Path, path: Path) -> bytes:
	"""Mirror `schema_file_name` in `src/game/schema/source.rs`: the name below
	`root`, its components joined with `/`."""
	return b"/".join(os.fsencode(name) for name in path.relative_to(root).parts)


def schema_file_order_key(root: Path, path: Path) -> tuple[bytes, bytes]:
	"""Mirror `schema_file_order_key` in `src/game/schema/source.rs`.

	Names below `root` are ASCII case folded, joined with `/` and compared
	bytewise; names that differ only in case fall back to their exact bytes.
	"""
	exact: bytes = schema_file_name(root, path)
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


def cwt_snapshot_hash(*schema_roots: Path) -> str:
	"""Mirror `cwt_schema_id_from_dirs`: after the domain tag, each file's name
	and normalized content, both length-prefixed (u64 little-endian), one
	directory after another with a boundary between them."""
	digest = hashlib.sha256(SCHEMA_ID_DOMAIN)
	for index, schema_root in enumerate(schema_roots):
		if index > 0:
			digest.update(SCHEMA_DIRECTORY_BOUNDARY)
		for path in cwt_files(schema_root):
			name: bytes = schema_file_name(schema_root, path)
			content: bytes = (
				path.read_bytes().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
			)
			for part in (name, content):
				digest.update(len(part).to_bytes(8, "little"))
				digest.update(part)
	return digest.hexdigest()


def repository_schema_hash(base: Path) -> str:
	"""The `cwt_schema_id` of the schema inputs below `base`, a repository
	root."""
	return cwt_snapshot_hash(*(base / directory for directory in SCHEMA_DIRS))
