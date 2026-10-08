from __future__ import annotations

import gzip
import shutil
import stat
import sys
import tarfile
import tempfile
import time
import unittest
import zipfile
from collections.abc import Mapping
from pathlib import Path
from unittest import mock

from foch_dev.binary import FochIdentity
from foch_dev.dist import (
	ARCHIVE_LICENSES,
	DEFAULT_EPOCH,
	TARGETS,
	Target,
	host_target,
	pinned_maturin_version,
	read_archive,
	read_wheel,
	record_hash,
	required_executable,
	smoke,
	source_date_epoch,
	uv_environment,
	verify_archive,
	wheel_source,
	write_archive,
)
from foch_dev.versions import pep440_version

SCHEMA_ID: str = "5d636ca3ec1497a27308b712b2601fef9cb8993a14d777824486c6930a7070f3"
LICENSES: dict[str, bytes] = {
	name: f"text of {name}\n".encode() for name in ARCHIVE_LICENSES
}
PLATFORMS: dict[str, str] = {
	"linux-x64": "manylinux_2_28_x86_64",
	"darwin-arm64": "macosx_11_0_arm64",
	"win32-x64": "win_amd64",
}
LINUX: Target = TARGETS["linux-x64"]
DARWIN: Target = TARGETS["darwin-arm64"]
WINDOWS: Target = TARGETS["win32-x64"]


def make_wheel(
	directory: Path,
	*,
	version: str = "0.0.1",
	platform: str = PLATFORMS["darwin-arm64"],
	executable: str = "foch",
	content: bytes = b"\x7fELF foch",
	licenses: Mapping[str, bytes] = LICENSES,
	tampered: str | None = None,
) -> Path:
	"""A wheel shaped like maturin's bin wheels; `tampered` breaks one RECORD hash."""
	dist_info: str = f"foch-{version}.dist-info"
	metadata: str = (
		f"Metadata-Version: 2.4\nName: foch\nVersion: {version}\n"
		+ "".join(f"License-File: {name}\n" for name in licenses)
		+ "\n# foch\n"
	)
	wheel_file: str = "Wheel-Version: 1.0\nRoot-Is-Purelib: false\n" + "".join(
		f"Tag: py3-none-{tag}\n" for tag in platform.split(".")
	)
	files: dict[str, bytes] = {
		f"foch-{version}.data/scripts/{executable}": content,
		f"{dist_info}/METADATA": metadata.encode(),
		f"{dist_info}/WHEEL": wheel_file.encode(),
		**{f"{dist_info}/licenses/{name}": data for name, data in licenses.items()},
	}
	record: str = "".join(
		f"{name},{'sha256=bad' if name == tampered else record_hash(data)},"
		f"{len(data)}\n"
		for name, data in files.items()
	)
	path: Path = directory / f"foch-{version}-py3-none-{platform}.whl"
	with zipfile.ZipFile(path, "w") as wheel:
		for name, data in files.items():
			info = zipfile.ZipInfo(name)
			info.external_attr = (stat.S_IFREG | 0o755) << 16
			wheel.writestr(info, data)
		wheel.writestr(f"{dist_info}/RECORD", record + f"{dist_info}/RECORD,,\n")
	return path


class ArchiveTests(unittest.TestCase):
	def setUp(self) -> None:
		self.directory: Path = Path(tempfile.mkdtemp())
		self.addCleanup(shutil.rmtree, self.directory)

	def archive_twice(self, target: Target, version: str, epoch: int) -> Path:
		wheel_path: Path = make_wheel(
			self.directory,
			version=pep440_version(version),
			platform=PLATFORMS[target.name],
			executable=target.executable,
		)
		wheel = read_wheel(wheel_path, target)
		first: Path = write_archive(wheel, version, self.directory / "a", epoch)
		second: Path = write_archive(wheel, version, self.directory / "b", epoch)
		self.assertEqual(first.name, second.name)
		self.assertEqual(first.read_bytes(), second.read_bytes())
		self.assertEqual(
			sorted(read_archive(first), key=lambda entry: entry.name),
			wheel.archive_entries(),
		)
		verify_archive(first, target, version, wheel)
		return first

	def test_unix_archive_is_flat_sorted_and_normalized(self) -> None:
		archive: Path = self.archive_twice(DARWIN, "0.0.1", DEFAULT_EPOCH)
		self.assertEqual(archive.name, "foch-0.0.1-darwin-arm64.tar.gz")
		with gzip.open(archive) as compressed:
			compressed.read()
			self.assertEqual(compressed.mtime, DEFAULT_EPOCH)
		with tarfile.open(archive, "r:gz") as tar:
			members: list[tarfile.TarInfo] = tar.getmembers()
		self.assertEqual(
			[member.name for member in members],
			sorted(["foch", *ARCHIVE_LICENSES.values()]),
		)
		self.assertIn("LICENSE-cwtools-eu4-config.txt", ARCHIVE_LICENSES.values())
		for member in members:
			with self.subTest(member=member.name):
				self.assertEqual(member.mode, 0o755 if member.name == "foch" else 0o644)
				self.assertEqual(member.mtime, DEFAULT_EPOCH)
				self.assertEqual((member.uid, member.gid), (0, 0))
				self.assertEqual((member.uname, member.gname), ("", ""))

	def test_source_date_epoch_sets_every_mtime(self) -> None:
		epoch: int = 1_700_000_000
		self.assertEqual(source_date_epoch({"SOURCE_DATE_EPOCH": str(epoch)}), epoch)
		self.assertEqual(source_date_epoch({}), DEFAULT_EPOCH)
		with self.assertRaises(ValueError):
			source_date_epoch({"SOURCE_DATE_EPOCH": "yesterday"})
		archive: Path = self.archive_twice(LINUX, "0.0.1", epoch)
		with tarfile.open(archive, "r:gz") as tar:
			self.assertEqual({member.mtime for member in tar.getmembers()}, {epoch})
		zipped: Path = self.archive_twice(WINDOWS, "0.0.1", epoch)
		with zipfile.ZipFile(zipped) as archive_zip:
			self.assertEqual(
				{info.date_time for info in archive_zip.infolist()},
				{time.gmtime(epoch)[:6]},
			)

	def test_windows_archive_is_a_zip_with_foch_exe(self) -> None:
		archive: Path = self.archive_twice(WINDOWS, "0.1.0-rc.1", DEFAULT_EPOCH)
		self.assertEqual(archive.name, "foch-0.1.0-rc.1-win32-x64.zip")
		with zipfile.ZipFile(archive) as archive_zip:
			infos: list[zipfile.ZipInfo] = archive_zip.infolist()
		self.assertEqual(
			[info.filename for info in infos],
			sorted(["foch.exe", *ARCHIVE_LICENSES.values()]),
		)
		modes: dict[str, int] = {
			info.filename: stat.S_IMODE(info.external_attr >> 16) for info in infos
		}
		self.assertEqual(modes["foch.exe"], 0o755)
		self.assertEqual(modes["NOTICE.md"], 0o644)

	def test_wheel_version_must_be_the_release_version(self) -> None:
		wheel = read_wheel(make_wheel(self.directory, version="0.0.2"), DARWIN)
		with self.assertRaisesRegex(ValueError, "release 0.0.1 is 0.0.1 on PyPI"):
			write_archive(wheel, "0.0.1", self.directory / "out", DEFAULT_EPOCH)
		candidate = read_wheel(make_wheel(self.directory, version="0.1.0rc1"), DARWIN)
		with self.assertRaisesRegex(ValueError, "release 0.1.0-beta.1 is 0.1.0b1"):
			write_archive(candidate, "0.1.0-beta.1", self.directory, DEFAULT_EPOCH)

	def test_platform_tags_must_fit_the_target(self) -> None:
		cases: list[tuple[str, Target]] = [
			(PLATFORMS["linux-x64"], DARWIN),
			("linux_x86_64", LINUX),
			("manylinux_2_28_x86_64.manylinux_2_28_aarch64", LINUX),
			("manylinux_2_39_x86_64", LINUX),
			("manylinux_2_17_x86_64.manylinux2014_x86_64", LINUX),
			("macosx_11_0_x86_64", DARWIN),
			("macosx_14_0_arm64", DARWIN),
			("win32", WINDOWS),
		]
		for platform, target in cases:
			with (
				self.subTest(platform=platform),
				self.assertRaisesRegex(ValueError, "does not fit"),
			):
				read_wheel(make_wheel(self.directory, platform=platform), target)

	def test_windows_wheel_must_ship_foch_exe(self) -> None:
		path: Path = make_wheel(self.directory, platform="win_amd64")
		with self.assertRaisesRegex(ValueError, r"exactly foch.exe, found \['foch'\]"):
			read_wheel(path, WINDOWS)

	def test_record_hashes_and_license_files_are_verified(self) -> None:
		tampered: Path = make_wheel(
			self.directory, tampered="foch-0.0.1.data/scripts/foch"
		)
		with self.assertRaisesRegex(ValueError, "does not match its RECORD hash"):
			read_wheel(tampered, DARWIN)
		without_notice: dict[str, bytes] = {
			name: data for name, data in LICENSES.items() if name != "NOTICE.md"
		}
		with self.assertRaisesRegex(ValueError, "carries License-File"):
			read_wheel(make_wheel(self.directory, licenses=without_notice), DARWIN)

	def test_an_archive_must_be_exactly_its_wheel(self) -> None:
		archive: Path = self.archive_twice(DARWIN, "0.0.1", DEFAULT_EPOCH)
		other = read_wheel(
			make_wheel(self.directory / "a", content=b"another build"), DARWIN
		)
		with self.assertRaisesRegex(ValueError, "differs from the archive"):
			verify_archive(archive, DARWIN, "0.0.1", other)
		with self.assertRaisesRegex(ValueError, "not the linux-x64 archive"):
			verify_archive(archive, LINUX, "0.0.1", None)
		extra: Path = self.directory / "extra" / archive.name
		extra.parent.mkdir()
		with (
			tarfile.open(archive, "r:gz") as source,
			tarfile.open(extra, "w:gz") as copy,
		):
			for member in source.getmembers():
				copy.addfile(member, source.extractfile(member))
			copy.addfile(tarfile.TarInfo("README.md"))
		with self.assertRaisesRegex(ValueError, "holds"):
			verify_archive(extra, DARWIN, "0.0.1", None)


class SmokeEnvironmentTests(unittest.TestCase):
	def test_host_targets(self) -> None:
		self.assertIs(host_target("Darwin", "arm64"), DARWIN)
		self.assertIs(host_target("Linux", "x86_64"), LINUX)
		self.assertIs(host_target("Windows", "AMD64"), WINDOWS)
		with self.assertRaisesRegex(ValueError, "no supported target"):
			host_target("Linux", "aarch64")

	def test_executables_resolve_to_absolute_paths(self) -> None:
		# The checks run from a scratch directory, so a relative path must not survive.
		with mock.patch("shutil.which", return_value="target/release/foch"):
			self.assertEqual(
				required_executable("target/release/foch"),
				str(Path.cwd() / "target/release/foch"),
			)
		with (
			mock.patch("shutil.which", return_value=None),
			self.assertRaisesRegex(ValueError, "or on PATH"),
		):
			required_executable("foch")

	def test_uv_environment_inherits_no_uv_state(self) -> None:
		base: dict[str, str] = {
			"PATH": "/bin",
			"UV_INDEX_URL": "https://example.invalid/simple",
			"UV_TOOL_DIR": "/home/user/tools",
			"VIRTUAL_ENV": "/repo/.venv",
		}
		root: Path = Path("/scratch/uv")
		online: dict[str, str] = uv_environment(base, root, offline=False)
		self.assertNotIn("UV_INDEX_URL", online)
		self.assertNotIn("VIRTUAL_ENV", online)
		self.assertNotIn("UV_OFFLINE", online)
		self.assertEqual(online["PATH"], "/bin")
		self.assertEqual(online["UV_NO_CONFIG"], "1")
		self.assertEqual(online["UV_TOOL_DIR"], str(root / "tools"))
		self.assertEqual(uv_environment(base, root, offline=True)["UV_OFFLINE"], "1")

	def test_maturin_must_be_pinned_exactly(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			pyproject: Path = Path(directory) / "pyproject.toml"
			pyproject.write_text(
				'[build-system]\nrequires = ["maturin==1.15.0"]\n', encoding="utf-8"
			)
			self.assertEqual(pinned_maturin_version(pyproject), "1.15.0")
			for requires in ('["maturin>=1.15,<2"]', '["maturin==1.15.0", "x"]'):
				pyproject.write_text(
					f"[build-system]\nrequires = {requires}\n", encoding="utf-8"
				)
				with (
					self.subTest(requires=requires),
					self.assertRaisesRegex(ValueError, "exactly"),
				):
					pinned_maturin_version(pyproject)


FAKE_FOCH: str = """\
#!/bin/sh
case "$1" in
--version) printf '%s' '{version_output}' ;;
--help) echo 'Usage: foch <COMMAND>' ;;
input) echo 'game: eu4'; echo "local_patch $(dirname "$3")/local_patch" ;;
*) exit 2 ;;
esac
"""


def host_platform() -> str | None:
	try:
		return PLATFORMS[host_target().name]
	except ValueError:
		return None


@unittest.skipIf(
	sys.platform == "win32"
	or host_platform() is None
	or shutil.which("uv") is None
	or shutil.which("uvx") is None,
	"needs uv and uvx on a supported Unix host",
)
class OfflineSmokeTests(unittest.TestCase):
	"""Install a stand-in wheel through uvx and uv tool, offline."""

	def test_wheel_and_archive_pass_the_smoke(self) -> None:
		identity = FochIdentity("0.1.0-rc.1", SCHEMA_ID)
		target: Target = host_target()
		script: bytes = FAKE_FOCH.format(
			version_output=identity.version_output()
		).encode()
		with tempfile.TemporaryDirectory() as directory:
			work: Path = Path(directory).resolve()
			wheel = read_wheel(
				make_wheel(
					work,
					version="0.1.0rc1",
					platform=str(host_platform()),
					content=script,
				),
				target,
			)
			archive: Path = write_archive(
				wheel, identity.version, work / "dist", DEFAULT_EPOCH
			)
			passing: Path = work / "passing"
			passing.mkdir()
			smoke(
				wheel_source(wheel, passing / "links"),
				identity,
				passing,
				wheel=wheel,
				archive=archive,
			)


if __name__ == "__main__":
	unittest.main()
