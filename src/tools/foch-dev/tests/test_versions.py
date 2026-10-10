import tempfile
import unittest
from pathlib import Path

from foch_dev.versions import (
	pep440_version,
	releasable_version,
	release_metadata,
	version_from_tag,
	workspace_version,
)


class VersionTests(unittest.TestCase):
	def test_pep440_spelling_of_releasable_versions(self) -> None:
		self.assertEqual(pep440_version("0.0.1"), "0.0.1")
		self.assertEqual(pep440_version("1.2.3-alpha.1"), "1.2.3a1")
		self.assertEqual(pep440_version("1.2.3-beta.0"), "1.2.3b0")
		self.assertEqual(pep440_version("1.2.3-rc.12"), "1.2.3rc12")

	def test_versions_without_a_shared_spelling_are_rejected(self) -> None:
		for version in (
			"1.2",
			"01.2.3",
			"1.2.3-dev.1",
			"1.2.3-alpha",
			"1.2.3-rc.1.2",
			"1.2.3+build.5",
			"1.2.3-alpha.01",
		):
			with self.subTest(version=version), self.assertRaises(ValueError):
				releasable_version(version)

	def test_tags_are_v_prefixed_versions(self) -> None:
		self.assertEqual(version_from_tag("v0.0.1"), "0.0.1")
		with self.assertRaises(ValueError):
			version_from_tag("0.0.1")

	def test_workspace_version_reads_the_root_manifest(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo = Path(directory)
			(repo / "Cargo.toml").write_text(
				'[workspace.package]\nversion = "0.3.0-rc.1"\n', encoding="utf-8"
			)
			self.assertEqual(workspace_version(repo), "0.3.0-rc.1")
			(repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
			with self.assertRaises(ValueError):
				workspace_version(repo)

	def test_release_metadata_spells_the_workspace_version_per_channel(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo = Path(directory)
			(repo / "Cargo.toml").write_text(
				'[workspace.package]\nversion = "1.2.3-beta.4"\n', encoding="utf-8"
			)
			expected = {
				"tag": "v1.2.3-beta.4",
				"version": "1.2.3-beta.4",
				"pep440": "1.2.3b4",
			}
			self.assertEqual(release_metadata(repo, None), expected)
			self.assertEqual(release_metadata(repo, "v1.2.3-beta.4"), expected)
			for tag in ("v1.2.3", "v1.2.3-beta.4+build.1", "1.2.3-beta.4"):
				with self.subTest(tag=tag), self.assertRaises(ValueError):
					release_metadata(repo, tag)


if __name__ == "__main__":
	unittest.main()
