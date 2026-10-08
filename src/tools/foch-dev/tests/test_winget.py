from __future__ import annotations

import argparse
import contextlib
import dataclasses
import hashlib
import io
import shutil
import tempfile
import unittest
from pathlib import Path

from foch_dev import winget
from foch_dev.dist import (
	ARCHIVE_LICENSES,
	DEFAULT_EPOCH,
	EXECUTABLE_MODE,
	TEXT_MODE,
	ArchiveEntry,
	write_zip,
)
from foch_dev.winget import (
	MANIFEST_FILES,
	MANIFEST_VERSION,
	SCHEMA_SHA256,
	RenderOptions,
	check_manifests,
	installer_sha256,
	load_schema,
	render,
	schema_from_bytes,
)

VERSION: str = "0.0.1"
RELEASE_DATE: str = "2026-10-08"

GOLDEN: dict[str, str] = {
	"Acture.Foch.yaml": """\
# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.12.0.schema.json

PackageIdentifier: "Acture.Foch"
PackageVersion: "0.0.1"
DefaultLocale: "en-US"
ManifestType: "version"
ManifestVersion: "1.12.0"
""",
	"Acture.Foch.installer.yaml": """\
# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.12.0.schema.json

PackageIdentifier: "Acture.Foch"
PackageVersion: "0.0.1"
InstallerType: "zip"
NestedInstallerType: "portable"
NestedInstallerFiles:
- RelativeFilePath: "foch.exe"
  PortableCommandAlias: "foch"
Commands:
- "foch"
UpgradeBehavior: "install"
ReleaseDate: "2026-10-08"
Installers:
- Architecture: "x64"
  InstallerUrl: "https://github.com/Acture/foch/releases/download/v0.0.1/foch-0.0.1-win32-x64.zip"
  InstallerSha256: "<sha256>"
ManifestType: "installer"
ManifestVersion: "1.12.0"
""",
	"Acture.Foch.locale.en-US.yaml": """\
# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.12.0.schema.json

PackageIdentifier: "Acture.Foch"
PackageVersion: "0.0.1"
PackageLocale: "en-US"
Publisher: "Acture"
PublisherUrl: "https://github.com/Acture"
Author: "Acture"
PackageName: "Foch"
PackageUrl: "https://github.com/Acture/foch"
License: "AGPL-3.0-only AND GPL-3.0-only AND MIT"
LicenseUrl: "https://github.com/Acture/foch/blob/v0.0.1/NOTICE.md"
ShortDescription: "Analyze Europa Universalis IV mod playsets and write a separate merged mod."
Description: "Foch reads an ordered Europa Universalis IV playset, models the parts \
of the game's loader behavior it has verified, and reports where mods conflict. foch \
merge analyzes the complete result before writing anything, writes what it can merge \
safely to a separate output mod, and leaves ambiguous units for review instead of \
picking a winner. Source mods and the game installation are only read. Before its \
first merge, foch needs an EU4 base-data snapshot built from your own game installation \
with foch data build eu4 --install. Foch is alpha software: merging is not yet reliable across arbitrary modlists, and Foch does not \
launch the game, so check a merged mod in game. foch lsp provides a language server \
for EU4 script. Only Europa Universalis IV is supported."
Moniker: "foch"
Tags:
- "cli"
- "eu4"
- "europa-universalis-iv"
- "language-server"
- "mod-merge"
- "modding"
ReleaseNotesUrl: "https://github.com/Acture/foch/releases/tag/v0.0.1"
ManifestType: "defaultLocale"
ManifestVersion: "1.12.0"
""",
}


def release_entries(executable: str = "foch.exe") -> list[ArchiveEntry]:
	"""The entries `dist archive` writes for win32-x64, with stand-in bytes."""
	return [
		ArchiveEntry(executable, b"MZ\x90\x00", EXECUTABLE_MODE),
		*(
			ArchiveEntry(name, b"text\n", TEXT_MODE)
			for name in ARCHIVE_LICENSES.values()
		),
	]


class WingetTestCase(unittest.TestCase):
	def setUp(self) -> None:
		directory = tempfile.TemporaryDirectory()
		self.addCleanup(directory.cleanup)
		self.root: Path = Path(directory.name)

	def asset(
		self, version: str = VERSION, entries: list[ArchiveEntry] | None = None
	) -> Path:
		path: Path = self.root / f"foch-{version}-win32-x64.zip"
		write_zip(entries or release_entries(), path, DEFAULT_EPOCH)
		return path

	def render(self, version: str = VERSION, **overrides: str) -> Path:
		options = RenderOptions(
			asset=self.asset(version),
			release_date=RELEASE_DATE,
			out=self.root / "out",
			version=version,
		)
		return render(dataclasses.replace(options, **overrides))


class RenderTests(WingetTestCase):
	def test_renders_the_golden_manifest_set(self) -> None:
		directory: Path = self.render()
		self.assertEqual(directory, self.root / "out/manifests/a/Acture/Foch/0.0.1")
		sha256: str = hashlib.sha256(self.asset().read_bytes()).hexdigest().upper()
		for name, expected in GOLDEN.items():
			with self.subTest(name=name):
				self.assertEqual(
					(directory / name).read_bytes(),
					expected.replace("<sha256>", sha256).encode("ascii"),
				)

	def test_version_defaults_to_the_workspace_version(self) -> None:
		repo: Path = self.root / "repo"
		(repo / "src/packages/foch").mkdir(parents=True)
		(repo / "src/packages/foch/Cargo.toml").write_text("[package]\n")
		(repo / "Cargo.toml").write_text(
			'[workspace.package]\nversion = "0.3.0-rc.1"\n'
		)
		options = RenderOptions(
			self.asset("0.3.0-rc.1"), RELEASE_DATE, self.root / "out", repo=repo
		)
		self.assertEqual(render(options).name, "0.3.0-rc.1")

	def test_hash_is_the_upper_case_sha256_of_the_asset(self) -> None:
		asset: Path = self.asset()
		self.assertEqual(
			installer_sha256(asset),
			hashlib.sha256(asset.read_bytes()).hexdigest().upper(),
		)

	def test_a_rendered_set_passes_the_release_check(self) -> None:
		check_manifests(self.render(), release=True)

	def test_semver_prereleases_are_valid_release_versions(self) -> None:
		check_manifests(self.render("0.0.1-rc.1"), release=True)

	def test_smoke_versions_render_but_are_not_releasable(self) -> None:
		directory: Path = self.render(
			"0.0.1-smoke",
			installer_url="http://127.0.0.1:8765/foch-0.0.1-smoke-win32-x64.zip",
		)
		check_manifests(directory, release=False)
		with self.assertRaisesRegex(ValueError, "release version '0.0.1-smoke'"):
			check_manifests(directory, release=True)

	def test_a_local_installer_url_is_not_a_release(self) -> None:
		directory: Path = self.render(
			installer_url="http://127.0.0.1:8765/foch-0.0.1-win32-x64.zip"
		)
		check_manifests(directory, release=False)
		with self.assertRaisesRegex(
			ValueError, "release InstallerUrl must be https://"
		):
			check_manifests(directory, release=True)

	def test_the_asset_must_be_a_release_archive(self) -> None:
		asset: Path = self.asset(entries=release_entries("bin/foch.exe"))
		options = RenderOptions(asset, RELEASE_DATE, self.root / "out", version=VERSION)
		with self.assertRaisesRegex(ValueError, r"holds \['LICENSE',"):
			render(options)

	def test_rejects_an_installer_url_naming_another_file(self) -> None:
		with self.assertRaisesRegex(
			ValueError, "must end with /foch-0.0.1-win32-x64.zip"
		):
			self.render(installer_url="https://example.com/download/foch.zip")

	def test_asset_name_must_carry_the_version(self) -> None:
		asset: Path = self.asset("0.0.2")
		options = RenderOptions(asset, RELEASE_DATE, self.root / "out", version=VERSION)
		with self.assertRaisesRegex(
			ValueError, "is not the win32-x64 archive of 0.0.1"
		):
			render(options)

	def test_rejects_versions_that_are_not_a_single_path_segment(self) -> None:
		for version in ("..", "0.0.1/x", "v0.0.1", ""):
			with self.subTest(version=version), self.assertRaises(ValueError):
				winget.renderable_version(version)

	def test_release_dates_are_calendar_dates(self) -> None:
		for value in ("2026-13-40", "2026-02-30", "20261008", "2026-1-8"):
			with (
				self.subTest(value=value),
				self.assertRaisesRegex(ValueError, f"'{value}' is not a 'date'"),
			):
				self.render(release_date=value)
			shutil.rmtree(self.root / "out")


class CheckTests(WingetTestCase):
	def edit(self, directory: Path, name: str, old: str, new: str) -> None:
		path: Path = directory / name
		text: str = path.read_text(encoding="utf-8")
		self.assertIn(old, text)
		path.write_text(text.replace(old, new, 1), encoding="utf-8")

	def test_rejects_mismatched_package_versions(self) -> None:
		directory: Path = self.render()
		self.edit(
			directory,
			"Acture.Foch.yaml",
			'PackageVersion: "0.0.1"',
			'PackageVersion: "0.0.2"',
		)
		with self.assertRaisesRegex(ValueError, "PackageVersion differs across files"):
			check_manifests(directory, release=False)

	def test_rejects_an_unquoted_numeric_hash(self) -> None:
		directory: Path = self.render()
		text: str = (directory / "Acture.Foch.installer.yaml").read_text()
		sha256: str = text.split('InstallerSha256: "', 1)[1].split('"', 1)[0]
		self.edit(
			directory,
			"Acture.Foch.installer.yaml",
			f'"{sha256}"',
			"0" * 64,
		)
		with self.assertRaisesRegex(ValueError, "0 is not of type 'string'"):
			check_manifests(directory, release=False)

	def test_formats_are_checked(self) -> None:
		directory: Path = self.render()
		self.edit(directory, "Acture.Foch.installer.yaml", RELEASE_DATE, "2026-13-40")
		with self.assertRaisesRegex(ValueError, "'2026-13-40' is not a 'date'"):
			check_manifests(directory, release=False)

	def test_header_must_spell_the_manifest_type(self) -> None:
		# The schema $id is lower-case, but winget validate parses the type from
		# the header case-sensitively and rejects "defaultlocale".
		directory: Path = self.render()
		self.edit(
			directory,
			"Acture.Foch.locale.en-US.yaml",
			"winget-manifest.defaultLocale.",
			"winget-manifest.defaultlocale.",
		)
		with self.assertRaisesRegex(ValueError, "first line must be"):
			check_manifests(directory, release=False)

	def test_rejects_a_manifest_version_without_a_vendored_schema(self) -> None:
		directory: Path = self.render()
		self.edit(
			directory,
			"Acture.Foch.yaml",
			'ManifestVersion: "1.12.0"',
			'ManifestVersion: "1.10.0"',
		)
		with self.assertRaisesRegex(
			ValueError, "ManifestVersion '1.10.0' must be '1.12.0'"
		):
			check_manifests(directory, release=False)

	def test_rejects_a_wrong_manifest_type(self) -> None:
		directory: Path = self.render()
		self.edit(
			directory,
			"Acture.Foch.yaml",
			'ManifestType: "version"',
			'ManifestType: "installer"',
		)
		with self.assertRaisesRegex(ValueError, "Acture.Foch.yaml: \\$.ManifestType"):
			check_manifests(directory, release=False)

	def test_directory_must_match_the_identifier_and_version(self) -> None:
		moved: Path = self.root / "manifests/a/Acture/Other/0.0.1"
		shutil.copytree(self.render(), moved)
		with self.assertRaisesRegex(
			ValueError, "directory must end with manifests/a/Acture/Foch"
		):
			check_manifests(moved, release=False)

	def test_directory_holds_exactly_the_three_manifests(self) -> None:
		directory: Path = self.render()
		(directory / "notes.txt").write_text("extra")
		with self.assertRaisesRegex(ValueError, "must hold exactly"):
			check_manifests(directory, release=False)


class SchemaTests(unittest.TestCase):
	def test_vendored_schemas_load_with_their_pinned_hashes(self) -> None:
		for manifest_type in SCHEMA_SHA256:
			with self.subTest(manifest_type=manifest_type):
				schema: dict[str, object] = load_schema(manifest_type)
				self.assertEqual(
					schema["$id"],
					f"https://aka.ms/winget-manifest.{manifest_type.lower()}."
					f"{MANIFEST_VERSION}.schema.json",
				)

	def test_rejects_altered_schema_bytes(self) -> None:
		with self.assertRaisesRegex(ValueError, "has sha256"):
			schema_from_bytes(b"{}", "0" * 64, "manifest.version.1.12.0.json")

	def test_every_manifest_type_has_a_file_name(self) -> None:
		self.assertEqual(set(SCHEMA_SHA256), set(MANIFEST_FILES))


class CommandLineTests(WingetTestCase):
	def test_render_then_check_through_the_subcommands(self) -> None:
		parser = argparse.ArgumentParser()
		winget.add_arguments(parser)
		stdout = io.StringIO()
		with contextlib.redirect_stdout(stdout):
			status: int = winget.run(
				parser.parse_args(
					[
						"render",
						"--asset",
						str(self.asset()),
						"--release-date",
						RELEASE_DATE,
						"--version",
						VERSION,
						"--out",
						str(self.root / "out"),
					]
				)
			)
		self.assertEqual(status, 0)
		directory = Path(stdout.getvalue().strip())
		self.assertEqual(
			winget.run(parser.parse_args(["check", "--release", str(directory)])), 0
		)


if __name__ == "__main__":
	unittest.main()
