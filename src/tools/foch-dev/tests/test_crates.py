from __future__ import annotations

import io
import tarfile
import tempfile
import unittest
from pathlib import Path

from foch_dev.crates import (
	CrateLayout,
	crate_files,
	require_outside_checkout,
	verify_crate,
	verify_relock,
)

REGISTRY: str = 'source = "registry+https://github.com/rust-lang/crates.io-index"\n'


def lock(*entries: str) -> str:
	return "version = 4\n\n" + "\n".join(entries)


def entry(name: str, version: str, *, registry: bool, checksum: str = "") -> str:
	text: str = f'[[package]]\nname = "{name}"\nversion = "{version}"\n'
	if registry:
		text += REGISTRY + f'checksum = "{checksum or name}"\n'
	return text


def write_crate(path: Path, root: str, files: dict[str, bytes]) -> None:
	with tarfile.open(path, "w:gz") as crate:
		for name, content in files.items():
			info = tarfile.TarInfo(f"{root}/{name}")
			info.size = len(content)
			crate.addfile(info, io.BytesIO(content))


class RelockTests(unittest.TestCase):
	packaged: str = lock(
		entry("foch", "0.0.1", registry=True),
		entry("foch-cli", "0.0.1", registry=False),
		entry("serde", "1.0.0", registry=True),
	)

	def test_patched_crates_may_only_lose_their_registry_source(self) -> None:
		relocked: str = lock(
			entry("foch", "0.0.1", registry=False),
			entry("foch-cli", "0.0.1", registry=False),
			entry("serde", "1.0.0", registry=True),
		)
		verify_relock(self.packaged, relocked, frozenset({"foch"}))

	def test_rejects_a_changed_third_party_pin(self) -> None:
		relocked: str = lock(
			entry("foch", "0.0.1", registry=False),
			entry("foch-cli", "0.0.1", registry=False),
			entry("serde", "1.0.0", registry=True, checksum="other"),
		)
		with self.assertRaisesRegex(
			ValueError, r"other lock entries: \['serde 1.0.0'\]"
		):
			verify_relock(self.packaged, relocked, frozenset({"foch"}))

	def test_rejects_a_patch_the_lock_does_not_use(self) -> None:
		with self.assertRaisesRegex(ValueError, "tree-sitter-paradox"):
			verify_relock(
				self.packaged, self.packaged, frozenset({"tree-sitter-paradox"})
			)


class CrateLayoutTests(unittest.TestCase):
	def test_rejects_missing_and_repository_only_files(self) -> None:
		layout = CrateLayout(frozenset({"build.rs", "LICENSE"}), ("tests/",))
		with tempfile.TemporaryDirectory() as directory:
			archive: Path = Path(directory) / "foch-0.0.1.crate"
			write_crate(archive, "foch-0.0.1", {"build.rs": b"", "LICENSE": b"text"})
			verify_crate(archive, "foch-0.0.1", layout)

			write_crate(
				archive,
				"foch-0.0.1",
				{"build.rs": b"", "tests/merge.rs": b""},
			)
			with self.assertRaisesRegex(
				ValueError, r"missing \['LICENSE'\], forbidden \['tests/merge.rs'\]"
			):
				verify_crate(archive, "foch-0.0.1", layout)

	def test_copies_must_equal_the_repository_files(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "LICENSE").write_bytes(b"AGPL text\n")
			layout = CrateLayout(frozenset(), (), {"LICENSE": root / "LICENSE"})
			archive: Path = root / "foch-0.0.1.crate"
			write_crate(archive, "foch-0.0.1", {"LICENSE": b"AGPL text\n"})
			verify_crate(archive, "foch-0.0.1", layout)
			# What a checkout without symlinks packages for LICENSE -> ../../../LICENSE.
			write_crate(archive, "foch-0.0.1", {"LICENSE": b"../../../LICENSE"})
			with self.assertRaisesRegex(ValueError, r"differing \['LICENSE'\]"):
				verify_crate(archive, "foch-0.0.1", layout)

	def test_rejects_entries_outside_the_crate_root(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			archive: Path = Path(directory) / "foch-0.0.1.crate"
			write_crate(archive, "other-0.0.1", {"build.rs": b""})
			with self.assertRaisesRegex(ValueError, "unexpected entry"):
				verify_crate(archive, "foch-0.0.1", None)


class CrateFilesTests(unittest.TestCase):
	def crate(self, root: str, files: dict[str, bytes]) -> bytes:
		with tempfile.TemporaryDirectory() as directory:
			archive: Path = Path(directory) / "x.crate"
			write_crate(archive, root, files)
			return archive.read_bytes()

	def test_reads_regular_files_below_the_root(self) -> None:
		files: dict[str, bytes] = crate_files(
			self.crate("x-1.0.0", {"a": b"1", "dir/b": b"2"}), "x-1.0.0"
		)
		self.assertEqual(files, {"a": b"1", "dir/b": b"2"})

	def test_rejects_entries_outside_the_root_or_not_regular(self) -> None:
		with self.assertRaisesRegex(ValueError, "unexpected entry"):
			crate_files(self.crate("y-1.0.0", {"a": b"1"}), "x-1.0.0")
		buffer = io.BytesIO()
		with tarfile.open(fileobj=buffer, mode="w:gz") as crate:
			link = tarfile.TarInfo("x-1.0.0/link")
			link.type = tarfile.SYMTYPE
			link.linkname = "/etc/passwd"
			crate.addfile(link)
		with self.assertRaisesRegex(ValueError, "unexpected entry"):
			crate_files(buffer.getvalue(), "x-1.0.0")

	def test_the_smoke_runs_outside_the_checkout(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo_root: Path = Path(directory)
			with self.assertRaisesRegex(ValueError, "outside the checkout"):
				require_outside_checkout(repo_root, repo_root / "target" / "smoke")
			require_outside_checkout(repo_root / "repo", repo_root / "smoke")


if __name__ == "__main__":
	unittest.main()
