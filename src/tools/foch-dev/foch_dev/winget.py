"""Render and check the WinGet manifests that publish `foch` as Acture.Foch.

`render` turns one Windows release archive, exactly as `dist archive` writes
it, into the three-file multi-file manifest set that microsoft/winget-pkgs
expects at `manifests/a/Acture/Foch/<version>/`. The archive carries `foch.exe`
at its root, so WinGet's zip + portable installer links it as the `foch`
command. The installer hash is always computed from the archive itself, and
the installer URL must name that archive, by default the GitHub release
download of `v<version>`.

`check` validates a manifest directory against the official WinGet JSON
schemas of `MANIFEST_VERSION`, vendored under `schemas/winget/` from
microsoft/winget-cli at `WINGET_CLI_COMMIT` under its MIT license
(`schemas/winget/LICENSE`). Their SHA-256 is asserted on every load.
Validation uses JSON Schema Draft 7 with format checking, and then checks what
the schemas cannot express: one identity across the three files, the per-file
`ManifestType`, the `$schema` header spelling it and naming the schema `$id`, the
winget-pkgs directory layout, and the archive name in the installer URL.
`--release` also requires a releasable version and the canonical release URL.

Scalars keep their YAML types, so `check` is stricter than WinGet, which reads
every scalar as a string: an unquoted date, number or boolean fails. The
renderer quotes every string, so the same file means the same thing to every
YAML reader.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import logging
import re
from collections.abc import Mapping
from dataclasses import dataclass
from functools import cache
from importlib.resources import files
from pathlib import Path, PurePosixPath
from typing import cast
from urllib.parse import urlsplit

import yaml
from jsonschema import Draft7Validator

from . import dist
from .repository import find_repository
from .versions import releasable_version, workspace_version

LOGGER: logging.Logger = logging.getLogger(__name__)

WINGET_CLI_COMMIT: str = "dafe5e7795c6b36e2a8cea86aa9fddec898df795"
# The winget-pkgs pull request template names the schema version it accepts.
MANIFEST_VERSION: str = "1.12.0"
SCHEMA_SHA256: dict[str, str] = {
	"version": "0cdf9c17a0d19221a3612980c95a47d120c4ce532157177aa6af4368a8a78273",
	"installer": "47de5aefdea4e7ccbc8ab4e32e8230671300054bd4f688cd7ca8bdebf459f006",
	"defaultLocale": "b87eaa2252daf0bc9b2d495a3f7f5547b8b36236bdcc0543cf9f7403bd56707e",
}

PACKAGE_IDENTIFIER: str = "Acture.Foch"
PACKAGE_LOCALE: str = "en-US"
REPOSITORY_URL: str = "https://github.com/Acture/foch"
# The distribution license the PyPI project and the `foch` crate also declare.
LICENSE_EXPRESSION: str = "AGPL-3.0-only AND GPL-3.0-only AND MIT"
WINDOWS: dist.Target = dist.TARGETS["win32-x64"]
COMMAND: str = "foch"
MANIFEST_FILES: dict[str, str] = {
	"version": f"{PACKAGE_IDENTIFIER}.yaml",
	"installer": f"{PACKAGE_IDENTIFIER}.installer.yaml",
	"defaultLocale": f"{PACKAGE_IDENTIFIER}.locale.{PACKAGE_LOCALE}.yaml",
}
NESTED_INSTALLER_FILES: list[dict[str, str]] = [
	{"RelativeFilePath": WINDOWS.executable, "PortableCommandAlias": COMMAND}
]
# Dot-separated parts of letters, digits and hyphens: every releasable
# version, plus suffixed smoke versions such as `0.0.1-smoke`, never a path.
RENDERABLE_VERSION: re.Pattern[str] = re.compile(r"[0-9]+(?:\.[0-9A-Za-z-]+)*")


@dataclass(frozen=True)
class Release:
	"""The per-release inputs of one manifest set."""

	version: str
	installer_url: str
	installer_sha256: str
	release_date: str


@dataclass(frozen=True)
class RenderOptions:
	asset: Path
	release_date: str
	out: Path
	installer_url: str | None = None
	version: str | None = None
	repo: Path | None = None


def add_arguments(parser: argparse.ArgumentParser) -> None:
	commands = parser.add_subparsers(dest="winget_command", required=True)
	render = commands.add_parser(
		"render", help="Write the WinGet manifest set for a Windows release archive"
	)
	render.add_argument(
		"--asset",
		required=True,
		type=Path,
		help=f"The release archive, named {asset_name('<version>')}",
	)
	render.add_argument(
		"--installer-url",
		help="Where WinGet downloads the archive; it must end with the archive "
		"name (default: the GitHub release download of v<version>)",
	)
	render.add_argument("--release-date", required=True, help="YYYY-MM-DD")
	render.add_argument(
		"--version", help="Package version (default: the workspace version)"
	)
	render.add_argument(
		"--out",
		required=True,
		type=Path,
		help="Root that receives manifests/a/Acture/Foch/<version>/",
	)
	render.add_argument("--repo", type=Path)
	check = commands.add_parser(
		"check",
		help="Validate a WinGet manifest directory against the vendored schemas",
	)
	check.add_argument(
		"directory", type=Path, help=".../manifests/a/Acture/Foch/<version>"
	)
	check.add_argument(
		"--release",
		action="store_true",
		help="Also require a releasable version and the GitHub release installer URL",
	)


def run(args: argparse.Namespace) -> int:
	if args.winget_command == "render":
		directory: Path = render(
			RenderOptions(
				asset=args.asset,
				release_date=args.release_date,
				out=args.out,
				installer_url=args.installer_url,
				version=args.version,
				repo=args.repo,
			)
		)
		print(directory)
		return 0
	check_manifests(args.directory, release=args.release)
	return 0


def asset_name(version: str) -> str:
	return dist.archive_name(version, WINDOWS)


def release_url(version: str) -> str:
	return f"{REPOSITORY_URL}/releases/download/v{version}/{asset_name(version)}"


def manifest_path(version: str) -> PurePosixPath:
	"""The winget-pkgs location of one version, relative to the repository root."""
	return PurePosixPath(
		"manifests",
		PACKAGE_IDENTIFIER[0].lower(),
		*PACKAGE_IDENTIFIER.split("."),
		version,
	)


def renderable_version(version: str) -> str:
	if RENDERABLE_VERSION.fullmatch(version) is None:
		raise ValueError(
			f"WinGet package version {version!r} must be dot-separated letters, "
			"digits and hyphens starting with a number"
		)
	return version


def installer_sha256(asset: Path) -> str:
	with asset.open("rb") as stream:
		return hashlib.file_digest(stream, "sha256").hexdigest().upper()


def installer_url_problem(url: str, version: str) -> str | None:
	parts = urlsplit(url)
	if parts.scheme not in {"http", "https"} or not parts.netloc:
		return f"installer URL {url!r} must be an http(s) URL"
	if PurePosixPath(parts.path).name != asset_name(version):
		return f"installer URL {url!r} must end with /{asset_name(version)}"
	return None


def schema_from_bytes(
	content: bytes, expected_sha256: str, origin: str
) -> dict[str, object]:
	digest: str = hashlib.sha256(content).hexdigest()
	if digest != expected_sha256:
		raise ValueError(
			f"vendored WinGet schema {origin} has sha256 {digest}, expected "
			f"{expected_sha256} (microsoft/winget-cli@{WINGET_CLI_COMMIT})"
		)
	return cast(dict[str, object], json.loads(content))


@cache
def load_schema(manifest_type: str) -> dict[str, object]:
	name: str = f"manifest.{manifest_type}.{MANIFEST_VERSION}.json"
	resource = files("foch_dev") / "schemas" / "winget" / MANIFEST_VERSION / name
	return schema_from_bytes(resource.read_bytes(), SCHEMA_SHA256[manifest_type], name)


def schema_header(manifest_type: str, schema: Mapping[str, object]) -> str:
	"""The `$schema` comment WinGet requires on a manifest's first line.

	WinGet reads the manifest type from this URL case-sensitively, so it must
	spell `ManifestType` (`defaultLocale`), while it matches the whole URL to
	the schema `$id` case-insensitively; the 1.12.0 defaultLocale `$id` is
	lower-case, so copying it fails `winget validate`.
	"""
	url: str = (
		f"https://aka.ms/winget-manifest.{manifest_type}.{MANIFEST_VERSION}.schema.json"
	)
	if url.lower() != str(schema["$id"]).lower():
		raise ValueError(f"{url} does not name the schema {schema['$id']}")
	return f"# yaml-language-server: $schema={url}"


def version_manifest(release: Release) -> dict[str, object]:
	return {
		"PackageIdentifier": PACKAGE_IDENTIFIER,
		"PackageVersion": release.version,
		"DefaultLocale": PACKAGE_LOCALE,
		"ManifestType": "version",
		"ManifestVersion": MANIFEST_VERSION,
	}


def installer_manifest(release: Release) -> dict[str, object]:
	return {
		"PackageIdentifier": PACKAGE_IDENTIFIER,
		"PackageVersion": release.version,
		"InstallerType": "zip",
		"NestedInstallerType": "portable",
		"NestedInstallerFiles": list(NESTED_INSTALLER_FILES),
		"Commands": [COMMAND],
		"UpgradeBehavior": "install",
		"ReleaseDate": release.release_date,
		"Installers": [
			{
				"Architecture": "x64",
				"InstallerUrl": release.installer_url,
				"InstallerSha256": release.installer_sha256,
			}
		],
		"ManifestType": "installer",
		"ManifestVersion": MANIFEST_VERSION,
	}


def locale_manifest(release: Release) -> dict[str, object]:
	return {
		"PackageIdentifier": PACKAGE_IDENTIFIER,
		"PackageVersion": release.version,
		"PackageLocale": PACKAGE_LOCALE,
		"Publisher": "Acture",
		"PublisherUrl": "https://github.com/Acture",
		"Author": "Acture",
		"PackageName": "Foch",
		"PackageUrl": REPOSITORY_URL,
		"License": LICENSE_EXPRESSION,
		# Names and links every license text a release carries.
		"LicenseUrl": f"{REPOSITORY_URL}/blob/v{release.version}/NOTICE.md",
		"ShortDescription": "Analyze Europa Universalis IV mod playsets and write a "
		"separate merged mod.",
		"Description": "Foch reads an ordered Europa Universalis IV playset, models "
		"the parts of the game's loader behavior it has verified, and reports where "
		"mods conflict. foch merge analyzes the complete result before writing "
		"anything, writes what it can merge safely to a separate output mod, and "
		"leaves ambiguous units for review instead of picking a winner. Source mods "
		"and the game installation are only read. Before its first merge, foch needs "
		"an EU4 base-data snapshot built from your own game installation with foch "
		"data build eu4 --install. Foch is alpha software: merging is not yet "
		"reliable across arbitrary modlists, and Foch does not launch the game, so "
		"check a merged mod in game. foch lsp provides a language server for EU4 "
		"script. Only Europa Universalis IV is supported.",
		"Moniker": COMMAND,
		"Tags": [
			"cli",
			"eu4",
			"europa-universalis-iv",
			"language-server",
			"mod-merge",
			"modding",
		],
		"ReleaseNotesUrl": f"{REPOSITORY_URL}/releases/tag/v{release.version}",
		"ManifestType": "defaultLocale",
		"ManifestVersion": MANIFEST_VERSION,
	}


def yaml_scalar(value: str) -> str:
	"""A JSON string is a YAML double-quoted scalar; ASCII escapes keep it portable."""
	return json.dumps(value, ensure_ascii=True)


def yaml_mapping(document: Mapping[str, object], indent: str) -> list[str]:
	lines: list[str] = []
	for key, value in document.items():
		if isinstance(value, str):
			lines.append(f"{indent}{key}: {yaml_scalar(value)}")
		elif isinstance(value, list):
			lines.append(f"{indent}{key}:")
			lines.extend(yaml_sequence(cast(list[object], value), indent))
		else:
			raise ValueError(f"cannot emit {key}={value!r} in a manifest")
	return lines


def yaml_sequence(items: list[object], indent: str) -> list[str]:
	"""Block sequence items at their key's indent, as winget-pkgs manifests write them."""
	lines: list[str] = []
	for item in items:
		if isinstance(item, str):
			lines.append(f"{indent}- {yaml_scalar(item)}")
		elif isinstance(item, dict):
			nested: list[str] = yaml_mapping(
				cast(dict[str, object], item), indent + "  "
			)
			lines.append(f"{indent}- {nested[0].removeprefix(indent + '  ')}")
			lines.extend(nested[1:])
		else:
			raise ValueError(f"cannot emit sequence item {item!r} in a manifest")
	return lines


def manifest_text(document: Mapping[str, object], schema: Mapping[str, object]) -> str:
	header: str = schema_header(str(document["ManifestType"]), schema)
	return "\n".join([header, "", *yaml_mapping(document, "")]) + "\n"


def render_manifests(release: Release) -> dict[str, str]:
	"""File name to LF text for the three manifests of `release`."""
	documents: dict[str, dict[str, object]] = {
		"version": version_manifest(release),
		"installer": installer_manifest(release),
		"defaultLocale": locale_manifest(release),
	}
	return {
		MANIFEST_FILES[manifest_type]: manifest_text(
			document, load_schema(manifest_type)
		)
		for manifest_type, document in documents.items()
	}


def render(options: RenderOptions) -> Path:
	version: str = renderable_version(
		options.version or workspace_version(find_repository(options.repo))
	)
	dist.verify_archive(options.asset, WINDOWS, version, None)
	installer_url: str = options.installer_url or release_url(version)
	url_problem: str | None = installer_url_problem(installer_url, version)
	if url_problem is not None:
		raise ValueError(url_problem)
	release = Release(
		version=version,
		installer_url=installer_url,
		installer_sha256=installer_sha256(options.asset),
		release_date=options.release_date,
	)
	LOGGER.info(
		"%s %s: %s sha256 %s",
		PACKAGE_IDENTIFIER,
		version,
		options.asset.name,
		release.installer_sha256,
	)
	directory: Path = options.out.joinpath(*manifest_path(version).parts)
	directory.mkdir(parents=True, exist_ok=True)
	for name, text in render_manifests(release).items():
		(directory / name).write_bytes(text.encode("utf-8"))
	check_manifests(directory, release=False)
	return directory


def schema_errors(schema: dict[str, object], document: object) -> list[str]:
	Draft7Validator.check_schema(schema)
	validator = Draft7Validator(schema, format_checker=Draft7Validator.FORMAT_CHECKER)
	return sorted(
		f"{error.json_path}: {error.message}"
		for error in validator.iter_errors(document)
	)


def load_manifests(directory: Path) -> dict[str, dict[str, object]]:
	"""Each manifest by `ManifestType`, after its header and schema validation."""
	if not directory.is_dir():
		raise ValueError(f"missing manifest directory: {directory}")
	present: list[str] = sorted(path.name for path in directory.iterdir())
	if present != sorted(MANIFEST_FILES.values()):
		raise ValueError(
			f"{directory} must hold exactly {sorted(MANIFEST_FILES.values())}; "
			f"found {present}"
		)
	problems: list[str] = []
	documents: dict[str, dict[str, object]] = {}
	for manifest_type, name in MANIFEST_FILES.items():
		text: str = (directory / name).read_text(encoding="utf-8")
		document: object = yaml.safe_load(text)
		if not isinstance(document, dict):
			problems.append(f"{name}: not a YAML mapping")
			continue
		manifest = cast(dict[str, object], document)
		manifest_version: object = manifest.get("ManifestVersion")
		if manifest_version != MANIFEST_VERSION:
			problems.append(
				f"{name}: ManifestVersion {manifest_version!r} must be "
				f"{MANIFEST_VERSION!r}, the schema winget-pkgs accepts"
			)
			continue
		schema: dict[str, object] = load_schema(manifest_type)
		header: str = text.split("\n", 1)[0]
		expected: str = schema_header(manifest_type, schema)
		if header != expected:
			problems.append(
				f"{name}: first line must be {expected!r}, found {header!r}"
			)
		problems.extend(f"{name}: {error}" for error in schema_errors(schema, manifest))
		documents[manifest_type] = manifest
	if problems:
		raise ValueError(
			f"invalid WinGet manifests in {directory}:\n" + "\n".join(problems)
		)
	return documents


def identity_problems(documents: Mapping[str, Mapping[str, object]]) -> list[str]:
	problems: list[str] = []
	for key in ("PackageIdentifier", "PackageVersion"):
		values: dict[str, object] = {
			MANIFEST_FILES[manifest_type]: document[key]
			for manifest_type, document in documents.items()
		}
		if len(set(values.values())) != 1:
			problems.append(f"{key} differs across files: {values}")
	identifier: object = documents["version"]["PackageIdentifier"]
	if identifier != PACKAGE_IDENTIFIER:
		problems.append(
			f"PackageIdentifier must be {PACKAGE_IDENTIFIER}, found {identifier!r}"
		)
	locales: tuple[object, object] = (
		documents["version"]["DefaultLocale"],
		documents["defaultLocale"]["PackageLocale"],
	)
	if locales != (PACKAGE_LOCALE, PACKAGE_LOCALE):
		problems.append(
			f"DefaultLocale and PackageLocale must be {PACKAGE_LOCALE}, found {locales}"
		)
	return problems


def installer_problems(
	installer: Mapping[str, object], version: str, release: bool
) -> list[str]:
	problems: list[str] = []
	layout: dict[str, object] = {
		"InstallerType": "zip",
		"NestedInstallerType": "portable",
		"NestedInstallerFiles": NESTED_INSTALLER_FILES,
	}
	for key, expected in layout.items():
		if installer.get(key) != expected:
			problems.append(f"{key} must be {expected!r}, found {installer.get(key)!r}")
	installers = cast(list[dict[str, object]], installer["Installers"])
	architectures: list[object] = [entry["Architecture"] for entry in installers]
	if architectures != ["x64"]:
		problems.append(f"Installers must be one x64 installer, found {architectures}")
	for entry in installers:
		url: str = str(entry["InstallerUrl"])
		url_problem: str | None = installer_url_problem(url, version)
		if url_problem is not None:
			problems.append(url_problem)
		if release and url != release_url(version):
			problems.append(
				f"release InstallerUrl must be {release_url(version)}, found {url}"
			)
	return problems


def check_manifests(directory: Path, *, release: bool) -> None:
	documents: dict[str, dict[str, object]] = load_manifests(directory)
	problems: list[str] = identity_problems(documents)
	version: str = str(documents["version"]["PackageVersion"])
	expected: tuple[str, ...] = manifest_path(version).parts
	actual: tuple[str, ...] = directory.resolve().parts[-len(expected) :]
	if actual != expected:
		problems.append(
			f"directory must end with {'/'.join(expected)}, found {'/'.join(actual)}"
		)
	problems.extend(installer_problems(documents["installer"], version, release))
	if release:
		try:
			releasable_version(version)
		except ValueError as error:
			problems.append(str(error))
	if problems:
		raise ValueError(
			f"invalid WinGet manifests in {directory}:\n" + "\n".join(problems)
		)
	LOGGER.info(
		"%s: %s %s (ManifestVersion %s) is valid%s",
		directory,
		PACKAGE_IDENTIFIER,
		version,
		MANIFEST_VERSION,
		" for release" if release else "",
	)
