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


class CargoPackage(TypedDict):
	name: str
	manifest_path: str
	targets: list[CargoTarget]


class CargoMetadata(TypedDict):
	packages: list[CargoPackage]


EXPECTED_BINARIES: tuple[tuple[str, str], ...] = (("foch-cli", "foch"),)
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
			f"normal Cargo binaries must be exactly {EXPECTED_BINARIES}; found {actual}"
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
