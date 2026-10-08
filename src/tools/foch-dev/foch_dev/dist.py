"""Turn maturin's `foch` wheels into release archives and verify both.

The root `pyproject.toml` makes maturin build each supported target's
`foch-<pep440>-py3-none-<platform>.whl`. The executable in its
`.data/scripts/` is the one build every binary channel ships: PyPI publishes
the wheel, while the GitHub release, and WinGet through it, publish the
`foch-<version>-<target>.tar.gz` (`.zip` on Windows) that `archive` makes
from it. An archive holds the executable at its root plus the license texts
the wheel carries, and depends only on the wheel and `SOURCE_DATE_EPOCH`:
entries are sorted, share one mtime and are root-owned, with mode 0755 for
the executable and 0644 for the texts. `<version>` is always the Cargo
workspace version; wheels carry its PEP 440 spelling.

`smoke` installs one release the ways a user would, inside a uv environment
that inherits none of the caller's uv configuration, cache, tools or managed
Pythons: `uvx`, then `uv tool install`, `upgrade` and `uninstall`. It runs the
installed-binary checks of `foch_dev.binary` at each step, and a release
archive's executable must be byte-identical to the installed one. A local
wheel is installed offline from a directory holding only that wheel; an index
release (`--index-requirement`) verifies a published PyPI upload.
`check-binary` runs the same checks on any installed `foch`, so post-release
jobs can verify cargo, WinGet and Homebrew installs. `maturin-version` prints
the exact maturin version the root pyproject pins, which CI passes to
maturin-action, so local and CI wheels come from one maturin.
"""

from __future__ import annotations

import argparse
import base64
import csv
import gzip
import hashlib
import io
import logging
import os
import platform
import re
import shutil
import stat
import tarfile
import tempfile
import time
import tomllib
import zipfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from email.parser import BytesParser
from email.policy import compat32
from pathlib import Path

from .binary import (
	COMMAND_TIMEOUT_SECONDS,
	FochIdentity,
	scratch_environment,
	timed_run,
	verify_foch,
)
from .repository import find_repository
from .schema import SCHEMA_DIR, cwt_snapshot_hash
from .versions import pep440_version, releasable_version, workspace_version

LOGGER: logging.Logger = logging.getLogger(__name__)

DISTRIBUTION: str = "foch"
# Every License-File the wheel carries, and its name at the archive root.
ARCHIVE_LICENSES: dict[str, str] = {
	"LICENSE": "LICENSE",
	"LICENSE-MERGIRAF.txt": "LICENSE-MERGIRAF.txt",
	"NOTICE.md": "NOTICE.md",
	(SCHEMA_DIR / "LICENSE").as_posix(): "LICENSE-cwtools-eu4-config.txt",
	"THIRD-PARTY-LICENSES.txt": "THIRD-PARTY-LICENSES.txt",
}
EXECUTABLE_MODE: int = 0o755
TEXT_MODE: int = 0o644
# 1980-01-01T00:00:00Z, the earliest zip timestamp: the mtime of every archive
# entry unless SOURCE_DATE_EPOCH says otherwise.
DEFAULT_EPOCH: int = 315532800
WHEEL_NAME: re.Pattern[str] = re.compile(
	r"(?P<name>[^-]+)-(?P<version>[^-]+)-(?P<python>[^-]+)-(?P<abi>[^-]+)"
	r"-(?P<platform>[^-]+)\.whl"
)
MATURIN_PIN: re.Pattern[str] = re.compile(r"maturin==([0-9]+\.[0-9]+\.[0-9]+)")


@dataclass(frozen=True)
class Target:
	"""A supported platform: its wheel tags, executable and archive format.

	`platform_tag` fixes the platform floor every wheel tag must name: glibc
	2.17 (manylinux2014) and macOS 11.0, so a build that drifts to a newer floor
	fails here instead of narrowing the support matrix unnoticed.
	"""

	name: str
	system: str
	machines: frozenset[str]
	platform_tag: re.Pattern[str]
	executable: str
	archive_suffix: str


TARGETS: dict[str, Target] = {
	target.name: target
	for target in (
		Target(
			"linux-x64",
			"Linux",
			frozenset({"x86_64", "amd64"}),
			re.compile(r"manylinux_2_17_x86_64|manylinux2014_x86_64"),
			"foch",
			".tar.gz",
		),
		Target(
			"darwin-arm64",
			"Darwin",
			frozenset({"arm64"}),
			re.compile(r"macosx_11_0_arm64"),
			"foch",
			".tar.gz",
		),
		Target(
			"win32-x64",
			"Windows",
			frozenset({"amd64", "x86_64"}),
			re.compile(r"win_amd64"),
			"foch.exe",
			".zip",
		),
	)
}


@dataclass(frozen=True)
class ArchiveEntry:
	name: str
	data: bytes
	mode: int


@dataclass(frozen=True)
class Wheel:
	"""The parts of a verified `foch` wheel that its release archive carries."""

	path: Path
	version: str
	target: Target
	executable: bytes
	licenses: Mapping[str, bytes]
	"""License texts keyed by their archive name."""

	def archive_entries(self) -> list[ArchiveEntry]:
		entries: list[ArchiveEntry] = [
			ArchiveEntry(self.target.executable, self.executable, EXECUTABLE_MODE),
			*(
				ArchiveEntry(name, data, TEXT_MODE)
				for name, data in self.licenses.items()
			),
		]
		return sorted(entries, key=lambda entry: entry.name)


@dataclass(frozen=True)
class InstallSource:
	"""Where uv installs `foch` from, as `uvx --from` and `uv tool` arguments."""

	from_spec: str
	requirement: str
	index_arguments: tuple[str, ...]
	offline: bool


def add_identity_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument(
		"--expect-version",
		help="Cargo version foch must report (default: the checkout's workspace version)",
	)
	parser.add_argument(
		"--expect-cwt-schema-id",
		help="Embedded CWT schema id foch must report (default: the checkout's "
		"schema-hash)",
	)
	parser.add_argument("--repo", type=Path)


def add_arguments(parser: argparse.ArgumentParser) -> None:
	commands = parser.add_subparsers(dest="dist_command", required=True)
	archive = commands.add_parser(
		"archive", help="Write the release archive of a target's wheel"
	)
	archive.add_argument("--wheel", type=Path, required=True)
	archive.add_argument("--target", choices=sorted(TARGETS), required=True)
	archive.add_argument("--out", type=Path, required=True, help="Output directory")
	archive.add_argument("--repo", type=Path)

	smoke = commands.add_parser(
		"smoke", help="Install a wheel or index release through uvx and uv tool"
	)
	source = smoke.add_mutually_exclusive_group(required=True)
	source.add_argument("--wheel", type=Path, help="Local wheel, installed offline")
	source.add_argument(
		"--index-requirement",
		metavar="REQUIREMENT",
		help="Published release such as foch==0.1.0, installed from PyPI",
	)
	smoke.add_argument(
		"--archive",
		type=Path,
		help="Release archive whose executable must match the installed one",
	)
	add_identity_arguments(smoke)

	check = commands.add_parser(
		"check-binary", help="Run the installed-binary checks on any foch"
	)
	check.add_argument("executable", help="Path to foch, or a name to find on PATH")
	add_identity_arguments(check)

	maturin = commands.add_parser(
		"maturin-version", help="Print the maturin version the root pyproject pins"
	)
	maturin.add_argument("--repo", type=Path)


def host_target(system: str | None = None, machine: str | None = None) -> Target:
	system = platform.system() if system is None else system
	machine = (platform.machine() if machine is None else machine).lower()
	for target in TARGETS.values():
		if target.system == system and machine in target.machines:
			return target
	raise ValueError(f"no supported target for {system} {machine}")


def record_hash(data: bytes) -> str:
	digest: bytes = hashlib.sha256(data).digest()
	return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


def header_fields(text: bytes) -> dict[str, list[str]]:
	"""The fields of a METADATA or WHEEL file, each with all of its values."""
	message = BytesParser(policy=compat32).parsebytes(text, headersonly=True)
	fields: dict[str, list[str]] = {}
	for key, value in message.items():
		fields.setdefault(key, []).append(str(value))
	return fields


def single_field(fields: Mapping[str, list[str]], key: str, source: str) -> str:
	values: list[str] = fields.get(key, [])
	if len(values) != 1:
		raise ValueError(f"{source} must have one {key} field, found {values}")
	return values[0]


def read_wheel(path: Path, target: Target) -> Wheel:
	"""Read a `foch` bin wheel for `target`, verifying what its archive needs."""
	match = WHEEL_NAME.fullmatch(path.name)
	if match is None or match["name"] != DISTRIBUTION:
		raise ValueError(f"{path.name} is not a {DISTRIBUTION} wheel name")
	if (match["python"], match["abi"]) != ("py3", "none"):
		raise ValueError(f"{path.name} must be a py3-none bin wheel")
	platforms: list[str] = match["platform"].split(".")
	if any(target.platform_tag.fullmatch(tag) is None for tag in platforms):
		raise ValueError(
			f"{path.name} is tagged {match['platform']}, which does not fit "
			f"{target.name}"
		)
	version: str = match["version"]
	dist_info: str = f"{DISTRIBUTION}-{version}.dist-info"
	scripts: str = f"{DISTRIBUTION}-{version}.data/scripts/"
	with zipfile.ZipFile(path) as wheel:
		names: list[str] = wheel.namelist()
		record: dict[str, str] = {
			row[0]: row[1]
			for row in csv.reader(
				io.StringIO(wheel.read(f"{dist_info}/RECORD").decode("utf-8"))
			)
			if row
		}

		def verified(name: str) -> bytes:
			data: bytes = wheel.read(name)
			if record.get(name) != record_hash(data):
				raise ValueError(f"{path.name}: {name} does not match its RECORD hash")
			return data

		metadata = header_fields(verified(f"{dist_info}/METADATA"))
		tags = header_fields(verified(f"{dist_info}/WHEEL"))
		executables: list[str] = sorted(
			name.removeprefix(scripts) for name in names if name.startswith(scripts)
		)
		if executables != [target.executable]:
			raise ValueError(
				f"{path.name} must install exactly {target.executable}, found "
				f"{executables}"
			)
		executable: bytes = verified(scripts + target.executable)
		license_files: list[str] = metadata.get("License-File", [])
		if sorted(license_files) != sorted(ARCHIVE_LICENSES):
			raise ValueError(
				f"{path.name} carries License-File {sorted(license_files)}, expected "
				f"{sorted(ARCHIVE_LICENSES)}"
			)
		licenses: dict[str, bytes] = {
			ARCHIVE_LICENSES[name]: verified(f"{dist_info}/licenses/{name}")
			for name in license_files
		}

	source: str = f"{path.name} METADATA"
	if single_field(metadata, "Name", source) != DISTRIBUTION:
		raise ValueError(f"{source} does not name {DISTRIBUTION}")
	if single_field(metadata, "Version", source) != version:
		raise ValueError(f"{source} Version differs from the file name's {version}")
	expected_tags: list[str] = sorted(f"py3-none-{tag}" for tag in platforms)
	if (
		sorted(tags.get("Tag", [])) != expected_tags
		or single_field(tags, "Root-Is-Purelib", f"{path.name} WHEEL") != "false"
	):
		raise ValueError(
			f"{path.name} WHEEL must be platform-specific with Tag {expected_tags}"
		)
	return Wheel(path, version, target, executable, licenses)


def require_wheel_version(wheel: Wheel, version: str) -> None:
	expected: str = pep440_version(version)
	if wheel.version != expected:
		raise ValueError(
			f"{wheel.path.name} has version {wheel.version}; release {version} "
			f"is {expected} on PyPI"
		)


def archive_name(version: str, target: Target) -> str:
	return f"{DISTRIBUTION}-{version}-{target.name}{target.archive_suffix}"


def source_date_epoch(environment: Mapping[str, str]) -> int:
	value: str | None = environment.get("SOURCE_DATE_EPOCH")
	if value is None:
		return DEFAULT_EPOCH
	if not value.isdigit():
		raise ValueError(f"SOURCE_DATE_EPOCH must be a decimal integer: {value!r}")
	return int(value)


def write_tar_gz(entries: Sequence[ArchiveEntry], path: Path, epoch: int) -> None:
	with (
		path.open("wb") as raw,
		gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed,
		tarfile.open(
			fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT
		) as archive,
	):
		for entry in entries:
			info = tarfile.TarInfo(entry.name)
			info.size = len(entry.data)
			info.mode = entry.mode
			info.mtime = epoch
			info.uid = info.gid = 0
			info.uname = info.gname = ""
			archive.addfile(info, io.BytesIO(entry.data))


def write_zip(entries: Sequence[ArchiveEntry], path: Path, epoch: int) -> None:
	date_time = time.gmtime(max(epoch, DEFAULT_EPOCH))[:6]
	with zipfile.ZipFile(path, "w") as archive:
		for entry in entries:
			info = zipfile.ZipInfo(entry.name, date_time)
			info.create_system = 3
			info.external_attr = (stat.S_IFREG | entry.mode) << 16
			info.compress_type = zipfile.ZIP_DEFLATED
			archive.writestr(info, entry.data, compresslevel=9)


def write_archive(wheel: Wheel, version: str, out: Path, epoch: int) -> Path:
	require_wheel_version(wheel, version)
	out.mkdir(parents=True, exist_ok=True)
	path: Path = out / archive_name(version, wheel.target)
	writer = write_zip if wheel.target.archive_suffix == ".zip" else write_tar_gz
	writer(wheel.archive_entries(), path, epoch)
	LOGGER.info("wrote %s (%d bytes)", path, path.stat().st_size)
	return path


def read_archive(path: Path) -> list[ArchiveEntry]:
	"""Every entry of a release archive; anything but a regular file is refused."""
	if path.name.endswith(".zip"):
		with zipfile.ZipFile(path) as archive:
			infos: list[zipfile.ZipInfo] = archive.infolist()
			special: list[str] = [
				info.filename
				for info in infos
				if not stat.S_ISREG(info.external_attr >> 16)
			]
			if special:
				raise ValueError(f"{path.name} has non-file entries {special}")
			return [
				ArchiveEntry(
					info.filename,
					archive.read(info),
					stat.S_IMODE(info.external_attr >> 16),
				)
				for info in infos
			]
	with tarfile.open(path, "r:gz") as archive:
		members: list[tarfile.TarInfo] = archive.getmembers()
		special = [member.name for member in members if not member.isfile()]
		if special:
			raise ValueError(f"{path.name} has non-file entries {special}")
		entries: list[ArchiveEntry] = []
		for member in members:
			content = archive.extractfile(member)
			assert content is not None
			entries.append(ArchiveEntry(member.name, content.read(), member.mode))
		return entries


def verify_archive(
	path: Path, target: Target, version: str, wheel: Wheel | None
) -> ArchiveEntry:
	"""Check a release archive's name and layout; return its executable entry.

	Given the wheel it was made from, its entries must be exactly the ones
	`archive` writes from that wheel.
	"""
	if path.name != archive_name(version, target):
		raise ValueError(
			f"{path.name} is not the {target.name} archive of {version}: "
			f"{archive_name(version, target)}"
		)
	entries: list[ArchiveEntry] = read_archive(path)
	names: list[str] = sorted(entry.name for entry in entries)
	expected: list[str] = sorted([target.executable, *ARCHIVE_LICENSES.values()])
	if names != expected:
		raise ValueError(f"{path.name} holds {names}, expected {expected}")
	if wheel is not None and sorted(entries, key=lambda entry: entry.name) != (
		wheel.archive_entries()
	):
		raise ValueError(f"{path.name} differs from the archive of {wheel.path.name}")
	executable: ArchiveEntry = next(
		entry for entry in entries if entry.name == target.executable
	)
	if executable.mode != EXECUTABLE_MODE:
		raise ValueError(
			f"{path.name}: {target.executable} has mode {executable.mode:o}"
		)
	return executable


def uv_environment(
	base: Mapping[str, str], root: Path, *, offline: bool
) -> dict[str, str]:
	"""`base` with uv confined to `root`: no inherited config, index or venv."""
	environment: dict[str, str] = {
		key: value
		for key, value in base.items()
		if not key.startswith("UV_") and key != "VIRTUAL_ENV"
	}
	environment.update(
		UV_NO_CONFIG="1",
		UV_CACHE_DIR=str(root / "cache"),
		UV_TOOL_DIR=str(root / "tools"),
		UV_TOOL_BIN_DIR=str(root / "bin"),
		UV_PYTHON_INSTALL_DIR=str(root / "python"),
		UV_PYTHON_BIN_DIR=str(root / "python-bin"),
	)
	if offline:
		environment["UV_OFFLINE"] = "1"
	return environment


def required_executable(name: str) -> str:
	"""The absolute path of an executable path, or of a bare name's PATH match.

	Links are kept, so a WinGet or Homebrew shim is what runs.
	"""
	located: str | None = shutil.which(name)
	if located is None:
		raise ValueError(f"no executable {name!r} at that path or on PATH")
	return str(Path(located).absolute())


def wheel_source(wheel: Wheel, links: Path) -> InstallSource:
	"""Install from a find-links directory that holds only this wheel."""
	links.mkdir()
	copied: Path = links / wheel.path.name
	shutil.copyfile(wheel.path, copied)
	return InstallSource(
		str(copied), DISTRIBUTION, ("--no-index", "--find-links", str(links)), True
	)


def index_source(requirement: str) -> InstallSource:
	return InstallSource(requirement, requirement, (), False)


def smoke(
	source: InstallSource,
	identity: FochIdentity,
	work: Path,
	*,
	wheel: Wheel | None,
	archive: Path | None,
) -> None:
	target: Target = host_target()
	uv_root: Path = work / "uv"
	environment: dict[str, str] = uv_environment(
		scratch_environment(work / "home"), uv_root, offline=source.offline
	)
	uv: str = required_executable("uv")

	def uv_tool(*arguments: str) -> None:
		timed_run(
			[uv, "tool", *arguments],
			cwd=work,
			env=environment,
			timeout=COMMAND_TIMEOUT_SECONDS,
		)

	def check(command: Sequence[str]) -> None:
		verify_foch(command, identity, work=work, environment=environment)

	packed: ArchiveEntry | None = (
		None
		if archive is None
		else verify_archive(archive, target, identity.version, wheel)
	)
	check(
		[
			required_executable("uvx"),
			*source.index_arguments,
			"--from",
			source.from_spec,
			DISTRIBUTION,
		]
	)
	uv_tool("install", *source.index_arguments, source.requirement)
	installed: Path = uv_root / "bin" / target.executable
	if not installed.is_file():
		raise ValueError(f"uv tool install did not create {installed}")
	check([str(installed)])
	installed_bytes: bytes = installed.read_bytes()
	if wheel is not None and installed_bytes != wheel.executable:
		raise ValueError(f"the uv tool executable differs from {wheel.path.name}'s")
	uv_tool("upgrade", *source.index_arguments, DISTRIBUTION)
	check([str(installed)])
	uv_tool("uninstall", DISTRIBUTION)
	leftovers: list[str] = [
		str(path)
		for path in (installed, uv_root / "tools" / DISTRIBUTION)
		if os.path.lexists(path)
	]
	if leftovers:
		raise ValueError(f"uv tool uninstall left {leftovers}")

	if packed is not None and packed.data != installed_bytes:
		raise ValueError(f"the archive carries a different {target.executable}")


def expected_identity(args: argparse.Namespace) -> FochIdentity:
	"""The identity to require: explicit values, else the checkout's."""

	def checkout() -> Path:
		return find_repository(args.repo)

	version: str = args.expect_version or workspace_version(checkout())
	schema_id: str = args.expect_cwt_schema_id or cwt_snapshot_hash(
		checkout() / SCHEMA_DIR
	)
	return FochIdentity(releasable_version(version), schema_id)


def pinned_maturin_version(pyproject: Path) -> str:
	document: dict[str, object] = tomllib.loads(pyproject.read_text(encoding="utf-8"))
	build_system: object = document.get("build-system")
	requires: object = (
		build_system.get("requires") if isinstance(build_system, dict) else None
	)
	pin: re.Match[str] | None = (
		MATURIN_PIN.fullmatch(requires[0])
		if isinstance(requires, list)
		and len(requires) == 1
		and isinstance(requires[0], str)
		else None
	)
	if pin is None:
		raise ValueError(
			f"{pyproject} [build-system] requires must be exactly [maturin==X.Y.Z], "
			f"found {requires}"
		)
	return pin.group(1)


def run_smoke(args: argparse.Namespace, identity: FochIdentity) -> None:
	started: float = time.monotonic()
	with tempfile.TemporaryDirectory(prefix="foch-dist-smoke-") as directory:
		work: Path = Path(directory).resolve()
		wheel: Wheel | None = None
		if args.wheel is not None:
			wheel = read_wheel(args.wheel.resolve(), host_target())
			require_wheel_version(wheel, identity.version)
			source: InstallSource = wheel_source(wheel, work / "links")
		else:
			source = index_source(args.index_requirement)
		archive: Path | None = None if args.archive is None else args.archive.resolve()
		smoke(source, identity, work, wheel=wheel, archive=archive)
	LOGGER.info("dist smoke passed in %.1fs", time.monotonic() - started)


def run(args: argparse.Namespace) -> int:
	command: str = args.dist_command
	if command == "archive":
		version: str = workspace_version(find_repository(args.repo))
		wheel: Wheel = read_wheel(args.wheel, TARGETS[args.target])
		print(write_archive(wheel, version, args.out, source_date_epoch(os.environ)))
		return 0
	if command == "maturin-version":
		print(pinned_maturin_version(find_repository(args.repo) / "pyproject.toml"))
		return 0
	identity: FochIdentity = expected_identity(args)
	if command == "smoke":
		run_smoke(args, identity)
		return 0
	located: str = required_executable(args.executable)
	with tempfile.TemporaryDirectory(prefix="foch-check-binary-") as directory:
		work: Path = Path(directory).resolve()
		verify_foch(
			[located],
			identity,
			work=work,
			environment=scratch_environment(work / "home"),
		)
	return 0
