from __future__ import annotations

import hashlib
from pathlib import Path


def cwt_snapshot_hash(schema_root: Path) -> str:
	if not schema_root.is_dir():
		raise ValueError(f"missing schema vendor directory: {schema_root}")

	cwt_files: list[Path] = sorted(
		path for path in schema_root.rglob("*.cwt") if path.is_file()
	)

	if not cwt_files:
		raise ValueError(f"no .cwt files found under {schema_root}")

	digest = hashlib.sha256()
	for path in cwt_files:
		content: bytes = path.read_bytes().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
		digest.update(content)

	return digest.hexdigest()
