from __future__ import annotations

import json
import re
import subprocess
import tomllib
from pathlib import Path
from typing import NotRequired, TypedDict, cast

from .dist import DISTRIBUTION
from .schema import SCHEMA_DIR
from .winget import LICENSE_EXPRESSION
from .winget import REPOSITORY_URL as WINGET_REPOSITORY_URL

CargoTarget = TypedDict(
	"CargoTarget",
	{
		"name": str,
		"kind": list[str],
		"required-features": list[str],
	},
)


class CargoDependency(TypedDict):
	features: list[str]
	kind: str | None
	name: str
	target: str | None
	req: str
	path: NotRequired[str]


class CargoPackage(TypedDict):
	name: str
	version: str
	publish: list[str] | None
	manifest_path: str
	repository: str | None
	targets: list[CargoTarget]
	dependencies: list[CargoDependency]


class CargoMetadata(TypedDict):
	packages: list[CargoPackage]


# The crate `cargo install foch-cli` installs. crates.io builds it from
# published crates alone, so a release publishes it and every workspace crate
# it builds from (`published_crates`).
INSTALLED_CRATE: str = "foch-cli"
EXPECTED_BINARIES: tuple[tuple[str, str], ...] = (
	(INSTALLED_CRATE, "foch"),
	("foch-desktop", "foch-desktop"),
)
EXPECTED_EXAMPLES: tuple[tuple[str, str, tuple[str, ...]], ...] = (
	(INSTALLED_CRATE, "parse_stats", ("dev-tools",)),
	(INSTALLED_CRATE, "symbol_dump", ("dev-tools",)),
)
# Workspace members released from their own repository, resolved from the
# registry by published crates like any third-party dependency.
EXTERNALLY_RELEASED_PACKAGES: frozenset[str] = frozenset({"tree-sitter-paradox"})
# Dependency kinds a registry build of a published crate resolves.
CLOSURE_DEPENDENCY_KINDS: frozenset[str | None] = frozenset({None, "build"})
SCHEMA_PACKAGE: str = "foch"
BUILD_SCHEMA_DIR: re.Pattern[str] = re.compile(
	r'\bconst\s+SCHEMA_DIR\s*:\s*&\s*str\s*=\s*"([^"]+)"\s*;'
)
FOCH_DEV_PYPROJECT: Path = Path("src/tools/foch-dev/pyproject.toml")
LIBRARY_MANIFEST: Path = Path("src/packages/foch/Cargo.toml")
REQUIRES_PYTHON_FLOOR: re.Pattern[str] = re.compile(r">=\s*([0-9]+\.[0-9]+)")
REQUIRED_DESKTOP_RUNTIME_CRATES: frozenset[str] = frozenset({"foch", "tauri"})
ALLOWED_DESKTOP_TAURI_PLUGIN_CRATES: frozenset[str] = frozenset()
ALLOWED_DESKTOP_PROCESS_HELPER_CRATES: frozenset[str] = frozenset()
RUST_PROCESS_HELPER_CRATES: frozenset[str] = frozenset(
	{
		"async-process",
		"command-group",
		"duct",
		"execute",
		"open",
		"opener",
		"process-control",
		"process-wrap",
		"run-script",
		"shared-child",
		"subprocess",
		"xshell",
	}
)
FRONTEND_DEPENDENCY_SECTIONS: tuple[str, ...] = (
	"dependencies",
	"devDependencies",
	"optionalDependencies",
	"peerDependencies",
)
FRONTEND_BUNDLED_DEPENDENCY_SECTIONS: tuple[str, ...] = (
	"bundleDependencies",
	"bundledDependencies",
)
ALLOWED_DESKTOP_FRONTEND_TAURI_PLUGINS: frozenset[str] = frozenset()
ALLOWED_DESKTOP_FRONTEND_PROCESS_HELPERS: frozenset[str] = frozenset()
FRONTEND_PROCESS_HELPERS: frozenset[str] = frozenset(
	{
		"child-process-promise",
		"cross-spawn",
		"execa",
		"foreground-child",
		"node-pty",
		"shelljs",
		"spawn-command",
		"tinyspawn",
		"zx",
	}
)
RUST_SOURCE_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
	(
		"standard-library process API",
		re.compile(r"\bstd\s*::(?:\s*\{[^{}]*\bprocess\b|\s*process\b)"),
	),
	("Tokio process API", re.compile(r"\btokio\s*::\s*process\b")),
	(
		"process-helper crate API",
		re.compile(
			r"\b(?:async_process|command_group|duct|process_control|process_wrap|"
			r"run_script|shared_child|subprocess|xshell)\s*::"
		),
	),
	("Tauri plugin API", re.compile(r"\btauri_plugin_[A-Za-z0-9_]+\b")),
	(
		"process command construction",
		re.compile(r"\bCommand\s*::\s*(?:new|new_sidecar)\s*\("),
	),
	("shell/process extension", re.compile(r"\b(?:ProcessExt|ShellExt)\b")),
	("sidecar command", re.compile(r"\.\s*sidecar\s*\(")),
	("embedded binary payload", re.compile(r"\binclude_bytes\s*!")),
)
FRONTEND_MODULE_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
	(
		"Tauri privileged plugin module",
		re.compile(r"[\"']@tauri-apps/(?:api/(?:process|shell)|plugin-[^\"']+)[\"']"),
	),
	(
		"Node process module",
		re.compile(r"[\"'](?:node:)?child_process[\"']"),
	),
	(
		"frontend process-helper module",
		re.compile(
			r"[\"'](?:child-process-promise|cross-spawn|execa|foreground-child|"
			r"node-pty|shelljs|spawn-command|tinyspawn|zx)[\"']"
		),
	),
)
FRONTEND_SOURCE_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
	("Tauri shell command", re.compile(r"\bCommand\s*\.\s*(?:create|sidecar)\s*\(")),
	(
		"Bun or Deno process API",
		re.compile(r"\b(?:Bun\s*\.\s*spawn|Deno\s*\.\s*Command)\b"),
	),
)


def custom_harness_targets(manifest_path: Path) -> list[str]:
	manifest = cast(
		dict[str, object],
		tomllib.loads(manifest_path.read_text(encoding="utf-8")),
	)
	targets: list[str] = []
	for section in ("test", "bench"):
		raw_targets = manifest.get(section, [])
		if not isinstance(raw_targets, list):
			continue
		for raw_target in raw_targets:
			if (
				not isinstance(raw_target, dict)
				or raw_target.get("harness") is not False
			):
				continue
			name = raw_target.get("name", "<unnamed>")
			targets.append(f"{manifest_path}:{section}:{name}")
	return targets


def json_object(path: Path) -> dict[str, object]:
	value = json.loads(path.read_text(encoding="utf-8"))
	if not isinstance(value, dict):
		raise ValueError(f"{path} must contain a JSON object")
	return cast(dict[str, object], value)


def canonical_package_name(name: str) -> str:
	return name.lower().replace("_", "-")


def dependency_description(dependency: CargoDependency) -> str:
	kind = dependency["kind"] or "normal"
	target = dependency["target"]
	if target is None:
		return f"{dependency['name']} ({kind})"
	return f"{dependency['name']} ({kind}, target={target})"


def verify_desktop_rust_dependencies(desktop: CargoPackage) -> None:
	dependencies = desktop["dependencies"]
	runtime_dependencies = {
		canonical_package_name(dependency["name"])
		for dependency in dependencies
		if dependency["kind"] is None and dependency["target"] is None
	}
	if not REQUIRED_DESKTOP_RUNTIME_CRATES.issubset(runtime_dependencies):
		raise ValueError(
			"foch-desktop must have unconditional runtime dependencies on "
			f"{sorted(REQUIRED_DESKTOP_RUNTIME_CRATES)}; found {sorted(runtime_dependencies)}"
		)

	forbidden: list[str] = []
	for dependency in dependencies:
		name = canonical_package_name(dependency["name"])
		if name == "foch-cli":
			forbidden.append(dependency_description(dependency))
		elif (
			name.startswith("tauri-plugin-")
			and name not in ALLOWED_DESKTOP_TAURI_PLUGIN_CRATES
		):
			forbidden.append(dependency_description(dependency))
		elif (
			name in RUST_PROCESS_HELPER_CRATES
			and name not in ALLOWED_DESKTOP_PROCESS_HELPER_CRATES
		):
			forbidden.append(dependency_description(dependency))
		elif name == "tokio" and "process" in dependency["features"]:
			forbidden.append(f"{dependency_description(dependency)}, feature=process")
	if forbidden:
		raise ValueError(
			"foch-desktop APP-001 has forbidden Rust dependencies: "
			+ ", ".join(sorted(forbidden))
		)


def frontend_dependency_names(package: dict[str, object], path: Path) -> set[str]:
	dependencies: set[str] = set()
	for section in FRONTEND_DEPENDENCY_SECTIONS:
		raw_dependencies = package.get(section, {})
		if not isinstance(raw_dependencies, dict):
			raise ValueError(f"{path}:{section} must be an object")
		dependencies.update(str(name) for name in raw_dependencies)
	for section in FRONTEND_BUNDLED_DEPENDENCY_SECTIONS:
		raw_dependencies = package.get(section, [])
		if not isinstance(raw_dependencies, list) or not all(
			isinstance(name, str) for name in raw_dependencies
		):
			raise ValueError(f"{path}:{section} must be a string array")
		dependencies.update(cast(list[str], raw_dependencies))
	return dependencies


def verify_desktop_frontend_dependencies(desktop_root: Path) -> None:
	package_path = desktop_root / "package.json"
	dependencies = frontend_dependency_names(json_object(package_path), package_path)
	forbidden = sorted(
		name
		for name in dependencies
		if (
			name.startswith("@tauri-apps/plugin-")
			and name not in ALLOWED_DESKTOP_FRONTEND_TAURI_PLUGINS
		)
		or (
			name in FRONTEND_PROCESS_HELPERS
			and name not in ALLOWED_DESKTOP_FRONTEND_PROCESS_HELPERS
		)
		or name in {"@tauri-apps/api/process", "@tauri-apps/api/shell"}
	)
	if forbidden:
		raise ValueError(
			"foch-desktop APP-001 has forbidden frontend dependencies: "
			+ ", ".join(forbidden)
		)


def tauri_config_paths(src_tauri: Path) -> list[Path]:
	paths = sorted(
		set(src_tauri.glob("tauri.conf.*")) | set(src_tauri.glob("tauri.*.conf.*"))
	)
	unsupported = [path for path in paths if path.suffix not in {".json", ".toml"}]
	if unsupported:
		raise ValueError(
			"desktop contract validator cannot inspect Tauri config files: "
			+ ", ".join(str(path) for path in unsupported)
		)
	return paths


def config_object(path: Path) -> dict[str, object]:
	if path.suffix == ".json":
		return json_object(path)
	value = tomllib.loads(path.read_text(encoding="utf-8"))
	return cast(dict[str, object], value)


def normalized_config_key(key: object) -> str:
	return re.sub(r"[-_]", "", str(key)).lower()


def verify_tauri_config(src_tauri: Path) -> None:
	base_config_path = src_tauri / "tauri.conf.json"
	if not base_config_path.is_file():
		raise ValueError("foch-desktop tauri.conf.json is missing")
	base_config = json_object(base_config_path)
	if base_config.get("identifier") != "dev.acture.foch":
		raise ValueError("foch-desktop bundle identifier must be dev.acture.foch")

	for path in tauri_config_paths(src_tauri):
		config = config_object(path)
		bundle = config.get("bundle", {})
		if not isinstance(bundle, dict):
			raise ValueError(f"{path}:bundle must be an object")
		forbidden_bundle_keys = sorted(
			str(key)
			for key in bundle
			if normalized_config_key(key) in {"externalbin", "resources"}
		)
		if forbidden_bundle_keys:
			raise ValueError(
				f"{path} must not bundle sidecars, CLI binaries, or resources; found "
				+ ", ".join(forbidden_bundle_keys)
			)

		plugins = config.get("plugins", {})
		if not isinstance(plugins, dict):
			raise ValueError(f"{path}:plugins must be an object")
		forbidden_plugins = sorted(
			str(name)
			for name in plugins
			if str(name) not in ALLOWED_DESKTOP_FRONTEND_TAURI_PLUGINS
		)
		if forbidden_plugins:
			raise ValueError(
				f"{path} must not configure Tauri plugins in APP-001; found "
				+ ", ".join(forbidden_plugins)
			)

		app = config.get("app", {})
		if not isinstance(app, dict):
			raise ValueError(f"{path}:app must be an object")
		security = app.get("security", {})
		if not isinstance(security, dict):
			raise ValueError(f"{path}:app.security must be an object")
		if security.get("capabilities", []) != []:
			raise ValueError(f"{path}: inline Tauri capabilities must remain empty")


def capability_object(path: Path) -> dict[str, object]:
	if path.suffix == ".json":
		return json_object(path)
	return cast(
		dict[str, object],
		tomllib.loads(path.read_text(encoding="utf-8")),
	)


def verify_tauri_capabilities(src_tauri: Path) -> None:
	capabilities_root = src_tauri / "capabilities"
	default_path = capabilities_root / "default.json"
	if not default_path.is_file():
		raise ValueError("foch-desktop default Tauri capability is missing")
	paths = sorted(
		set(capabilities_root.glob("*.json")) | set(capabilities_root.glob("*.toml"))
	)
	for path in paths:
		capability = capability_object(path)
		if capability.get("permissions") != []:
			raise ValueError(
				f"foch-desktop APP-001 capability must remain empty: {path}"
			)


def mask_source(source: str, language: str, *, mask_literals: bool) -> str:
	masked = list(source)
	index = 0
	length = len(source)

	def mask(start: int, end: int) -> None:
		for position in range(start, end):
			if source[position] not in "\r\n":
				masked[position] = " "

	while index < length:
		if source.startswith("//", index):
			end = source.find("\n", index + 2)
			end = length if end == -1 else end
			mask(index, end)
			index = end
			continue
		if source.startswith("/*", index):
			depth = 1
			end = index + 2
			while end < length and depth > 0:
				if source.startswith("/*", end):
					depth += 1
					end += 2
				elif source.startswith("*/", end):
					depth -= 1
					end += 2
				else:
					end += 1
			mask(index, end)
			index = end
			continue

		if language == "rust" and source[index] in {"b", "r"}:
			prefix_end = index
			if source.startswith("br", index):
				prefix_end += 2
			elif source[index] == "r":
				prefix_end += 1
			while prefix_end < length and source[prefix_end] == "#":
				prefix_end += 1
			if prefix_end < length and source[prefix_end] == '"':
				hashes = source[index:prefix_end].count("#")
				terminator = '"' + "#" * hashes
				end = source.find(terminator, prefix_end + 1)
				end = length if end == -1 else end + len(terminator)
				if mask_literals:
					mask(index, end)
				index = end
				continue

		quotes = {'"'} if language == "rust" else {'"', "'", "`"}
		if source[index] in quotes:
			quote = source[index]
			end = index + 1
			while end < length:
				if source[end] == "\\":
					end += 2
					continue
				end += 1
				if source[end - 1] == quote:
					break
			if mask_literals:
				mask(index, min(end, length))
			index = end
			continue
		index += 1
	return "".join(masked)


def mask_rust_test_modules(source: str) -> str:
	code = mask_source(source, "rust", mask_literals=True)
	masked = list(source)
	pattern = re.compile(
		r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*(?:pub\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{"
	)
	for match in pattern.finditer(code):
		opening = code.find("{", match.start(), match.end())
		depth = 1
		end = opening + 1
		while end < len(code) and depth > 0:
			if code[end] == "{":
				depth += 1
			elif code[end] == "}":
				depth -= 1
			end += 1
		for position in range(match.start(), end):
			if source[position] not in "\r\n":
				masked[position] = " "
	return "".join(masked)


def is_test_source(path: Path, source_root: Path) -> bool:
	relative = path.relative_to(source_root)
	if any(part.lower() in {"__tests__", "test", "tests"} for part in relative.parts):
		return True
	return any(marker in path.name.lower() for marker in (".spec.", ".test."))


def source_violations(desktop_root: Path) -> list[str]:
	violations: list[str] = []
	source_groups = (
		(desktop_root / "src-tauri" / "src", {".rs"}, "rust"),
		(desktop_root / "src", {".js", ".jsx", ".ts", ".tsx"}, "frontend"),
	)
	for source_root, suffixes, language in source_groups:
		for path in sorted(source_root.rglob("*")):
			if (
				not path.is_file()
				or path.suffix not in suffixes
				or is_test_source(path, source_root)
			):
				continue
			source = path.read_text(encoding="utf-8")
			if language == "rust":
				source = mask_rust_test_modules(source)
				code = mask_source(source, "rust", mask_literals=True)
				pattern_groups = ((code, RUST_SOURCE_PATTERNS),)
			else:
				comments_removed = mask_source(source, "frontend", mask_literals=False)
				code = mask_source(source, "frontend", mask_literals=True)
				pattern_groups = (
					(comments_removed, FRONTEND_MODULE_PATTERNS),
					(code, FRONTEND_SOURCE_PATTERNS),
				)
			for scanned_source, patterns in pattern_groups:
				for label, pattern in patterns:
					for match in pattern.finditer(scanned_source):
						line = scanned_source.count("\n", 0, match.start()) + 1
						relative = path.relative_to(desktop_root)
						violations.append(f"{relative}:{line}: {label}")
	return sorted(set(violations))


def verify_desktop_sources(desktop_root: Path) -> None:
	violations = source_violations(desktop_root)
	if violations:
		raise ValueError(
			"foch-desktop APP-001 source must not launch or bundle processes:\n"
			+ "\n".join(violations)
		)


def verify_desktop_contract(repo_root: Path, packages: list[CargoPackage]) -> None:
	desktop = next(
		(package for package in packages if package["name"] == "foch-desktop"),
		None,
	)
	if desktop is None:
		raise ValueError("foch-desktop Cargo package is missing")
	desktop_root: Path = repo_root / "src" / "apps" / "foch-desktop"
	verify_desktop_rust_dependencies(desktop)
	verify_desktop_frontend_dependencies(desktop_root)
	verify_tauri_config(desktop_root / "src-tauri")
	verify_tauri_capabilities(desktop_root / "src-tauri")
	verify_desktop_sources(desktop_root)


def is_publishable(package: CargoPackage) -> bool:
	return package["publish"] != []


def packages_by_directory(packages: list[CargoPackage]) -> dict[Path, CargoPackage]:
	return {
		Path(package["manifest_path"]).parent.resolve(): package for package in packages
	}


def published_crates(packages: list[CargoPackage]) -> tuple[str, ...]:
	"""The workspace crates a release publishes to crates.io, sorted by name.

	They are INSTALLED_CRATE and every workspace package its normal and build
	dependencies reach, except the externally released ones, which published
	crates resolve from the registry like any third-party crate.
	"""
	by_name: dict[str, CargoPackage] = {
		package["name"]: package for package in packages
	}
	if INSTALLED_CRATE not in by_name:
		raise ValueError(f"the workspace has no {INSTALLED_CRATE} package")
	by_directory: dict[Path, CargoPackage] = packages_by_directory(packages)
	reached: set[str] = set()
	pending: list[str] = [INSTALLED_CRATE]
	while pending:
		name: str = pending.pop()
		if name in reached or name in EXTERNALLY_RELEASED_PACKAGES:
			continue
		reached.add(name)
		for dependency in by_name[name]["dependencies"]:
			path: str | None = dependency.get("path")
			if path is None or dependency["kind"] not in CLOSURE_DEPENDENCY_KINDS:
				continue
			target: CargoPackage | None = by_directory.get(Path(path).resolve())
			if target is not None:
				pending.append(target["name"])
	return tuple(sorted(reached))


def publishable_closure_problems(packages: list[CargoPackage]) -> list[str]:
	"""Why the published crates would not build from registry artifacts alone.

	Exactly the `published_crates` are publishable. Each path dependency a
	registry build resolves must pin exactly the version of its path package,
	so the published closure is the one built here, and no published crate may
	depend on a package that is never published.
	"""
	if not any(package["name"] == INSTALLED_CRATE for package in packages):
		return [f"the workspace has no {INSTALLED_CRATE} package"]
	published: tuple[str, ...] = published_crates(packages)
	ordered: list[CargoPackage] = sorted(packages, key=lambda package: package["name"])
	violations: list[str] = [
		*(
			f"{package['name']} is published, but its manifest sets publish = false"
			for package in ordered
			if package["name"] in published and not is_publishable(package)
		),
		*(
			f"{package['name']} is publishable, but {INSTALLED_CRATE} does not build "
			"from it"
			for package in ordered
			if is_publishable(package)
			and package["name"] not in published
			and package["name"] not in EXTERNALLY_RELEASED_PACKAGES
		),
	]
	by_directory: dict[Path, CargoPackage] = packages_by_directory(packages)
	for package in packages:
		if package["name"] not in published:
			continue
		for dependency in package["dependencies"]:
			path = dependency.get("path")
			if path is None:
				continue
			edge = f"{package['name']} -> {dependency_description(dependency)}"
			target = by_directory.get(Path(path).resolve())
			if target is None:
				violations.append(f"{edge}: {path} is not a workspace package")
			# A published target that sets publish = false is reported above.
			elif target["name"] not in published and not is_publishable(target):
				violations.append(f"{edge}: {target['name']} is never published")
			elif (
				dependency["kind"] in CLOSURE_DEPENDENCY_KINDS
				and dependency["req"] != f"={target['version']}"
			):
				violations.append(
					f"{edge}: requires {dependency['req']!r}, "
					f"expected '={target['version']}'"
				)
	return violations


def verify_publishable_closure(packages: list[CargoPackage]) -> None:
	problems: list[str] = publishable_closure_problems(packages)
	if problems:
		raise ValueError(
			"publishable crates must resolve from registry artifacts:\n"
			+ "\n".join(problems)
		)


def verify_schema_in_package(repo_root: Path, packages: list[CargoPackage]) -> None:
	"""The CWT input `build.rs` compiles must ship inside the `foch` crate."""
	package = next(
		(package for package in packages if package["name"] == SCHEMA_PACKAGE),
		None,
	)
	if package is None:
		raise ValueError(f"{SCHEMA_PACKAGE} Cargo package is missing")
	package_root = Path(package["manifest_path"]).parent.resolve()
	build_script = package_root / "build.rs"
	source = mask_source(
		build_script.read_text(encoding="utf-8"), "rust", mask_literals=False
	)
	match = BUILD_SCHEMA_DIR.search(source)
	if match is None:
		raise ValueError(f"{build_script} must name its CWT input as SCHEMA_DIR")
	schema_dir = (package_root / match.group(1)).resolve()
	if not schema_dir.is_relative_to(package_root):
		raise ValueError(
			f"{build_script} compiles {schema_dir}, outside the {SCHEMA_PACKAGE} "
			f"package {package_root}; its crate would not carry the schema"
		)
	expected = (repo_root / SCHEMA_DIR).resolve()
	if schema_dir != expected:
		raise ValueError(
			f"{build_script} compiles {schema_dir}, but foch_dev hashes {expected}"
		)
	if not schema_dir.is_dir():
		raise ValueError(f"CWT schema directory is missing: {schema_dir}")


def toml_value(path: Path, *keys: str) -> object:
	"""The value at `keys` in a TOML file, or None if a table is missing."""
	value: object = tomllib.loads(path.read_text(encoding="utf-8"))
	for key in keys:
		if not isinstance(value, dict):
			return None
		value = cast(dict[str, object], value).get(key)
	return value


def distribution_problems(repo_root: Path) -> list[str]:
	"""Release identities that several files spell, and where they disagree.

	The PyPI project, the WinGet manifests and the `foch` crate describe one
	binary, so they share its name, repository and license expression. The
	root pyproject also sets the Python version ty checks foch-dev at, which
	must stay foch-dev's own floor.
	"""
	root: Path = repo_root / "pyproject.toml"
	cargo: Path = repo_root / "Cargo.toml"
	library: Path = repo_root / LIBRARY_MANIFEST
	tools: Path = repo_root / FOCH_DEV_PYPROJECT
	floor_spec: object = toml_value(tools, "project", "requires-python")
	floor: re.Match[str] | None = (
		REQUIRES_PYTHON_FLOOR.fullmatch(floor_spec)
		if isinstance(floor_spec, str)
		else None
	)
	expected: tuple[tuple[str, object, object], ...] = (
		(f"{root} [project] name", toml_value(root, "project", "name"), DISTRIBUTION),
		(
			f"{cargo} [workspace.package] repository",
			toml_value(cargo, "workspace", "package", "repository"),
			WINGET_REPOSITORY_URL,
		),
		(
			f"{root} [project] license",
			toml_value(root, "project", "license"),
			LICENSE_EXPRESSION,
		),
		(
			f"{library} [package] license",
			toml_value(library, "package", "license"),
			LICENSE_EXPRESSION,
		),
		(
			f"{root} [tool.ty.environment] python-version",
			toml_value(root, "tool", "ty", "environment", "python-version"),
			None if floor is None else floor.group(1),
		),
	)
	return [
		f"{where} is {actual!r}, expected {wanted!r}"
		for where, actual, wanted in expected
		if wanted is None or actual != wanted
	]


def verify_distribution_metadata(repo_root: Path) -> None:
	problems: list[str] = distribution_problems(repo_root)
	if problems:
		raise ValueError(
			"distribution metadata disagrees across channels:\n" + "\n".join(problems)
		)


def cargo_metadata(repo_root: Path) -> CargoMetadata:
	result = subprocess.run(
		[
			"cargo",
			"metadata",
			"--locked",
			"--no-deps",
			"--format-version",
			"1",
		],
		cwd=repo_root,
		check=True,
		# cargo's diagnostics go straight to stderr; CalledProcessError omits them.
		stdout=subprocess.PIPE,
		text=True,
		encoding="utf-8",
	)
	return cast(CargoMetadata, json.loads(result.stdout))


def verify_repository(repo_root: Path) -> None:
	metadata = cargo_metadata(repo_root)
	verify_desktop_contract(repo_root, metadata["packages"])
	verify_publishable_closure(metadata["packages"])
	verify_schema_in_package(repo_root, metadata["packages"])
	verify_distribution_metadata(repo_root)
	actual = tuple(
		sorted(
			(package["name"], target["name"])
			for package in metadata["packages"]
			for target in package["targets"]
			if "bin" in target["kind"]
		)
	)
	if actual != EXPECTED_BINARIES:
		raise ValueError(
			"Cargo binaries must be exactly the foch CLI and foch-desktop app; "
			f"expected {EXPECTED_BINARIES}, found {actual}"
		)
	examples = tuple(
		sorted(
			(package["name"], target["name"], tuple(target["required-features"]))
			for package in metadata["packages"]
			for target in package["targets"]
			if "example" in target["kind"]
		)
	)
	if examples != EXPECTED_EXAMPLES:
		raise ValueError(
			"Cargo examples must be the exact dev-tools allowlist; "
			f"expected {EXPECTED_EXAMPLES}, found {examples}"
		)
	custom_harnesses = [
		target
		for package in metadata["packages"]
		for target in custom_harness_targets(Path(package["manifest_path"]))
	]
	if custom_harnesses:
		raise ValueError(
			"custom Cargo test/bench harnesses are forbidden: "
			+ ", ".join(custom_harnesses)
		)
