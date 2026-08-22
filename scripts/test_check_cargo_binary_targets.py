from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from check_cargo_binary_targets import (
	CargoDependency,
	CargoPackage,
	source_violations,
	verify_desktop_frontend_dependencies,
	verify_desktop_rust_dependencies,
)


def cargo_dependency(
	name: str,
	*,
	kind: str | None = None,
	target: str | None = None,
	features: list[str] | None = None,
) -> CargoDependency:
	return CargoDependency(
		name=name,
		kind=kind,
		target=target,
		features=[] if features is None else features,
	)


def desktop_package(*extra_dependencies: CargoDependency) -> CargoPackage:
	return CargoPackage(
		name="foch-desktop",
		manifest_path="apps/foch-desktop/src-tauri/Cargo.toml",
		targets=[],
		dependencies=[
			cargo_dependency("foch-core"),
			cargo_dependency("foch-engine"),
			cargo_dependency("tauri"),
			*extra_dependencies,
		],
	)


class DesktopContractTests(unittest.TestCase):
	def test_rejects_forbidden_target_specific_dev_dependency(self) -> None:
		package = desktop_package(
			cargo_dependency("foch-cli", kind="dev", target="cfg(windows)")
		)

		with self.assertRaisesRegex(SystemExit, "foch-cli"):
			verify_desktop_rust_dependencies(package)

	def test_rejects_tokio_process_feature(self) -> None:
		package = desktop_package(cargo_dependency("tokio", features=["process"]))

		with self.assertRaisesRegex(SystemExit, "feature=process"):
			verify_desktop_rust_dependencies(package)

	def test_rejects_frontend_plugin_from_dev_dependencies(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			desktop_root = Path(directory)
			(desktop_root / "package.json").write_text(
				json.dumps({"devDependencies": {"@tauri-apps/plugin-shell": "2.0.0"}}),
				encoding="utf-8",
			)

			with self.assertRaisesRegex(SystemExit, "plugin-shell"):
				verify_desktop_frontend_dependencies(desktop_root)

	def test_source_scan_rejects_process_api_but_ignores_test_module(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			desktop_root = Path(directory)
			rust_root = desktop_root / "src-tauri" / "src"
			frontend_root = desktop_root / "src"
			rust_root.mkdir(parents=True)
			frontend_root.mkdir()
			(rust_root / "lib.rs").write_text(
				'fn launch() { std::process::Command::new("foch"); }\n',
				encoding="utf-8",
			)

			self.assertEqual(
				source_violations(desktop_root),
				[
					"src-tauri/src/lib.rs:1: process command construction",
					"src-tauri/src/lib.rs:1: standard-library process API",
				],
			)

			(rust_root / "lib.rs").write_text(
				'#[cfg(test)]\nmod tests {\n\tfn probe() { std::process::Command::new("foch"); }\n}\n',
				encoding="utf-8",
			)
			self.assertEqual(source_violations(desktop_root), [])


if __name__ == "__main__":
	unittest.main()
