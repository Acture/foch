"""Refuse a release tag that any distribution channel would reject or misplace.

`release preflight --tag vX.Y.Z[-(alpha|beta|rc).N]` runs before anything is
built or uploaded. It prints one line per check: PASS, FAIL with a remedy, or
SKIP for a check whose prerequisite failed. It exits 1 if any check fails.
Registries are only read, and an unexpected HTTP status, a network error or a
malformed answer fails its check, so the preflight fails closed; a 5xx answer
is retried once.

- tag: the tag spells the releasable workspace version, which crates.io,
  WinGet and GitHub publish verbatim and PyPI in its PEP 440 spelling.
- crates:closure: foch and foch-cli build from registry artifacts alone, each
  path dependency pinned exactly to its package's version, by the same rule
  `foch_dev check` enforces.
- tree-sitter-paradox (each externally released workspace member): crates.io
  has the checkout's version unyanked, and the registry `.crate` carries the
  same files and bytes as `cargo package` of the checkout. Published foch
  crates build against the registry copy, so any difference would ship another
  grammar under the version the checkout was tested with.
- crates.io: neither foch nor foch-cli has the release version, and foch 0.1.0,
  which shipped another product, is yanked; until then `cargo install foch`
  can install it.
- PyPI: project foch lacks the PEP 440 version.
- WinGet: microsoft/winget-pkgs has no Acture.Foch manifest for the version.
- GitHub: no published release for the tag has assets yet; channels hash
  published assets, so a re-run must never replace them. A draft is invisible
  to a contents:read token. Its assets are unconsumed, and the release job
  compares them byte for byte.
"""

from __future__ import annotations

import argparse
import enum
import hashlib
import http.client
import json
import logging
import os
import re
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable, Mapping
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import Protocol, TypeVar, cast

from .binary import COMMAND_TIMEOUT_SECONDS, timed_run
from .contracts import (
	EXTERNALLY_RELEASED_PACKAGES,
	PUBLISHABLE_PACKAGES,
	CargoPackage,
	cargo_metadata,
	publishable_closure_problems,
)
from .crates import crate_files
from .dist import DISTRIBUTION as PYPI_PROJECT
from .repository import find_repository
from .versions import pep440_version, version_from_tag, workspace_version
from .winget import REPOSITORY_URL, manifest_path

LOGGER: logging.Logger = logging.getLogger(__name__)

# crates.io's crawler policy asks every client to identify itself and a contact.
USER_AGENT: str = f"foch-dev-release-preflight (+{REPOSITORY_URL})"
REQUEST_TIMEOUT_SECONDS: float = 30.0
RETRY_DELAY_SECONDS: float = 2.0
MAX_RESPONSE_BYTES: int = 16 * 1024 * 1024
MAX_LISTED_DIFFERENCES: int = 8
MAX_DETAIL_CHARS: int = 600

CRATES_INDEX: str = "https://index.crates.io"
CRATES_DOWNLOADS: str = "https://static.crates.io/crates"
PYPI_API: str = "https://pypi.org/pypi"
GITHUB_API: str = "https://api.github.com"
GITHUB_REPOSITORY: str = urllib.parse.urlsplit(REPOSITORY_URL).path.strip("/")
WINGET_REPOSITORY: str = "microsoft/winget-pkgs"
# Versions that shipped another product under a crate name this release reuses.
# While one is unyanked, `cargo install <crate>` without a version can resolve
# it, so each must be yanked before Foch publishes the crate.
SUPERSEDED_VERSIONS: dict[str, frozenset[str]] = {"foch": frozenset({"0.1.0"})}
# Not compared between the registry and the local `.crate`:
# - `.cargo_vcs_info.json` records the commit and path of whichever checkout
#   ran `cargo package`;
# - `Cargo.toml` is cargo's normalized rewrite, whose text depends on the cargo
#   release that packaged it; `Cargo.toml.orig`, the author's manifest, is
#   compared instead;
# - `Cargo.lock` is resolved against the registry index at packaging time (the
#   local one from this workspace's lock), and dependents of a library never
#   read it; the grammar's dependency requirements live in `Cargo.toml.orig`.
UNCOMPARED_CRATE_FILES: frozenset[str] = frozenset(
	{".cargo_vcs_info.json", "Cargo.toml", "Cargo.lock"}
)

CRATES_REMEDY: str = (
	"make sure index.crates.io and static.crates.io are reachable, then re-run"
)
CONTENT_REMEDY: str = (
	"commit the grammar checkout and keep Cargo.lock current, so `cargo package "
	"--locked` succeeds; make sure index.crates.io and static.crates.io are "
	"reachable; then re-run"
)
METADATA_REMEDY: str = "make `cargo metadata --locked --no-deps` succeed, then re-run"
PYPI_REMEDY: str = "make sure pypi.org is reachable, then re-run"
GITHUB_REMEDY: str = (
	"set GITHUB_TOKEN (anonymous GitHub API calls are limited to 60 an hour) "
	"and make sure api.github.com is reachable, then re-run"
)
CHECK_ERRORS: tuple[type[Exception], ...] = (
	OSError,
	ValueError,
	http.client.HTTPException,
	subprocess.SubprocessError,
	tarfile.TarError,
)

# Full SemVer 2.0.0: registry versions may carry any pre-release label.
FULL_SEMVER: re.Pattern[str] = re.compile(
	r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
	r"(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
	r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?"
	r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)

T = TypeVar("T")
# (major, minor, patch, 1 for a release or 0 for a pre-release, identifiers);
# an identifier is (0, number, "") or (1, 0, text), so numbers rank lower.
SemverKey = tuple[int, int, int, int, tuple[tuple[int, int, str], ...]]


class Status(enum.StrEnum):
	PASS = "PASS"
	FAIL = "FAIL"
	SKIP = "SKIP"


@dataclass(frozen=True)
class CheckResult:
	check: str
	status: Status
	detail: str
	remedy: str = ""


@dataclass(frozen=True)
class Response:
	status: int
	body: bytes


class Fetch(Protocol):
	def __call__(self, url: str, headers: Mapping[str, str]) -> Response: ...


class Readable(Protocol):
	def read(self, size: int = -1, /) -> bytes: ...


@dataclass(frozen=True)
class Services:
	"""Everything the checks reach outside the checkout, injectable for tests."""

	fetch: Fetch
	# The workspace packages as `cargo metadata --no-deps` lists them.
	packages: Callable[[], list[CargoPackage]]
	# (crate name, version) -> the `.crate` bytes `cargo package` writes for it.
	package_crate: Callable[[str, str], bytes]
	sleep: Callable[[float], None]
	github_token: str | None


@dataclass(frozen=True)
class Release:
	tag: str
	version: str
	pypi_version: str


@dataclass(frozen=True)
class Unavailable:
	reason: str


@dataclass(frozen=True)
class Context:
	repo: Path
	tag: str
	release: Release | None
	packages: list[CargoPackage] | Unavailable
	services: Services


CheckGroup = Callable[[Context], list[CheckResult]]


@dataclass(frozen=True)
class IndexEntry:
	version: str
	yanked: bool
	checksum: str


@dataclass(frozen=True)
class ExternalCrate:
	"""A workspace member released from its own repository."""

	name: str
	version: str
	path: str
	repository: str


class UnexpectedResponse(ValueError):
	def __init__(self, url: str, response: Response) -> None:
		text: str = " ".join(response.body[:300].decode("utf-8", "replace").split())
		super().__init__(f"GET {url} returned HTTP {response.status}: {text}")


def add_arguments(parser: argparse.ArgumentParser) -> None:
	commands = parser.add_subparsers(dest="release_command", required=True)
	preflight_parser: argparse.ArgumentParser = commands.add_parser(
		"preflight",
		help="Check that every channel can publish the tag; read-only",
	)
	preflight_parser.add_argument(
		"--tag", required=True, help="vX.Y.Z or vX.Y.Z-(alpha|beta|rc).N"
	)
	preflight_parser.add_argument("--repo", type=Path)


def run(args: argparse.Namespace) -> int:
	"""Run `release preflight`, the release subcommand, against the live registries."""
	repo: Path = find_repository(args.repo)
	token: str = os.environ.get("GITHUB_TOKEN", "")
	services: Services = Services(
		fetch=urllib_fetch,
		packages=lambda: cargo_metadata(repo)["packages"],
		package_crate=cargo_packager(repo),
		sleep=time.sleep,
		github_token=token or None,
	)
	started: float = time.monotonic()
	status: int = report(args.tag, preflight(repo, args.tag, services))
	LOGGER.info("preflight finished in %.1fs", time.monotonic() - started)
	return status


def report(tag: str, results: list[CheckResult]) -> int:
	"""Print one line per check and a summary; 1 if any check failed."""
	width: int = max(len(result.check) for result in results)
	for result in results:
		print(render(result, width))
	failed: int = sum(result.status is Status.FAIL for result in results)
	print(
		f"preflight {tag}: {failed} of {len(results)} checks failed"
		if failed
		else f"preflight {tag}: all {len(results)} checks passed"
	)
	return 1 if failed else 0


def render(result: CheckResult, width: int) -> str:
	text: str = result.detail + (f" | remedy: {result.remedy}" if result.remedy else "")
	text = " ".join(text.split())
	if len(text) > MAX_DETAIL_CHARS:
		text = text[: MAX_DETAIL_CHARS - 3] + "..."
	return f"{result.status} {result.check.ljust(width)}  {text}"


def preflight(repo: Path, tag: str, services: Services) -> list[CheckResult]:
	"""Every check's result, in a fixed order; channel groups run concurrently."""
	context: Context = Context(
		repo, tag, releasable_tag(tag), attempt(services.packages), services
	)
	with ThreadPoolExecutor(max_workers=len(CHECK_GROUPS)) as pool:
		batches: list[Future[list[CheckResult]]] = [
			pool.submit(timed_group, name, group, context)
			for name, group in CHECK_GROUPS
		]
		return [result for batch in batches for result in batch.result()]


def timed_group(name: str, group: CheckGroup, context: Context) -> list[CheckResult]:
	started: float = time.monotonic()
	results: list[CheckResult] = group(context)
	LOGGER.info("%s checks finished in %.1fs", name, time.monotonic() - started)
	return results


def releasable_tag(tag: str) -> Release | None:
	try:
		version: str = version_from_tag(tag)
	except ValueError:
		return None
	return Release(tag, version, pep440_version(version))


def attempt(load: Callable[[], T]) -> T | Unavailable:
	"""`load()`, or why it failed; a failure fails the checks that need it."""
	try:
		return load()
	except CHECK_ERRORS as error:
		return Unavailable(str(error))


def guarded(
	check: str, remedy: str, evaluate: Callable[[], CheckResult]
) -> CheckResult:
	outcome: CheckResult | Unavailable = attempt(evaluate)
	if isinstance(outcome, Unavailable):
		return failed_lookup(check, outcome, remedy)
	return outcome


def failed_lookup(check: str, unavailable: Unavailable, remedy: str) -> CheckResult:
	return CheckResult(
		check, Status.FAIL, f"could not check: {unavailable.reason}", remedy
	)


def skipped(check: str, reason: str) -> CheckResult:
	return CheckResult(check, Status.SKIP, reason)


# HTTP


def urllib_fetch(url: str, headers: Mapping[str, str]) -> Response:
	"""GET `url`; an HTTP error status is a response, a network error raises."""
	request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, **headers})
	try:
		with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as answer:
			return Response(answer.status, read_capped(answer, url))
	except urllib.error.HTTPError as error:
		return Response(error.code, read_capped(error, url))


def read_capped(stream: Readable, url: str) -> bytes:
	body: bytes = stream.read(MAX_RESPONSE_BYTES + 1)
	if len(body) > MAX_RESPONSE_BYTES:
		raise ValueError(f"GET {url} returned more than {MAX_RESPONSE_BYTES} bytes")
	return body


def get(
	services: Services, url: str, headers: Mapping[str, str] | None = None
) -> bytes | None:
	"""The body on 200, None on 404; any other status raises UnexpectedResponse."""
	request_headers: Mapping[str, str] = headers or {}
	response: Response = services.fetch(url, request_headers)
	if response.status >= 500:
		LOGGER.info("GET %s returned HTTP %d; retrying once", url, response.status)
		services.sleep(RETRY_DELAY_SECONDS)
		response = services.fetch(url, request_headers)
	if response.status == 200:
		return response.body
	if response.status == 404:
		return None
	raise UnexpectedResponse(url, response)


def json_object(body: bytes, where: str) -> dict[str, object]:
	return as_object(json.loads(body), where)


def as_object(value: object, where: str) -> dict[str, object]:
	if not isinstance(value, dict):
		raise ValueError(f"{where} is not a JSON object")
	return cast(dict[str, object], value)


def typed_field(
	mapping: Mapping[str, object], key: str, kind: type[T], where: str
) -> T:
	value: object = mapping.get(key)
	if not isinstance(value, kind):
		raise ValueError(f"{where}: {key!r} must be a {kind.__name__}, got {value!r}")
	return value


def github_headers(token: str | None) -> dict[str, str]:
	headers: dict[str, str] = {
		"Accept": "application/vnd.github+json",
		"X-GitHub-Api-Version": "2022-11-28",
	}
	if token:
		headers["Authorization"] = f"Bearer {token}"
	return headers


def github_get(services: Services, path: str) -> bytes | None:
	return get(services, f"{GITHUB_API}/{path}", github_headers(services.github_token))


# SemVer


def semver_key(version: str) -> SemverKey:
	"""SemVer 2.0.0 precedence; build metadata does not take part."""
	match = FULL_SEMVER.fullmatch(version)
	if match is None:
		raise ValueError(f"{version!r} is not a SemVer version")
	major, minor, patch = (int(part) for part in match.group(1, 2, 3))
	prerelease: str | None = match.group(4)
	if prerelease is None:
		return (major, minor, patch, 1, ())
	return (
		major,
		minor,
		patch,
		0,
		tuple(
			(0, int(part), "") if part.isdigit() else (1, 0, part)
			for part in prerelease.split(".")
		),
	)


# crates.io


def index_path(name: str) -> str:
	"""The sparse-index file of a crate, as cargo lays it out."""
	lowered: str = name.lower()
	if len(lowered) <= 2:
		return f"{len(lowered)}/{lowered}"
	if len(lowered) == 3:
		return f"3/{lowered[0]}/{lowered}"
	return f"{lowered[:2]}/{lowered[2:4]}/{lowered}"


def parse_index(body: bytes, name: str) -> tuple[IndexEntry, ...]:
	entries: list[IndexEntry] = []
	for line in body.decode("utf-8").splitlines():
		if not line.strip():
			continue
		record: dict[str, object] = json_object(line.encode(), f"index entry of {name}")
		where: str = f"index entry of {name}"
		entries.append(
			IndexEntry(
				typed_field(record, "vers", str, where),
				typed_field(record, "yanked", bool, where),
				typed_field(record, "cksum", str, where),
			)
		)
	return tuple(entries)


def crate_index(services: Services, name: str) -> tuple[IndexEntry, ...] | None:
	"""Every version crates.io lists for `name`, or None if the crate is absent."""
	body: bytes | None = get(services, f"{CRATES_INDEX}/{index_path(name)}")
	return None if body is None else parse_index(body, name)


def same_version(entries: tuple[IndexEntry, ...], version: str) -> list[IndexEntry]:
	"""Entries crates.io treats as `version`; it ignores build metadata."""
	key: SemverKey = semver_key(version)
	return [entry for entry in entries if semver_key(entry.version) == key]


def check_unpublished(
	name: str, version: str, entries: tuple[IndexEntry, ...] | None
) -> CheckResult:
	check: str = f"{name}:unpublished"
	taken: list[IndexEntry] = same_version(entries or (), version)
	if not taken:
		return CheckResult(check, Status.PASS, f"crates.io has no {name} {version}")
	yanked: str = " (yanked)" if all(entry.yanked for entry in taken) else ""
	return CheckResult(
		check,
		Status.FAIL,
		f"crates.io already has {name} {taken[0].version}{yanked}; crates.io never "
		"accepts a version twice, even a yanked one",
		"bump [workspace.package] version in the root Cargo.toml and tag that",
	)


def check_superseded(name: str, entries: tuple[IndexEntry, ...] | None) -> CheckResult:
	check: str = f"{name}:superseded"
	superseded: frozenset[str] = SUPERSEDED_VERSIONS[name]
	live: list[str] = sorted(
		(
			entry.version
			for entry in entries or ()
			if not entry.yanked and entry.version in superseded
		),
		key=semver_key,
	)
	if not live:
		return CheckResult(
			check,
			Status.PASS,
			f"crates.io has no unyanked {name} "
			f"{', '.join(sorted(superseded, key=semver_key))}, which shipped "
			"another product",
		)
	return CheckResult(
		check,
		Status.FAIL,
		f"crates.io has {name} {', '.join(live)} unyanked, which shipped another "
		f"product; `cargo install {name}` can install it",
		"; ".join(f"cargo yank --version {version} {name}" for version in live),
	)


def publishable_crate_checks(name: str, context: Context) -> list[CheckResult]:
	checks: list[str] = [f"{name}:unpublished"]
	if name in SUPERSEDED_VERSIONS:
		checks.append(f"{name}:superseded")
	release: Release | None = context.release
	if release is None:
		return [skipped(check, "needs a releasable tag") for check in checks]
	loaded = attempt(partial(crate_index, context.services, name))
	if isinstance(loaded, Unavailable):
		return [failed_lookup(check, loaded, CRATES_REMEDY) for check in checks]
	results: list[CheckResult] = [
		guarded(
			checks[0],
			CRATES_REMEDY,
			partial(check_unpublished, name, release.version, loaded),
		)
	]
	if name in SUPERSEDED_VERSIONS:
		results.append(
			guarded(checks[1], CRATES_REMEDY, partial(check_superseded, name, loaded))
		)
	return results


def crates_checks(context: Context) -> list[CheckResult]:
	return [
		result
		for name in sorted(PUBLISHABLE_PACKAGES)
		for result in publishable_crate_checks(name, context)
	]


# Workspace crates: the published closure and externally released members


def check_closure(packages: list[CargoPackage]) -> CheckResult:
	check: str = "crates:closure"
	problems: list[str] = publishable_closure_problems(packages)
	if not problems:
		return CheckResult(
			check,
			Status.PASS,
			f"{' and '.join(sorted(PUBLISHABLE_PACKAGES))} pin every path "
			"dependency at its package's exact version",
		)
	return CheckResult(
		check,
		Status.FAIL,
		"; ".join(problems),
		'pin each [workspace.dependencies] path dependency as version = "=X.Y.Z" '
		"of its package, or move the submodule to the release the pin names",
	)


def external_crate(
	repo: Path, packages: list[CargoPackage], name: str
) -> ExternalCrate:
	package: CargoPackage | None = next(
		(package for package in packages if package["name"] == name), None
	)
	if package is None:
		raise ValueError(f"the workspace has no {name} package")
	repository: str | None = package["repository"]
	if repository is None:
		raise ValueError(f"{package['manifest_path']} names no repository")
	path: str = (
		Path(package["manifest_path"])
		.parent.resolve()
		.relative_to(repo.resolve())
		.as_posix()
	)
	return ExternalCrate(name, package["version"], path, repository)


def release_remedy(crate: ExternalCrate) -> str:
	return (
		f"release {crate.name} {crate.version} from its own repository "
		f"({crate.repository}) first, then point {crate.path} at the released commit"
	)


def registry_release(
	crate: ExternalCrate, entries: tuple[IndexEntry, ...] | None
) -> IndexEntry | None:
	"""The unyanked crates.io entry for exactly the checkout's version."""
	for entry in entries or ():
		if entry.version == crate.version and not entry.yanked:
			return entry
	return None


def check_registry_release(
	crate: ExternalCrate, entries: tuple[IndexEntry, ...] | None
) -> CheckResult:
	check: str = f"{crate.name}:crates.io"
	if registry_release(crate, entries) is not None:
		return CheckResult(
			check,
			Status.PASS,
			f"crates.io has {crate.name} {crate.version}, not yanked",
		)
	state: str
	if entries is None:
		state = f"crates.io has no {crate.name} crate"
	elif any(entry.version == crate.version for entry in entries):
		state = f"crates.io has {crate.name} {crate.version} only yanked"
	else:
		state = (
			f"crates.io has no {crate.name} {crate.version} "
			f"(it has {', '.join(entry.version for entry in entries) or 'no versions'})"
		)
	return CheckResult(
		check,
		Status.FAIL,
		f"{state}; published foch crates resolve it from crates.io",
		release_remedy(crate),
	)


def crate_differences(
	local: Mapping[str, bytes], published: Mapping[str, bytes]
) -> list[str]:
	local_names: set[str] = set(local) - UNCOMPARED_CRATE_FILES
	published_names: set[str] = set(published) - UNCOMPARED_CRATE_FILES
	return [
		*(
			f"only in the checkout: {name}"
			for name in sorted(local_names - published_names)
		),
		*(
			f"only on crates.io: {name}"
			for name in sorted(published_names - local_names)
		),
		*(
			f"different bytes: {name}"
			for name in sorted(local_names & published_names)
			if local[name] != published[name]
		),
	]


def packaged_commit(files: Mapping[str, bytes]) -> str:
	"""The commit `.cargo_vcs_info.json` names, for remedies only."""
	info: bytes | None = files.get(".cargo_vcs_info.json")
	if info is None:
		return "an unrecorded commit"
	git: object = json_object(info, ".cargo_vcs_info.json").get("git")
	sha: object = git.get("sha1") if isinstance(git, dict) else None
	return f"commit {sha[:12]}" if isinstance(sha, str) else "an unrecorded commit"


def check_crate_content(
	crate: ExternalCrate, entry: IndexEntry, services: Services
) -> CheckResult:
	check: str = f"{crate.name}:content"
	root: str = f"{crate.name}-{crate.version}"
	url: str = f"{CRATES_DOWNLOADS}/{crate.name}/{root}.crate"
	published: bytes | None = get(services, url)
	digest: str = "" if published is None else hashlib.sha256(published).hexdigest()
	if published is None or digest != entry.checksum.lower():
		found: str = "is missing" if published is None else f"has sha256 {digest}"
		return CheckResult(
			check,
			Status.FAIL,
			f"the crates.io index lists {root} with sha256 {entry.checksum}, "
			f"but {url} {found}",
			"re-run; if it persists, the registry does not serve what its index "
			"promises, so publish nothing until crates.io resolves it",
		)
	published_files: dict[str, bytes] = crate_files(published, root)
	local_files: dict[str, bytes] = crate_files(
		services.package_crate(crate.name, crate.version), root
	)
	differences: list[str] = crate_differences(local_files, published_files)
	if not differences:
		compared: int = len(set(published_files) - UNCOMPARED_CRATE_FILES)
		return CheckResult(
			check,
			Status.PASS,
			f"{compared} files of {root}.crate on crates.io (sha256 {digest[:12]}) "
			f"match `cargo package` of {crate.path} byte for byte",
		)
	listed: list[str] = differences[:MAX_LISTED_DIFFERENCES]
	more: str = (
		f" and {len(differences) - len(listed)} more"
		if len(differences) > len(listed)
		else ""
	)
	return CheckResult(
		check,
		Status.FAIL,
		f"crates.io {root} ({packaged_commit(published_files)}) differs from "
		f"{crate.path} ({packaged_commit(local_files)}): {'; '.join(listed)}{more}",
		f"move {crate.path} to the commit crates.io {crate.version} was packaged "
		f"from, or release the checkout as a new {crate.name} version from its own "
		f"repository ({crate.repository}) and pin that",
	)


def released_crate_checks(
	name: str, packages: list[CargoPackage], context: Context
) -> list[CheckResult]:
	registry, content = f"{name}:crates.io", f"{name}:content"
	crate = attempt(partial(external_crate, context.repo, packages, name))
	if isinstance(crate, Unavailable):
		return [
			failed_lookup(
				registry, crate, "fix the workspace manifest or the submodule"
			),
			skipped(content, "needs the workspace manifests"),
		]
	loaded = attempt(partial(crate_index, context.services, name))
	if isinstance(loaded, Unavailable):
		return [
			failed_lookup(registry, loaded, CRATES_REMEDY),
			skipped(content, f"needs the crates.io index of {name}"),
		]
	entry: IndexEntry | None = registry_release(crate, loaded)
	return [
		check_registry_release(crate, loaded),
		skipped(content, f"needs {name} {crate.version} on crates.io")
		if entry is None
		else guarded(
			content,
			CONTENT_REMEDY,
			partial(check_crate_content, crate, entry, context.services),
		),
	]


def workspace_crate_checks(context: Context) -> list[CheckResult]:
	packages: list[CargoPackage] | Unavailable = context.packages
	if isinstance(packages, Unavailable):
		return [
			failed_lookup("crates:closure", packages, METADATA_REMEDY),
			*(
				skipped(check, "needs the workspace packages")
				for name in sorted(EXTERNALLY_RELEASED_PACKAGES)
				for check in (f"{name}:crates.io", f"{name}:content")
			),
		]
	return [
		check_closure(packages),
		*(
			result
			for name in sorted(EXTERNALLY_RELEASED_PACKAGES)
			for result in released_crate_checks(name, packages, context)
		),
	]


def cargo_packager(repo: Path) -> Callable[[str, str], bytes]:
	"""Package a workspace member as a registry upload would carry it.

	`--no-verify` skips the build and a private target directory keeps the run
	clear of other cargo builds; without `--allow-dirty` an uncommitted
	checkout fails, as it would for a real publish.
	"""

	def package(name: str, version: str) -> bytes:
		with tempfile.TemporaryDirectory(prefix="foch-preflight-") as directory:
			timed_run(
				[
					"cargo",
					"package",
					"--locked",
					"--no-verify",
					"-p",
					name,
					"--target-dir",
					directory,
				],
				cwd=repo,
				capture=True,
				timeout=COMMAND_TIMEOUT_SECONDS,
			)
			return (
				Path(directory) / "package" / f"{name}-{version}.crate"
			).read_bytes()

	return package


# Tag


def check_tag(repo: Path, tag: str) -> CheckResult:
	check: str = "tag"
	try:
		version: str = version_from_tag(tag)
	except ValueError as error:
		return CheckResult(
			check,
			Status.FAIL,
			str(error),
			"tag vX.Y.Z or vX.Y.Z-(alpha|beta|rc).N, the [workspace.package] version",
		)
	try:
		workspace: str = workspace_version(repo)
	except ValueError as error:
		return CheckResult(
			check,
			Status.FAIL,
			str(error),
			"set [workspace.package] version to X.Y.Z or X.Y.Z-(alpha|beta|rc).N",
		)
	if version != workspace:
		return CheckResult(
			check,
			Status.FAIL,
			f"{tag} names {version}, but the workspace version is {workspace}",
			f"tag v{workspace}, or set [workspace.package] version to {version} "
			"and tag that commit",
		)
	return CheckResult(
		check,
		Status.PASS,
		f"{tag} releases {version} on crates.io, WinGet and GitHub and "
		f"{pep440_version(version)} on PyPI",
	)


def tag_checks(context: Context) -> list[CheckResult]:
	return [check_tag(context.repo, context.tag)]


# PyPI


def check_pypi_version(release: Release, services: Services) -> CheckResult:
	check: str = "pypi:version"
	url: str = f"{PYPI_API}/{PYPI_PROJECT}/{release.pypi_version}/json"
	if get(services, url) is None:
		return CheckResult(
			check, Status.PASS, f"PyPI has no {PYPI_PROJECT} {release.pypi_version}"
		)
	return CheckResult(
		check,
		Status.FAIL,
		f"PyPI already has {PYPI_PROJECT} {release.pypi_version}; PyPI never "
		"accepts a file name twice",
		"bump [workspace.package] version in the root Cargo.toml and tag that",
	)


def pypi_checks(context: Context) -> list[CheckResult]:
	release: Release | None = context.release
	if release is None:
		return [skipped("pypi:version", "needs a releasable tag")]
	return [
		guarded(
			"pypi:version",
			PYPI_REMEDY,
			lambda: check_pypi_version(release, context.services),
		)
	]


# WinGet


def check_winget_version(release: Release, services: Services) -> CheckResult:
	check: str = "winget:version"
	path: str = manifest_path(release.version).as_posix()
	if github_get(services, f"repos/{WINGET_REPOSITORY}/contents/{path}") is None:
		return CheckResult(check, Status.PASS, f"{WINGET_REPOSITORY} has no {path}")
	return CheckResult(
		check,
		Status.FAIL,
		f"{WINGET_REPOSITORY} already has {path}; its installers are hashed and "
		"never replaced",
		"bump [workspace.package] version in the root Cargo.toml and tag that",
	)


def winget_checks(context: Context) -> list[CheckResult]:
	release: Release | None = context.release
	if release is None:
		return [skipped("winget:version", "needs a releasable tag")]
	return [
		guarded(
			"winget:version",
			GITHUB_REMEDY,
			lambda: check_winget_version(release, context.services),
		)
	]


# GitHub release


def check_github_release(release: Release, services: Services) -> CheckResult:
	check: str = "github:release"
	slug: str = GITHUB_REPOSITORY
	if github_get(services, f"repos/{slug}") is None:
		return CheckResult(
			check,
			Status.FAIL,
			f"GitHub repository {slug} is not visible, so a missing release proves nothing",
			"set GITHUB_TOKEN to a token that can read the repository",
		)
	# This endpoint returns published releases only; see the module docstring.
	body: bytes | None = github_get(
		services,
		f"repos/{slug}/releases/tags/{urllib.parse.quote(release.tag, safe='')}",
	)
	if body is None:
		return CheckResult(
			check, Status.PASS, f"{slug} has no published release for {release.tag}"
		)
	document: dict[str, object] = json_object(body, f"release {release.tag}")
	assets: list[object] = typed_field(
		document, "assets", list, f"release {release.tag}"
	)
	names: list[str] = sorted(
		typed_field(as_object(asset, "release asset"), "name", str, "release asset")
		for asset in assets
	)
	if not names:
		return CheckResult(
			check,
			Status.PASS,
			f"{slug} has a published release for {release.tag} without assets",
		)
	return CheckResult(
		check,
		Status.FAIL,
		f"{slug} release {release.tag} already has assets {', '.join(names)}; "
		"registries and WinGet may have hashed them",
		"release a new version instead of re-running this one; delete the release "
		"only after confirming no channel consumed its assets",
	)


def github_checks(context: Context) -> list[CheckResult]:
	release: Release | None = context.release
	if release is None:
		return [skipped("github:release", "needs a releasable tag")]
	return [
		guarded(
			"github:release",
			GITHUB_REMEDY,
			lambda: check_github_release(release, context.services),
		)
	]


CHECK_GROUPS: tuple[tuple[str, CheckGroup], ...] = (
	("tag", tag_checks),
	("workspace crates", workspace_crate_checks),
	("crates.io", crates_checks),
	("PyPI", pypi_checks),
	("WinGet", winget_checks),
	("GitHub release", github_checks),
)
