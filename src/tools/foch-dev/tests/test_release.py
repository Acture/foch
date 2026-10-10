from __future__ import annotations

import contextlib
import hashlib
import io
import json
import random
import tarfile
import tempfile
import unittest
from collections.abc import Mapping, Sequence
from pathlib import Path

from foch_dev.contracts import CargoDependency, CargoPackage
from foch_dev.release import (
	CRATES_DOWNLOADS,
	CRATES_INDEX,
	GITHUB_API,
	PYPI_API,
	RETRY_DELAY_SECONDS,
	CheckResult,
	Context,
	IndexEntry,
	Response,
	Services,
	Status,
	Unavailable,
	UnexpectedResponse,
	attempt,
	check_superseded,
	check_tag,
	check_unpublished,
	crate_differences,
	crates_checks,
	get,
	github_checks,
	index_path,
	parse_index,
	preflight,
	pypi_checks,
	releasable_tag,
	report,
	semver_key,
	unpublished_crates,
	winget_checks,
	workspace_crate_checks,
)

GRAMMAR: str = "tree-sitter-paradox"
GRAMMAR_PATH: str = "src/packages/tree-sitter-paradox"
GRAMMAR_ROOT: str = f"{GRAMMAR}-0.3.0"
GRAMMAR_INDEX: str = f"{CRATES_INDEX}/tr/ee/{GRAMMAR}"
GRAMMAR_DOWNLOAD: str = f"{CRATES_DOWNLOADS}/{GRAMMAR}/{GRAMMAR_ROOT}.crate"
FOCH_INDEX: str = f"{CRATES_INDEX}/fo/ch/foch"
FOCH_CLI_INDEX: str = f"{CRATES_INDEX}/fo/ch/foch-cli"
PYPI_VERSION: str = f"{PYPI_API}/foch/0.0.1/json"
WINGET_VERSION: str = (
	f"{GITHUB_API}/repos/microsoft/winget-pkgs/contents/manifests/a/Acture/Foch/0.0.1"
)
FOCH_REPOSITORY: str = f"{GITHUB_API}/repos/Acture/foch"
FOCH_RELEASE: str = f"{FOCH_REPOSITORY}/releases/tags/v0.0.1"
NOT_FOUND: Response = Response(404, b'{"message": "Not Found"}')
# The internal libraries foch-cli links; crates.io has none of them yet.
LIBRARIES: tuple[str, ...] = ("foch-annotation", "foch-lsp", "foch-runner", "foch-test")
NEW_LIBRARY_INDEXES: dict[str, Response] = {
	f"{CRATES_INDEX}/fo/ch/{name}": NOT_FOUND for name in LIBRARIES
}

GRAMMAR_FILES: dict[str, bytes] = {
	".cargo_vcs_info.json": b'{"git": {"sha1": "c0e946a28f48176a9fc9"}}',
	"Cargo.toml": b"# normalized by cargo\n",
	"Cargo.toml.orig": b'[package]\nname = "tree-sitter-paradox"\n',
	"Cargo.lock": b"version = 4\n",
	"src/parser.c": b"int parse(void);\n",
	"LICENSE": b"AGPL\n",
}


def ok(body: bytes | str) -> Response:
	return Response(200, body.encode() if isinstance(body, str) else body)


class FakeFetch:
	"""Serves fixed responses per URL; an unexpected URL fails the test."""

	def __init__(self, routes: Mapping[str, Response | Sequence[Response]]) -> None:
		self.routes: dict[str, list[Response]] = {
			url: [answer] if isinstance(answer, Response) else list(answer)
			for url, answer in routes.items()
		}
		self.calls: list[tuple[str, dict[str, str]]] = []

	def __call__(self, url: str, headers: Mapping[str, str]) -> Response:
		self.calls.append((url, dict(headers)))
		answers: list[Response] = self.routes[url]
		return answers.pop(0) if len(answers) > 1 else answers[0]


def crate_archive(root: str, files: Mapping[str, bytes]) -> bytes:
	buffer = io.BytesIO()
	with tarfile.open(fileobj=buffer, mode="w:gz") as crate:
		for name, content in files.items():
			info = tarfile.TarInfo(f"{root}/{name}")
			info.size = len(content)
			crate.addfile(info, io.BytesIO(content))
	return buffer.getvalue()


def index_body(*entries: tuple[str, bool, str]) -> bytes:
	return "".join(
		json.dumps({"name": "x", "vers": version, "yanked": yanked, "cksum": cksum})
		+ "\n"
		for version, yanked, cksum in entries
	).encode()


def write_repo(root: Path, *, version: str = "0.0.1") -> Path:
	(root / "Cargo.toml").write_text(
		f'[workspace.package]\nversion = "{version}"\n', encoding="utf-8"
	)
	return root


def workspace_packages(repo: Path, *, pin: str = "=0.3.0") -> list[CargoPackage]:
	"""foch-cli -> its libraries -> foch -> tree-sitter-paradox, as cargo lists them."""

	def package(
		name: str, directory: str, version: str, *dependencies: CargoDependency
	) -> CargoPackage:
		return CargoPackage(
			name=name,
			version=version,
			publish=None,
			manifest_path=str(repo / directory / "Cargo.toml"),
			repository=(
				"https://github.com/acture/tree-sitter-paradox.git"
				if name == GRAMMAR
				else "https://github.com/Acture/foch"
			),
			targets=[],
			dependencies=list(dependencies),
		)

	def path_dependency(name: str, directory: str, req: str) -> CargoDependency:
		return CargoDependency(
			name=name,
			kind=None,
			target=None,
			features=[],
			req=req,
			path=str(repo / directory),
		)

	foch: CargoDependency = path_dependency("foch", "src/packages/foch", "=0.0.1")
	return [
		package(
			"foch-cli",
			"src/apps/foch-cli",
			"0.0.1",
			foch,
			*(
				path_dependency(name, f"src/packages/{name}", "=0.0.1")
				for name in LIBRARIES
			),
		),
		*(package(name, f"src/packages/{name}", "0.0.1", foch) for name in LIBRARIES),
		package(
			"foch",
			"src/packages/foch",
			"0.0.1",
			path_dependency(GRAMMAR, GRAMMAR_PATH, pin),
		),
		package(GRAMMAR, GRAMMAR_PATH, "0.3.0"),
	]


def services(
	fetch: FakeFetch,
	*,
	packages: list[CargoPackage] | None = None,
	local_crate: bytes = b"",
	token: str | None = None,
	sleeps: list[float] | None = None,
) -> Services:
	recorded: list[float] = [] if sleeps is None else sleeps
	return Services(
		fetch=fetch,
		packages=lambda: [] if packages is None else packages,
		package_crate=lambda name, version: local_crate,
		sleep=recorded.append,
		github_token=token,
	)


def context(
	repo: Path,
	fetch: FakeFetch,
	*,
	tag: str = "v0.0.1",
	local_crate: bytes = b"",
	packages: list[CargoPackage] | Unavailable | None = None,
) -> Context:
	return Context(
		repo,
		tag,
		releasable_tag(tag),
		workspace_packages(repo) if packages is None else packages,
		services(fetch, local_crate=local_crate),
	)


def statuses(results: list[CheckResult]) -> dict[str, Status]:
	return {result.check: result.status for result in results}


def grammar_routes(published: bytes) -> dict[str, Response]:
	checksum: str = hashlib.sha256(published).hexdigest()
	return {
		GRAMMAR_INDEX: ok(
			index_body(("0.2.0", False, "aa"), ("0.3.0", False, checksum))
		),
		GRAMMAR_DOWNLOAD: ok(published),
	}


def passing_routes() -> dict[str, Response]:
	return {
		**grammar_routes(crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES)),
		FOCH_INDEX: ok(index_body(("0.1.0", True, "bb"))),
		FOCH_CLI_INDEX: NOT_FOUND,
		**NEW_LIBRARY_INDEXES,
		PYPI_VERSION: NOT_FOUND,
		WINGET_VERSION: NOT_FOUND,
		FOCH_REPOSITORY: ok("{}"),
		FOCH_RELEASE: NOT_FOUND,
	}


class SemverTests(unittest.TestCase):
	def test_precedence_follows_semver(self) -> None:
		ordered: list[str] = [
			"0.0.1",
			"0.1.0-alpha",
			"0.1.0-alpha.1",
			"0.1.0-alpha.beta",
			"0.1.0-beta",
			"0.1.0-beta.2",
			"0.1.0-beta.11",
			"0.1.0-rc.1",
			"0.1.0",
			"0.1.1",
			"1.0.0",
		]
		shuffled: list[str] = list(ordered)
		random.Random(7).shuffle(shuffled)
		self.assertEqual(sorted(shuffled, key=semver_key), ordered)

	def test_build_metadata_does_not_affect_precedence(self) -> None:
		self.assertEqual(semver_key("1.0.0+build.5"), semver_key("1.0.0"))
		self.assertGreater(semver_key("1.0.0"), semver_key("1.0.0-rc.1+build"))

	def test_rejects_non_semver(self) -> None:
		for version in ("1.0", "01.0.0", "1.0.0-01", "1.0.0-", "v1.0.0", "1.0.0+"):
			with self.subTest(version=version), self.assertRaises(ValueError):
				semver_key(version)


class HttpTests(unittest.TestCase):
	url: str = "https://example.test/x"

	def test_not_found_is_absence(self) -> None:
		self.assertIsNone(get(services(FakeFetch({self.url: NOT_FOUND})), self.url))

	def test_server_errors_are_retried_once(self) -> None:
		sleeps: list[float] = []
		fetch = FakeFetch({self.url: [Response(503, b""), ok("body")]})
		self.assertEqual(get(services(fetch, sleeps=sleeps), self.url), b"body")
		self.assertEqual(sleeps, [RETRY_DELAY_SECONDS])
		self.assertEqual(len(fetch.calls), 2)

	def test_a_second_server_error_fails(self) -> None:
		fetch = FakeFetch({self.url: [Response(502, b"bad"), Response(502, b"bad")]})
		with self.assertRaisesRegex(UnexpectedResponse, "HTTP 502"):
			get(services(fetch), self.url)
		self.assertEqual(len(fetch.calls), 2)

	def test_other_errors_fail_without_retry(self) -> None:
		fetch = FakeFetch({self.url: Response(403, b"rate limit exceeded")})
		with self.assertRaisesRegex(UnexpectedResponse, "HTTP 403: rate limit"):
			get(services(fetch), self.url)
		self.assertEqual(len(fetch.calls), 1)


class TagTests(unittest.TestCase):
	def test_tag_matching_the_workspace_passes(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory), version="0.0.1-rc.2")
			result: CheckResult = check_tag(repo, "v0.0.1-rc.2")
		self.assertIs(result.status, Status.PASS)
		self.assertIn("0.0.1rc2 on PyPI", result.detail)

	def test_mismatched_or_unreleasable_tags_fail(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			for tag, detail in (
				("v0.0.2", "workspace version is 0.0.1"),
				("0.0.1", "v-prefixed"),
				("v0.0.1+build", "must be X.Y.Z"),
			):
				with self.subTest(tag=tag):
					result: CheckResult = check_tag(repo, tag)
					self.assertIs(result.status, Status.FAIL)
					self.assertIn(detail, result.detail)
					self.assertTrue(result.remedy)

	def test_unreleasable_workspace_version_fails(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory), version="0.0.1-dev.1")
			result: CheckResult = check_tag(repo, "v0.0.1")
		self.assertIs(result.status, Status.FAIL)
		self.assertIn("[workspace.package] version", result.remedy)


class WorkspaceCrateTests(unittest.TestCase):
	def checks(
		self,
		routes: Mapping[str, Response],
		local: Mapping[str, bytes] = GRAMMAR_FILES,
		pin: str = "=0.3.0",
	) -> list[CheckResult]:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			return workspace_crate_checks(
				context(
					repo,
					FakeFetch(routes),
					local_crate=crate_archive(GRAMMAR_ROOT, local),
					packages=workspace_packages(repo, pin=pin),
				)
			)

	def test_identical_release_passes(self) -> None:
		local: dict[str, bytes] = {
			**GRAMMAR_FILES,
			".cargo_vcs_info.json": b'{"git": {"sha1": "other"}}',
			"Cargo.toml": b"# normalized by another cargo\n",
			"Cargo.lock": b"version = 4\n# newer cc\n",
		}
		published: bytes = crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES)
		results = self.checks(grammar_routes(published), local)
		self.assertEqual(
			statuses(results),
			{
				"crates:closure": Status.PASS,
				f"{GRAMMAR}:crates.io": Status.PASS,
				f"{GRAMMAR}:content": Status.PASS,
			},
		)
		self.assertIn("3 files", results[2].detail)
		self.assertIn(GRAMMAR_PATH, results[2].detail)

	def test_content_differences_fail(self) -> None:
		local: dict[str, bytes] = {
			**GRAMMAR_FILES,
			"src/parser.c": b"int parse(int);\n",
			"src/scanner.c": b"",
		}
		published: bytes = crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES)
		content: CheckResult = self.checks(grammar_routes(published), local)[2]
		self.assertIs(content.status, Status.FAIL)
		self.assertIn("only in the checkout: src/scanner.c", content.detail)
		self.assertIn("different bytes: src/parser.c", content.detail)
		self.assertIn("commit c0e946a28f48", content.detail)
		self.assertIn(
			f"move {GRAMMAR_PATH} to the commit crates.io 0.3.0", content.remedy
		)

	def test_a_download_not_matching_the_index_fails(self) -> None:
		routes: dict[str, Response] = grammar_routes(
			crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES)
		)
		routes[GRAMMAR_DOWNLOAD] = ok(crate_archive(GRAMMAR_ROOT, {"LICENSE": b"x"}))
		content: CheckResult = self.checks(routes)[2]
		self.assertIs(content.status, Status.FAIL)
		self.assertIn("but https://static.crates.io", content.detail)
		self.assertIn("publish nothing", content.remedy)
		routes[GRAMMAR_DOWNLOAD] = NOT_FOUND
		self.assertIn("is missing", self.checks(routes)[2].detail)

	def test_a_missing_release_fails_and_skips_the_comparison(self) -> None:
		for index, state in (
			(NOT_FOUND, "has no tree-sitter-paradox crate"),
			(ok(index_body(("0.2.0", False, "aa"))), "(it has 0.2.0)"),
			(ok(index_body(("0.3.0", True, "aa"))), "0.3.0 only yanked"),
		):
			with self.subTest(state=state):
				results = self.checks({GRAMMAR_INDEX: index})
				self.assertEqual(
					[result.status for result in results],
					[Status.PASS, Status.FAIL, Status.SKIP],
				)
				self.assertIn(state, results[1].detail)
				self.assertIn("release tree-sitter-paradox 0.3.0", results[1].remedy)
				self.assertIn("acture/tree-sitter-paradox", results[1].remedy)

	def test_a_loose_or_stale_pin_fails_the_closure(self) -> None:
		routes: dict[str, Response] = {GRAMMAR_INDEX: NOT_FOUND}
		for pin in ("0.3", "=0.2.0", "*"):
			with self.subTest(pin=pin):
				result: CheckResult = self.checks(routes, pin=pin)[0]
				self.assertIs(result.status, Status.FAIL)
				self.assertIn(f"requires {pin!r}, expected '=0.3.0'", result.detail)

	def test_an_index_error_fails_closed(self) -> None:
		results = self.checks({GRAMMAR_INDEX: Response(403, b"denied")})
		self.assertEqual(
			[result.status for result in results],
			[Status.PASS, Status.FAIL, Status.SKIP],
		)
		self.assertIn("HTTP 403", results[1].detail)

	def test_unreadable_workspace_metadata_fails_closed(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			results = workspace_crate_checks(
				context(
					write_repo(Path(directory)),
					FakeFetch({}),
					packages=Unavailable("cargo metadata exited 101"),
				)
			)
		self.assertEqual(
			[(result.check, result.status) for result in results],
			[
				("crates:closure", Status.FAIL),
				(f"{GRAMMAR}:crates.io", Status.SKIP),
				(f"{GRAMMAR}:content", Status.SKIP),
			],
		)
		self.assertIn("cargo metadata exited 101", results[0].detail)

	def test_attempt_keeps_unexpected_errors_loud(self) -> None:
		def broken() -> None:
			raise KeyError("bug")

		with self.assertRaises(KeyError):
			attempt(broken)


class CrateComparisonTests(unittest.TestCase):
	def test_ignores_only_packaging_artifacts(self) -> None:
		local: dict[str, bytes] = {"Cargo.lock": b"1", "Cargo.toml": b"1", "a": b"1"}
		published: dict[str, bytes] = {"Cargo.lock": b"2", "Cargo.toml.orig": b"m"}
		self.assertEqual(
			crate_differences(local, published),
			["only in the checkout: a", "only on crates.io: Cargo.toml.orig"],
		)


class CratesRegistryTests(unittest.TestCase):
	def entries(self, *entries: tuple[str, bool]) -> tuple[IndexEntry, ...]:
		return parse_index(index_body(*((v, y, "cc") for v, y in entries)), "foch")

	def test_index_path_follows_cargo(self) -> None:
		self.assertEqual(index_path("a"), "1/a")
		self.assertEqual(index_path("ab"), "2/ab")
		self.assertEqual(index_path("abc"), "3/a/abc")
		self.assertEqual(index_path("Foch-CLI"), "fo/ch/foch-cli")

	def test_parse_index_rejects_malformed_entries(self) -> None:
		with self.assertRaisesRegex(ValueError, "'yanked' must be a bool"):
			parse_index(b'{"vers": "1.0.0", "yanked": "no", "cksum": "a"}\n', "foch")

	def test_unpublished_version_passes(self) -> None:
		result = check_unpublished("foch", "0.0.1", self.entries(("0.1.0", False)))
		self.assertIs(result.status, Status.PASS)
		self.assertEqual(result.detail, "crates.io has no foch 0.0.1")

	def test_a_crate_crates_io_lacks_passes_with_its_first_publish_note(
		self,
	) -> None:
		result = check_unpublished("foch-lsp", "0.0.1", None)
		self.assertIs(result.status, Status.PASS)
		self.assertIn("no foch-lsp crate yet", result.detail)
		self.assertIn("first publish needs an API token", result.detail)

	def test_a_taken_version_fails_even_yanked_or_with_build_metadata(self) -> None:
		for entries in (
			self.entries(("0.0.1", False)),
			self.entries(("0.0.1", True)),
			self.entries(("0.0.1+build.1", False)),
		):
			with self.subTest(entries=entries):
				result = check_unpublished("foch", "0.0.1", entries)
				self.assertIs(result.status, Status.FAIL)
				self.assertIn("never accepts a version twice", result.detail)

	def test_the_superseded_product_must_be_yanked_whatever_the_release(
		self,
	) -> None:
		entries = self.entries(("0.1.0", False), ("0.0.2", False))
		result = check_superseded("foch", entries)
		self.assertIs(result.status, Status.FAIL)
		self.assertIn("foch 0.1.0 unyanked", result.detail)
		self.assertIn("`cargo install foch` can install it", result.detail)
		self.assertEqual(result.remedy, "cargo yank --version 0.1.0 foch")

	def test_a_yanked_or_absent_superseded_product_passes(self) -> None:
		for entries in (
			self.entries(("0.1.0", True)),
			self.entries(("0.1.0", True), ("0.0.2", False), ("0.2.0-alpha.1", False)),
			None,
		):
			with self.subTest(entries=entries):
				self.assertIs(check_superseded("foch", entries).status, Status.PASS)

	def test_a_prerelease_tag_still_requires_the_yank(self) -> None:
		# `cargo install foch` never selects a pre-release, so 0.1.0 would win.
		for tag in ("v0.2.0-alpha.1", "v0.1.1-rc.1", "v0.0.1-rc.1"):
			with self.subTest(tag=tag), tempfile.TemporaryDirectory() as directory:
				repo: Path = write_repo(Path(directory), version=tag.removeprefix("v"))
				fetch = FakeFetch(
					{
						FOCH_INDEX: ok(index_body(("0.1.0", False, "bb"))),
						FOCH_CLI_INDEX: NOT_FOUND,
						**NEW_LIBRARY_INDEXES,
					}
				)
				results = crates_checks(context(repo, fetch, tag=tag))
				self.assertIs(statuses(results)["foch:superseded"], Status.FAIL)

	def test_every_published_crate_is_checked(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			fetch = FakeFetch(
				{
					FOCH_INDEX: ok(index_body(("0.1.0", True, "bb"))),
					FOCH_CLI_INDEX: NOT_FOUND,
					**NEW_LIBRARY_INDEXES,
					f"{CRATES_INDEX}/fo/ch/foch-test": ok(
						index_body(("0.0.1", False, "cc"))
					),
				}
			)
			results = crates_checks(context(repo, fetch))
		self.assertEqual(
			[(result.check, result.status) for result in results],
			[
				("foch:unpublished", Status.PASS),
				("foch:superseded", Status.PASS),
				("foch-annotation:unpublished", Status.PASS),
				("foch-cli:unpublished", Status.PASS),
				("foch-lsp:unpublished", Status.PASS),
				("foch-runner:unpublished", Status.PASS),
				("foch-test:unpublished", Status.FAIL),
			],
		)

	def test_unreadable_workspace_metadata_skips_the_crate_checks(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			results = crates_checks(
				context(
					write_repo(Path(directory)),
					FakeFetch({}),
					packages=Unavailable("cargo metadata exited 101"),
				)
			)
		self.assertEqual(
			[(result.check, result.status) for result in results],
			[("crates:unpublished", Status.SKIP)],
		)


class UnpublishedCratesTests(unittest.TestCase):
	def test_lists_the_published_crates_lacking_their_exact_version(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			fetch = FakeFetch(
				{
					# The superseded product and an exact match: foch is done.
					FOCH_INDEX: ok(
						index_body(("0.1.0", True, "a"), ("0.0.1", False, "b"))
					),
					FOCH_CLI_INDEX: NOT_FOUND,
					**NEW_LIBRARY_INDEXES,
					# Yanked still exists; cargo would refuse to publish it again.
					f"{CRATES_INDEX}/fo/ch/foch-annotation": ok(
						index_body(("0.0.1", True, "c"))
					),
					# Only an exact match counts; cargo publish fails loudly on
					# a build-metadata variant, which the preflight refused.
					f"{CRATES_INDEX}/fo/ch/foch-test": ok(
						index_body(("0.0.1+other", False, "d"))
					),
				}
			)
			missing = unpublished_crates(
				workspace_packages(repo), services(fetch), allow_new=True
			)
		self.assertEqual(missing, ["foch-cli", "foch-lsp", "foch-runner", "foch-test"])

	def test_a_crate_crates_io_lacks_fails_unless_allowed(self) -> None:
		# The release job's Trusted Publishing token cannot create them, so it
		# must stop before uploading foch, which crates.io has.
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			fetch = FakeFetch(
				{
					FOCH_INDEX: ok(index_body(("0.1.0", True, "a"))),
					FOCH_CLI_INDEX: NOT_FOUND,
					**NEW_LIBRARY_INDEXES,
				}
			)
			with self.assertRaisesRegex(
				ValueError,
				"crates.io has no foch-annotation, foch-cli, foch-lsp, foch-runner, "
				"foch-test crate yet",
			):
				unpublished_crates(
					workspace_packages(repo), services(fetch), allow_new=False
				)

	def test_a_registry_error_raises(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			fetch = FakeFetch(
				{
					FOCH_INDEX: Response(403, b"denied"),
					FOCH_CLI_INDEX: NOT_FOUND,
					**NEW_LIBRARY_INDEXES,
				}
			)
			with self.assertRaisesRegex(UnexpectedResponse, "HTTP 403"):
				unpublished_crates(
					workspace_packages(repo), services(fetch), allow_new=True
				)


class PypiTests(unittest.TestCase):
	def checks(self, routes: Mapping[str, Response]) -> list[CheckResult]:
		with tempfile.TemporaryDirectory() as directory:
			return pypi_checks(context(write_repo(Path(directory)), FakeFetch(routes)))

	def test_a_new_version_passes(self) -> None:
		results = self.checks({PYPI_VERSION: NOT_FOUND})
		self.assertEqual(statuses(results), {"pypi:version": Status.PASS})

	def test_an_existing_version_fails(self) -> None:
		results = self.checks({PYPI_VERSION: ok("{}")})
		self.assertIs(results[0].status, Status.FAIL)

	def test_prerelease_uses_the_pep440_spelling(self) -> None:
		fetch = FakeFetch({f"{PYPI_API}/foch/0.0.1rc1/json": NOT_FOUND})
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory), version="0.0.1-rc.1")
			results = pypi_checks(context(repo, fetch, tag="v0.0.1-rc.1"))
		self.assertIs(results[0].status, Status.PASS)


class WingetTests(unittest.TestCase):
	def checks(self, routes: Mapping[str, Response]) -> list[CheckResult]:
		with tempfile.TemporaryDirectory() as directory:
			return winget_checks(
				context(write_repo(Path(directory)), FakeFetch(routes))
			)

	def test_an_absent_manifest_passes(self) -> None:
		results = self.checks({WINGET_VERSION: NOT_FOUND})
		self.assertEqual(statuses(results), {"winget:version": Status.PASS})

	def test_a_published_version_fails(self) -> None:
		results = self.checks({WINGET_VERSION: ok("[]")})
		self.assertIs(results[0].status, Status.FAIL)

	def test_api_errors_fail_distinctly_from_absence(self) -> None:
		denied: Response = Response(403, b'{"message": "API rate limit exceeded"}')
		results = self.checks({WINGET_VERSION: denied})
		self.assertIs(results[0].status, Status.FAIL)
		self.assertIn("HTTP 403", results[0].detail)
		self.assertIn("GITHUB_TOKEN", results[0].remedy)


class GithubReleaseTests(unittest.TestCase):
	def check(self, routes: Mapping[str, Response]) -> CheckResult:
		with tempfile.TemporaryDirectory() as directory:
			return github_checks(
				context(write_repo(Path(directory)), FakeFetch(routes))
			)[0]

	def test_no_release_or_an_empty_one_passes(self) -> None:
		for release in (NOT_FOUND, ok(json.dumps({"assets": []}))):
			with self.subTest(release=release):
				result = self.check({FOCH_REPOSITORY: ok("{}"), FOCH_RELEASE: release})
				self.assertIs(result.status, Status.PASS)
				self.assertIn("published release", result.detail)

	def test_a_release_with_assets_fails(self) -> None:
		assets: str = json.dumps({"assets": [{"name": "b.zip"}, {"name": "a.tar.gz"}]})
		result = self.check({FOCH_REPOSITORY: ok("{}"), FOCH_RELEASE: ok(assets)})
		self.assertIs(result.status, Status.FAIL)
		self.assertIn("assets a.tar.gz, b.zip", result.detail)

	def test_an_invisible_repository_fails(self) -> None:
		result = self.check({FOCH_REPOSITORY: NOT_FOUND})
		self.assertIs(result.status, Status.FAIL)
		self.assertIn("not visible", result.detail)


class PreflightTests(unittest.TestCase):
	def test_every_check_passes_in_a_fixed_order(self) -> None:
		fetch = FakeFetch(passing_routes())
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			results = preflight(
				repo,
				"v0.0.1",
				services(
					fetch,
					packages=workspace_packages(repo),
					local_crate=crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES),
					token="secret",
				),
			)
		self.assertEqual(
			[(result.check, result.status) for result in results],
			[
				("tag", Status.PASS),
				("crates:closure", Status.PASS),
				(f"{GRAMMAR}:crates.io", Status.PASS),
				(f"{GRAMMAR}:content", Status.PASS),
				("foch:unpublished", Status.PASS),
				("foch:superseded", Status.PASS),
				("foch-annotation:unpublished", Status.PASS),
				("foch-cli:unpublished", Status.PASS),
				("foch-lsp:unpublished", Status.PASS),
				("foch-runner:unpublished", Status.PASS),
				("foch-test:unpublished", Status.PASS),
				("pypi:version", Status.PASS),
				("winget:version", Status.PASS),
				("github:release", Status.PASS),
			],
		)
		for url, headers in fetch.calls:
			with self.subTest(url=url):
				self.assertEqual(
					"Authorization" in headers, url.startswith(f"{GITHUB_API}/")
				)
		output = io.StringIO()
		with contextlib.redirect_stdout(output):
			self.assertEqual(report("v0.0.1", results), 0)
		self.assertIn("preflight v0.0.1: all 14 checks passed", output.getvalue())

	def test_an_unreleasable_tag_skips_the_registry_checks(self) -> None:
		routes: dict[str, Response] = grammar_routes(
			crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES)
		)
		with tempfile.TemporaryDirectory() as directory:
			repo: Path = write_repo(Path(directory))
			results = preflight(
				repo,
				"release-1",
				services(
					FakeFetch(routes),
					packages=workspace_packages(repo),
					local_crate=crate_archive(GRAMMAR_ROOT, GRAMMAR_FILES),
				),
			)
		found: dict[str, Status] = statuses(results)
		self.assertIs(found["tag"], Status.FAIL)
		self.assertIs(found[f"{GRAMMAR}:content"], Status.PASS)
		for check in (
			"foch:unpublished",
			"foch:superseded",
			*(f"{name}:unpublished" for name in ("foch-cli", *LIBRARIES)),
			"pypi:version",
			"winget:version",
			"github:release",
		):
			self.assertIs(found[check], Status.SKIP, check)
		output = io.StringIO()
		with contextlib.redirect_stdout(output):
			self.assertEqual(report("release-1", results), 1)
		lines: list[str] = output.getvalue().splitlines()
		self.assertEqual(len(lines), len(results) + 1)
		self.assertTrue(lines[0].startswith("FAIL tag "))
		self.assertIn("| remedy: tag vX.Y.Z", lines[0])


if __name__ == "__main__":
	unittest.main()
