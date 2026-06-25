#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import sys
from pathlib import Path


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    schema_root = repo_root / "vendor" / "cwtools-eu4-config"
    cwt_files = sorted(path for path in schema_root.rglob("*.cwt") if path.is_file())

    if not schema_root.is_dir():
        print(f"missing schema vendor directory: {schema_root}", file=sys.stderr)
        return 1

    if not cwt_files:
        print(f"no .cwt files found under {schema_root}", file=sys.stderr)
        return 1

    digest = hashlib.sha256()
    for path in cwt_files:
        digest.update(path.read_bytes())

    print(digest.hexdigest())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
