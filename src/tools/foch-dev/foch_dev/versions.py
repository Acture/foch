"""The one product version every distribution channel publishes.

The workspace version in the root `Cargo.toml` is the release identity: Cargo
crates, `foch --version` and WinGet carry it verbatim, and PyPI carries its
PEP 440 spelling. Only the SemVer shapes with an unambiguous PEP 440 spelling
are releasable, so a tag can never publish different versions per channel.

`python -m foch_dev version [--tag vX.Y.Z]` prints `tag=`, `version=` and
`pep440=` lines in the `$GITHUB_OUTPUT` format, so workflows take every
channel's spelling from these rules instead of re-implementing them. With
`--tag` it fails unless the tag names the workspace version.
"""

from __future__ import annotations

import argparse
import re
import tomllib
from pathlib import Path

from .repository import find_repository

SEMVER: re.Pattern[str] = re.compile(
	r"^(?P<release>(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))"
	r"(?:-(?P<label>alpha|beta|rc)\.(?P<number>0|[1-9][0-9]*))?$"
)
PEP440_LABELS: dict[str, str] = {"alpha": "a", "beta": "b", "rc": "rc"}


def workspace_version(repo: Path) -> str:
	manifest: dict[str, object] = tomllib.loads(
		(repo / "Cargo.toml").read_text(encoding="utf-8")
	)
	workspace = manifest.get("workspace")
	package = workspace.get("package") if isinstance(workspace, dict) else None
	version = package.get("version") if isinstance(package, dict) else None
	if not isinstance(version, str):
		raise ValueError(f"{repo / 'Cargo.toml'} has no [workspace.package] version")
	return releasable_version(version)


def releasable_version(version: str) -> str:
	"""Return `version` if every channel can publish it unchanged in meaning."""
	if SEMVER.fullmatch(version) is None:
		raise ValueError(
			f"release version {version!r} must be X.Y.Z or X.Y.Z-(alpha|beta|rc).N"
		)
	return version


def pep440_version(version: str) -> str:
	"""The PyPI spelling of a releasable SemVer version: 1.2.3-rc.1 -> 1.2.3rc1."""
	match = SEMVER.fullmatch(releasable_version(version))
	assert match is not None
	label: str | None = match["label"]
	if label is None:
		return match["release"]
	return f"{match['release']}{PEP440_LABELS[label]}{match['number']}"


def version_from_tag(tag: str) -> str:
	if not tag.startswith("v"):
		raise ValueError(f"release tag must be v-prefixed, got {tag!r}")
	return releasable_version(tag.removeprefix("v"))


def release_metadata(repo: Path, tag: str | None) -> dict[str, str]:
	"""The release tag and per-channel version spellings of the workspace."""
	version: str = workspace_version(repo)
	if tag is not None and version_from_tag(tag) != version:
		raise ValueError(
			f"tag {tag} does not name the workspace version {version} of "
			f"{repo / 'Cargo.toml'}"
		)
	return {"tag": f"v{version}", "version": version, "pep440": pep440_version(version)}


def add_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument(
		"--tag", help="Fail unless this vX.Y.Z[-(alpha|beta|rc).N] tag names it"
	)
	parser.add_argument("--repo", type=Path)


def run(args: argparse.Namespace) -> int:
	for key, value in release_metadata(find_repository(args.repo), args.tag).items():
		print(f"{key}={value}")
	return 0
