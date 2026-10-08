from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

from foch_dev.repository import find_repository
from foch_dev.schema import cwt_files, cwt_snapshot_hash


class SchemaTests(unittest.TestCase):
	def test_digest_matches_sorted_normalized_bytes(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "b.cwt").write_bytes(b"second\rline\r\n")
			(root / "a.cwt").write_bytes(b"first\n")
			(root / "ignored.txt").write_text("ignored")
			self.assertEqual(
				cwt_snapshot_hash(root),
				hashlib.sha256(b"first\nsecond\nline\n").hexdigest(),
			)

	def test_file_order_matches_the_rust_schema_order(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			for relative in (
				"c.cwt",
				"B.cwt",
				"a/b.cwt",
				"a.cwt",
				"a-b.cwt",
				"a/x.txt",
			):
				path: Path = root / relative
				path.parent.mkdir(parents=True, exist_ok=True)
				path.write_text("types = { }\n")
			(root / "linked.cwt").symlink_to(root / "c.cwt")
			self.assertEqual(
				[path.relative_to(root).as_posix() for path in cwt_files(root)],
				["a-b.cwt", "a.cwt", "a/b.cwt", "B.cwt", "c.cwt"],
			)

	def test_empty_schema_and_non_checkout_fail(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			with self.assertRaisesRegex(ValueError, "no .cwt"):
				cwt_snapshot_hash(root)
			with self.assertRaisesRegex(ValueError, "specify --repo"):
				find_repository(root)


if __name__ == "__main__":
	unittest.main()
