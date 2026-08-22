from __future__ import annotations

import json
import subprocess
import tomllib
from pathlib import Path
from typing import TypedDict, cast


CargoTarget = TypedDict(
	"CargoTarget",
	{
		"name": str,
		"kind": list[str],
		"required-features": list[str],
	},
)


class CargoDependency(TypedDict):
	name: str


class CargoPackage(TypedDict):
	name: str
	manifest_path: str
	targets: list[CargoTarget]
	dependencies: list[CargoDependency]


class CargoMetadata(TypedDict):
	packages: list[CargoPackage]


EXPECTED_BINARIES: tuple[tuple[str, str], ...] = (
	("foch-cli", "foch"),
	("foch-desktop", "foch-desktop"),
)
EXPECTED_EXAMPLES: tuple[tuple[str, str, tuple[str, ...]], ...] = (
	("foch-cli", "parse_stats", ("dev-tools",)),
	("foch-cli", "symbol_dump", ("dev-tools",)),
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


def verify_desktop_contract(repo_root: Path, packages: list[CargoPackage]) -> None:
	desktop = next(
		(package for package in packages if package["name"] == "foch-desktop"),
		None,
	)
	if desktop is None:
		raise SystemExit("foch-desktop Cargo package is missing")
	dependencies = {dependency["name"] for dependency in desktop["dependencies"]}
	required = {"foch-core", "foch-engine"}
	if not required.issubset(dependencies):
		raise SystemExit(
			f"foch-desktop must depend directly on {sorted(required)}; found {sorted(dependencies)}"
		)
	if "foch-cli" in dependencies:
		raise SystemExit("foch-desktop must not depend on or bundle the foch CLI")

	desktop_root = repo_root / "apps" / "foch-desktop"
	package = cast(
		dict[str, object],
		json.loads((desktop_root / "package.json").read_text(encoding="utf-8")),
	)
	frontend_dependencies = cast(dict[str, object], package.get("dependencies", {}))
	for forbidden in (
		"@tauri-apps/plugin-fs",
		"@tauri-apps/plugin-opener",
		"@tauri-apps/plugin-shell",
	):
		if forbidden in frontend_dependencies:
			raise SystemExit(f"foch-desktop must not enable {forbidden} in APP-001")

	config = cast(
		dict[str, object],
		json.loads(
			(desktop_root / "src-tauri" / "tauri.conf.json").read_text(encoding="utf-8")
		),
	)
	if config.get("identifier") != "dev.acture.foch":
		raise SystemExit("foch-desktop bundle identifier must be dev.acture.foch")
	bundle = cast(dict[str, object], config.get("bundle", {}))
	if "externalBin" in bundle or "resources" in bundle:
		raise SystemExit("foch-desktop APP-001 must not bundle sidecars or resources")

	capability = cast(
		dict[str, object],
		json.loads(
			(desktop_root / "src-tauri" / "capabilities" / "default.json").read_text(
				encoding="utf-8"
			)
		),
	)
	if capability.get("permissions") != []:
		raise SystemExit("foch-desktop APP-001 capability must remain empty")


def main() -> None:
	repo_root = Path(__file__).resolve().parent.parent
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
		capture_output=True,
		text=True,
	)
	metadata = cast(CargoMetadata, json.loads(result.stdout))
	verify_desktop_contract(repo_root, metadata["packages"])
	actual = tuple(
		sorted(
			(package["name"], target["name"])
			for package in metadata["packages"]
			for target in package["targets"]
			if "bin" in target["kind"]
		)
	)
	if actual != EXPECTED_BINARIES:
		raise SystemExit(
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
		raise SystemExit(
			"Cargo examples must be the exact dev-tools allowlist; "
			f"expected {EXPECTED_EXAMPLES}, found {examples}"
		)
	custom_harnesses = [
		target
		for package in metadata["packages"]
		for target in custom_harness_targets(Path(package["manifest_path"]))
	]
	if custom_harnesses:
		raise SystemExit(
			"custom Cargo test/bench harnesses are forbidden: "
			+ ", ".join(custom_harnesses)
		)


if __name__ == "__main__":
	main()
