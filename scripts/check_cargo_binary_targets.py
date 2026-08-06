from __future__ import annotations

import json
import subprocess
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
	targets: list[CargoTarget]


class CargoMetadata(TypedDict):
	packages: list[CargoPackage]


EXPECTED_BINARIES: tuple[tuple[str, str], ...] = (("foch-cli", "foch"),)
EXPECTED_DEV_EXAMPLES: tuple[str, ...] = ("parse_stats", "symbol_dump")


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
	dev_examples = {
		target["name"]: tuple(target["required-features"])
		for package in metadata["packages"]
		if package["name"] == "foch-cli"
		for target in package["targets"]
		if "example" in target["kind"] and target["name"] in EXPECTED_DEV_EXAMPLES
	}
	expected_examples = {name: ("dev-tools",) for name in EXPECTED_DEV_EXAMPLES}
	if dev_examples != expected_examples:
		raise SystemExit(
			"foch-cli diagnostic examples must exist only behind dev-tools; "
			f"expected {expected_examples}, found {dev_examples}"
		)


if __name__ == "__main__":
	main()
