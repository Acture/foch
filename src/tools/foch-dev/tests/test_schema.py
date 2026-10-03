from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

from foch_dev.repository import find_repository
from foch_dev.schema import cwt_snapshot_hash


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

	def test_empty_schema_and_non_checkout_fail(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			with self.assertRaisesRegex(ValueError, "no .cwt"):
				cwt_snapshot_hash(root)
			with self.assertRaisesRegex(ValueError, "specify --repo"):
				find_repository(root)


if __name__ == "__main__":
	unittest.main()
