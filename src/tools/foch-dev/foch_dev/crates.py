"""Package the publishable crates and install `foch` from them out of tree.

`cargo package` writes the `.crate` files a registry upload would carry for
every published crate (`contracts.published_crates`) and the externally
released grammar. They are unpacked into a directory outside the checkout,
where a `[patch.crates-io]` config stands each unpacked crate other than
foch-cli in for its registry release, so `cargo install --locked` builds
`foch` from the packaged sources alone. The installed binary must embed the
CWT schema id of the repository's vendored rules. The grammar crate is
packaged from the checkout's pinned submodule, which may differ from its
registry release; publishing therefore needs that revision released first.
"""

from __future__ import annotations

import argparse
import io
import json
import logging
import os
import tarfile
import tempfile
import time
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import IO, cast

from .binary import FochIdentity, scratch_environment, timed_run, verify_foch
from .contracts import (
	EXTERNALLY_RELEASED_PACKAGES,
	INSTALLED_CRATE,
	SCHEMA_PACKAGE,
	CargoPackage,
	cargo_metadata,
	published_crates,
	verify_publishable_closure,
)
from .repository import find_repository
from .schema import SCHEMA_DIR, cwt_snapshot_hash

LOGGER: logging.Logger = logging.getLogger(__name__)

INSTALLED_BINARIES: frozenset[str] = frozenset({"foch"})
# crates.io rejects larger uploads.
MAX_CRATE_BYTES: int = 10 * 1024 * 1024
# crates.io's own limit on an unpacked crate.
MAX_UNPACKED_CRATE_BYTES: int = 512 * 1024 * 1024
LICENSE_FILES: frozenset[str] = frozenset(
	{"LICENSE", "LICENSE-MERGIRAF.txt", "NOTICE.md"}
)
# The registry page of every published crate, and the PyPI project's.
PACKAGE_README: Path = Path("src/apps/foch-cli/README.md")


@dataclass(frozen=True)
class CrateSmokeOptions:
	repo: Path | None = None
	work_dir: Path | None = None
	offline: bool = False
	allow_dirty: bool = False


@dataclass(frozen=True)
class CrateLayout:
	"""Files a crate must carry and the repository-only trees it must not.

	`copies` maps files the crate must carry to the repository file each must
	equal byte for byte. The crates' license texts are symlinks to the root
	ones, which a checkout without symlink support turns into a line of text.
	"""

	required: frozenset[str]
	forbidden_prefixes: tuple[str, ...]
	copies: Mapping[str, Path] = field(default_factory=dict)


def add_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument("--repo", type=Path)
	parser.add_argument(
		"--work-dir",
		type=Path,
		help="Keep crates, build output and the install in this empty directory "
		"outside the checkout (default: a temporary directory, removed afterwards)",
	)
	parser.add_argument(
		"--offline",
		action="store_true",
		help="Pass --offline to cargo; the registry cache must hold every dependency",
	)
	parser.add_argument(
		"--allow-dirty",
		action="store_true",
		help="Package uncommitted changes (cargo package --allow-dirty)",
	)


def crate_layouts(
	repo_root: Path, schema_in_crate: Path, published: tuple[str, ...]
) -> dict[str, CrateLayout]:
	"""The layout of each published crate; any other is an internal library."""
	copies: dict[str, Path] = {
		**{name: repo_root / name for name in LICENSE_FILES},
		"README.md": repo_root / PACKAGE_README,
	}
	layouts: dict[str, CrateLayout] = {
		SCHEMA_PACKAGE: CrateLayout(
			frozenset({"build.rs", "src/lib.rs"}),
			("tests/", "fuzz/"),
			{
				**copies,
				(schema_in_crate / "LICENSE").as_posix(): repo_root
				/ SCHEMA_DIR
				/ "LICENSE",
			},
		),
		INSTALLED_CRATE: CrateLayout(
			frozenset({"Cargo.lock", "src/main.rs"}), ("tests/",), copies
		),
	}
	library: CrateLayout = CrateLayout(frozenset({"src/lib.rs"}), ("tests/",), copies)
	return {name: layouts.get(name, library) for name in published}


def cargo_command(options: CrateSmokeOptions, *arguments: str) -> list[str]:
	return ["cargo", *arguments, *(["--offline"] if options.offline else [])]


def repository_toolchain(repo_root: Path) -> str | None:
	path: Path = repo_root / "rust-toolchain.toml"
	if not path.is_file():
		return None
	document = tomllib.loads(path.read_text(encoding="utf-8"))
	toolchain = cast(dict[str, object], document["toolchain"])
	return str(toolchain["channel"])


def package_crates(
	repo_root: Path,
	target_dir: Path,
	versions: Mapping[str, str],
	options: CrateSmokeOptions,
) -> dict[str, Path]:
	"""Package every crate in `versions` in one cargo run.

	One run resolves the crates crates.io does not have yet from each other.
	"""
	arguments: list[str] = ["package", "--locked", "--no-verify"]
	if options.allow_dirty:
		arguments.append("--allow-dirty")
	arguments += ["--target-dir", str(target_dir)]
	for name in versions:
		arguments += ["-p", name]
	timed_run(cargo_command(options, *arguments), cwd=repo_root)
	return {
		name: target_dir / "package" / f"{name}-{version}.crate"
		for name, version in versions.items()
	}


def crate_files(archive: bytes, root: str) -> dict[str, bytes]:
	"""The regular files of a `.crate`, keyed by their path below `root/`."""
	prefix: str = f"{root}/"
	files: dict[str, bytes] = {}
	with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as crate:
		members: list[tarfile.TarInfo] = [
			member for member in crate.getmembers() if not member.isdir()
		]
		if sum(member.size for member in members) > MAX_UNPACKED_CRATE_BYTES:
			raise ValueError(
				f"{root}.crate unpacks to more than {MAX_UNPACKED_CRATE_BYTES} bytes"
			)
		for member in members:
			name: str = member.name.removeprefix(prefix)
			if not member.isfile() or name == member.name or name in files:
				raise ValueError(
					f"{root}.crate has an unexpected entry {member.name!r}; "
					f"a crate holds unique regular files below {prefix}"
				)
			extracted: IO[bytes] | None = crate.extractfile(member)
			if extracted is None:
				raise ValueError(f"{root}.crate entry {member.name!r} has no content")
			files[name] = extracted.read()
	return files


def verify_crate(archive: Path, root: str, layout: CrateLayout | None) -> None:
	size: int = archive.stat().st_size
	LOGGER.info("%s: %d bytes (%.1f KiB)", archive.name, size, size / 1024)
	if size > MAX_CRATE_BYTES:
		raise ValueError(
			f"{archive.name} is {size} bytes, over the {MAX_CRATE_BYTES}-byte crates.io limit"
		)
	files: dict[str, bytes] = crate_files(archive.read_bytes(), root)
	LOGGER.info("%s: %d files", archive.name, len(files))
	if layout is None:
		return
	required: frozenset[str] = layout.required | layout.copies.keys()
	missing: list[str] = sorted(path for path in required if path not in files)
	forbidden: list[str] = sorted(
		path for path in files if path.startswith(layout.forbidden_prefixes)
	)
	differing: list[str] = sorted(
		path
		for path, source in layout.copies.items()
		if path in files and files[path] != source.read_bytes()
	)
	if missing or forbidden or differing:
		raise ValueError(
			f"{archive.name} must carry {sorted(required)} and nothing below "
			f"{list(layout.forbidden_prefixes)}, with {sorted(layout.copies)} equal "
			f"to the repository's; missing {missing}, forbidden {forbidden}, "
			f"differing {differing}"
		)


def unpack_crate(archive: Path, destination: Path, root: str) -> Path:
	with tarfile.open(archive, "r:gz") as crate:
		crate.extractall(destination, filter="data")
	return destination / root


def lock_entries(text: str) -> dict[tuple[str, str], dict[str, object]]:
	document = tomllib.loads(text)
	entries = cast(list[dict[str, object]], document["package"])
	return {(str(entry["name"]), str(entry["version"])): entry for entry in entries}


def verify_relock(packaged: str, relocked: str, patched: frozenset[str]) -> None:
	"""Patching may only move the patched crates from the registry to their paths.

	Every other entry, every third-party version and checksum, stays exactly
	as `cargo package` locked it.
	"""
	before = lock_entries(packaged)
	after = lock_entries(relocked)
	if before.keys() != after.keys():
		raise ValueError(
			"patching the packaged crates changed the locked package set: added "
			f"{sorted(after.keys() - before.keys())}, removed "
			f"{sorted(before.keys() - after.keys())}"
		)
	unused: list[str] = sorted(patched - {name for name, _ in before})
	if unused:
		raise ValueError(f"{INSTALLED_CRATE} does not lock patched crates {unused}")
	changed: list[str] = []
	for key, entry in before.items():
		expected: dict[str, object] = (
			{
				field: value
				for field, value in entry.items()
				if field not in {"source", "checksum"}
			}
			if key[0] in patched
			else entry
		)
		if after[key] != expected:
			changed.append(f"{key[0]} {key[1]}")
	if changed:
		raise ValueError(
			f"patching the packaged crates changed other lock entries: {changed}"
		)
	LOGGER.info(
		"lock keeps all %d packaged pins; %s resolve to the unpacked crates",
		len(before),
		", ".join(sorted(patched)),
	)


def install_cli(
	crates: Mapping[str, Path],
	work: Path,
	toolchain: str | None,
	options: CrateSmokeOptions,
) -> Path:
	"""Install the unpacked CLI with every other packaged crate patched in."""
	patched: dict[str, Path] = {
		name: path for name, path in crates.items() if name != INSTALLED_CRATE
	}
	config: Path = work / ".cargo" / "config.toml"
	config.parent.mkdir()
	config.write_text(
		"[patch.crates-io]\n"
		+ "".join(
			f"{name} = {{ path = {json.dumps(str(path))} }}\n"
			for name, path in sorted(patched.items())
		),
		encoding="utf-8",
	)
	environment: dict[str, str] = dict(os.environ)
	environment["CARGO_TARGET_DIR"] = str(work / "target")
	if toolchain is not None:
		environment["RUSTUP_TOOLCHAIN"] = toolchain
	cli: Path = crates[INSTALLED_CRATE]
	lock: Path = cli / "Cargo.lock"
	packaged_lock: str = lock.read_text(encoding="utf-8")
	timed_run(
		cargo_command(
			options,
			"metadata",
			"--format-version",
			"1",
			"--manifest-path",
			str(cli / "Cargo.toml"),
		),
		cwd=work,
		env=environment,
		capture=True,
	)
	verify_relock(packaged_lock, lock.read_text(encoding="utf-8"), frozenset(patched))
	root: Path = work / "root"
	timed_run(
		cargo_command(
			options, "install", "--locked", "--path", str(cli), "--root", str(root)
		),
		cwd=work,
		env=environment,
	)
	binaries: dict[str, Path] = {path.stem: path for path in (root / "bin").iterdir()}
	if binaries.keys() != INSTALLED_BINARIES:
		raise ValueError(
			f"{INSTALLED_CRATE} must install exactly {sorted(INSTALLED_BINARIES)}; "
			f"found {sorted(binaries)}"
		)
	return binaries["foch"]


def require_outside_checkout(repo_root: Path, work: Path) -> None:
	"""Inside the checkout, cargo would read its workspace, config and toolchain."""
	if work.is_relative_to(repo_root):
		raise ValueError(f"the crate smoke must run outside the checkout: {work}")


def smoke(repo_root: Path, work: Path, options: CrateSmokeOptions) -> None:
	require_outside_checkout(repo_root, work)
	started: float = time.monotonic()
	metadata: list[CargoPackage] = cargo_metadata(repo_root)["packages"]
	verify_publishable_closure(metadata)
	published: tuple[str, ...] = published_crates(metadata)
	packages: dict[str, CargoPackage] = {
		package["name"]: package for package in metadata
	}
	versions: dict[str, str] = {
		name: packages[name]["version"]
		for name in sorted({*published, *EXTERNALLY_RELEASED_PACKAGES})
	}
	LOGGER.info("packaging %s", ", ".join(versions))
	schema_root: Path = repo_root / SCHEMA_DIR
	schema_in_crate: Path = schema_root.resolve().relative_to(
		Path(packages[SCHEMA_PACKAGE]["manifest_path"]).parent.resolve()
	)
	schema_id: str = cwt_snapshot_hash(schema_root)
	LOGGER.info("repository cwt-schema %s (%s)", schema_id, SCHEMA_DIR.as_posix())

	archives: dict[str, Path] = package_crates(
		repo_root, work / "package", versions, options
	)
	layouts: dict[str, CrateLayout] = crate_layouts(
		repo_root, schema_in_crate, published
	)
	crates: dict[str, Path] = {}
	for name, archive in archives.items():
		root: str = f"{name}-{versions[name]}"
		verify_crate(archive, root, layouts.get(name))
		crates[name] = unpack_crate(archive, work / "crates", root)
	packaged_id: str = cwt_snapshot_hash(crates[SCHEMA_PACKAGE] / schema_in_crate)
	if packaged_id != schema_id:
		raise ValueError(
			f"the packaged {SCHEMA_PACKAGE} crate carries cwt-schema {packaged_id}, "
			f"but the repository's vendored rules hash to {schema_id}"
		)

	binary: Path = install_cli(crates, work, repository_toolchain(repo_root), options)
	verify_foch(
		[str(binary)],
		FochIdentity(versions[INSTALLED_CRATE], schema_id),
		work=work,
		environment=scratch_environment(work / "home"),
	)
	LOGGER.info(
		"crate smoke passed in %.1fs: %s",
		time.monotonic() - started,
		", ".join(
			f"{archive.name} {archive.stat().st_size} bytes"
			for archive in archives.values()
		),
	)


def run(options: CrateSmokeOptions) -> int:
	repo_root: Path = find_repository(options.repo).resolve()
	if options.work_dir is None:
		with tempfile.TemporaryDirectory(prefix="foch-crate-smoke-") as directory:
			smoke(repo_root, Path(directory).resolve(), options)
		return 0
	work: Path = options.work_dir.resolve()
	require_outside_checkout(repo_root, work)
	if work.exists() and any(work.iterdir()):
		raise ValueError(f"--work-dir must be empty or absent: {work}")
	work.mkdir(parents=True, exist_ok=True)
	smoke(repo_root, work, options)
	return 0
