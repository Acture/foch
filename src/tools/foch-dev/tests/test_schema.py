from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

from foch_dev.repository import find_repository
from foch_dev.schema import cwt_files, cwt_snapshot_hash


class SchemaTests(unittest.TestCase):
	def test_digest_binds_names_and_normalized_contents(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "b.cwt").write_bytes(b"second\rline\r\n")
			(root / "a.cwt").write_bytes(b"first\n")
			(root / "ignored.txt").write_text("ignored")
			expected = hashlib.sha256(b"foch-cwt-schema-id/v2\n")
			for part in (b"a.cwt", b"first\n", b"b.cwt", b"second\nline\n"):
				expected.update(len(part).to_bytes(8, "little") + part)
			self.assertEqual(cwt_snapshot_hash(root), expected.hexdigest())

	def test_moving_a_file_changes_the_digest(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "common").mkdir()
			(root / "common/types.cwt").write_text("types = { }\n")
			before: str = cwt_snapshot_hash(root)
			(root / "events").mkdir()
			(root / "common/types.cwt").rename(root / "events/types.cwt")
			self.assertNotEqual(cwt_snapshot_hash(root), before)

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
