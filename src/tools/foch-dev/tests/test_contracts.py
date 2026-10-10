from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from foch_dev.contracts import (
	CargoDependency,
	CargoPackage,
	distribution_problems,
	publishable_closure_problems,
	published_crates,
	source_violations,
	verify_desktop_frontend_dependencies,
	verify_desktop_rust_dependencies,
	verify_publishable_closure,
	verify_schema_in_package,
)
from foch_dev.schema import SCHEMA_DIR


def cargo_dependency(
	name: str,
	*,
	kind: str | None = None,
	target: str | None = None,
	features: list[str] | None = None,
	req: str = "*",
	path: str | None = None,
) -> CargoDependency:
	dependency = CargoDependency(
		name=name,
		kind=kind,
		target=target,
		features=[] if features is None else features,
		req=req,
	)
	if path is not None:
		dependency["path"] = path
	return dependency


def cargo_package(
	name: str,
	directory: str,
	*dependencies: CargoDependency,
	version: str = "0.0.1",
	publish: list[str] | None = None,
) -> CargoPackage:
	return CargoPackage(
		name=name,
		version=version,
		publish=publish,
		manifest_path=f"/workspace/{directory}/Cargo.toml",
		repository=None,
		targets=[],
		dependencies=list(dependencies),
	)


def desktop_package(*extra_dependencies: CargoDependency) -> CargoPackage:
	return cargo_package(
		"foch-desktop",
		"src/apps/foch-desktop/src-tauri",
		cargo_dependency("foch"),
		cargo_dependency("tauri"),
		*extra_dependencies,
		publish=[],
	)


def library_dependency(name: str, *, req: str = "=0.0.1") -> CargoDependency:
	return cargo_dependency(name, req=req, path=f"/workspace/src/packages/{name}")


# The internal libraries foch-cli links, with their workspace dependencies.
LIBRARIES: dict[str, tuple[str, ...]] = {
	"foch-annotation": ("foch",),
	"foch-test": ("foch", "foch-annotation"),
	"foch-lsp": ("foch", "foch-annotation"),
	"foch-runner": ("foch", "foch-test"),
}
PUBLISHED: tuple[str, ...] = (
	"foch",
	"foch-annotation",
	"foch-cli",
	"foch-lsp",
	"foch-runner",
	"foch-test",
)


def workspace_packages(
	*cli_dependencies: CargoDependency,
	foch_req: str = "=0.0.1",
) -> list[CargoPackage]:
	"""The workspace as `cargo metadata --no-deps` lists it."""
	return [
		cargo_package(
			"foch",
			"src/packages/foch",
			cargo_dependency(
				"tree-sitter-paradox",
				req="=0.2.0",
				path="/workspace/src/packages/tree-sitter-paradox",
			),
			cargo_dependency(
				"tree-sitter-paradox",
				kind="build",
				req="=0.2.0",
				path="/workspace/src/packages/tree-sitter-paradox",
			),
		),
		*(
			cargo_package(
				name,
				f"src/packages/{name}",
				*(library_dependency(dependency) for dependency in dependencies),
			)
			for name, dependencies in LIBRARIES.items()
		),
		cargo_package(
			"foch-cli",
			"src/apps/foch-cli",
			library_dependency("foch", req=foch_req),
			*(library_dependency(name) for name in LIBRARIES),
			*cli_dependencies,
		),
		cargo_package(
			"foch-desktop",
			"src/apps/foch-desktop/src-tauri",
			cargo_dependency("foch", path="/workspace/src/packages/foch"),
			publish=[],
		),
		cargo_package(
			"tree-sitter-paradox", "src/packages/tree-sitter-paradox", version="0.2.0"
		),
	]


def package_named(packages: list[CargoPackage], name: str) -> CargoPackage:
	return next(package for package in packages if package["name"] == name)


class DesktopContractTests(unittest.TestCase):
	def test_rejects_forbidden_target_specific_dev_dependency(self) -> None:
		package = desktop_package(
			cargo_dependency("foch-cli", kind="dev", target="cfg(windows)")
		)

		with self.assertRaisesRegex(ValueError, "foch-cli"):
			verify_desktop_rust_dependencies(package)

	def test_rejects_tokio_process_feature(self) -> None:
		package = desktop_package(cargo_dependency("tokio", features=["process"]))

		with self.assertRaisesRegex(ValueError, "feature=process"):
			verify_desktop_rust_dependencies(package)

	def test_rejects_frontend_plugin_from_dev_dependencies(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			desktop_root = Path(directory)
			(desktop_root / "package.json").write_text(
				json.dumps({"devDependencies": {"@tauri-apps/plugin-shell": "2.0.0"}}),
				encoding="utf-8",
			)

			with self.assertRaisesRegex(ValueError, "plugin-shell"):
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


class PublishableClosureTests(unittest.TestCase):
	def test_published_crates_are_what_foch_cli_builds_from(self) -> None:
		packages = workspace_packages(
			cargo_dependency(
				"foch-desktop",
				kind="dev",
				path="/workspace/src/apps/foch-desktop/src-tauri",
			)
		)
		# Neither a dev-dependency nor the externally released grammar joins it.
		self.assertEqual(published_crates(packages), PUBLISHED)

	def test_published_crates_need_foch_cli(self) -> None:
		packages = [
			package for package in workspace_packages() if package["name"] != "foch-cli"
		]
		with self.assertRaisesRegex(ValueError, "no foch-cli package"):
			published_crates(packages)
		self.assertEqual(
			publishable_closure_problems(packages),
			["the workspace has no foch-cli package"],
		)

	def test_accepts_exact_path_versions_and_unversioned_dev_dependencies(
		self,
	) -> None:
		verify_publishable_closure(
			workspace_packages(
				cargo_dependency(
					"foch", kind="dev", path="/workspace/src/packages/foch"
				)
			)
		)

	def test_rejects_a_path_dependency_without_an_exact_version(self) -> None:
		with self.assertRaisesRegex(
			ValueError, r"requires '\^0.0.1', expected '=0.0.1'"
		):
			verify_publishable_closure(workspace_packages(foch_req="^0.0.1"))

	def test_rejects_a_loose_pin_between_internal_libraries(self) -> None:
		packages = workspace_packages()
		package_named(packages, "foch-runner")["dependencies"][1]["req"] = "^0.0.1"
		self.assertEqual(
			publishable_closure_problems(packages),
			["foch-runner -> foch-test (normal): requires '^0.0.1', expected '=0.0.1'"],
		)

	def test_rejects_a_dependency_on_a_never_published_package(self) -> None:
		with self.assertRaisesRegex(ValueError, "foch-desktop is never published"):
			verify_publishable_closure(
				workspace_packages(
					cargo_dependency(
						"foch-desktop",
						kind="dev",
						path="/workspace/src/apps/foch-desktop/src-tauri",
					)
				)
			)

	def test_rejects_a_library_foch_cli_builds_from_that_is_never_published(
		self,
	) -> None:
		packages = workspace_packages()
		package_named(packages, "foch-test")["publish"] = []
		# One problem, however many published crates depend on it.
		self.assertEqual(
			publishable_closure_problems(packages),
			["foch-test is published, but its manifest sets publish = false"],
		)

	def test_rejects_an_unpublishable_foch_cli(self) -> None:
		# Nothing depends on the installed crate, so no edge would catch it.
		packages = workspace_packages()
		package_named(packages, "foch-cli")["publish"] = []
		self.assertEqual(
			publishable_closure_problems(packages),
			["foch-cli is published, but its manifest sets publish = false"],
		)

	def test_rejects_an_unexpected_publishable_package(self) -> None:
		packages = workspace_packages()
		package_named(packages, "foch-desktop")["publish"] = None
		with self.assertRaisesRegex(
			ValueError, "foch-desktop is publishable, but foch-cli does not build"
		):
			verify_publishable_closure(packages)


class SchemaLocationTests(unittest.TestCase):
	def schema_package(self, repo_root: Path, schema_dir: str) -> list[CargoPackage]:
		package_root = repo_root / "src/packages/foch"
		(repo_root / SCHEMA_DIR).mkdir(parents=True)
		(package_root / "build.rs").write_text(
			f'// const SCHEMA_DIR: &str = "elsewhere";\nconst SCHEMA_DIR: &str = "{schema_dir}";\n',
			encoding="utf-8",
		)
		package = cargo_package("foch", "src/packages/foch")
		package["manifest_path"] = str(package_root / "Cargo.toml")
		return [package]

	def test_accepts_the_vendored_directory_inside_the_package(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo_root = Path(directory)
			packages = self.schema_package(repo_root, "vendor/cwtools-eu4-config")
			verify_schema_in_package(repo_root, packages)

	def test_rejects_a_schema_outside_the_package(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			repo_root = Path(directory)
			(repo_root / "vendor/cwtools-eu4-config").mkdir(parents=True)
			packages = self.schema_package(
				repo_root, "../../../vendor/cwtools-eu4-config"
			)
			with self.assertRaisesRegex(ValueError, "would not carry the schema"):
				verify_schema_in_package(repo_root, packages)


LICENSE: str = "AGPL-3.0-only AND GPL-3.0-only AND MIT"


def write_distribution_files(
	repo_root: Path, *, name: str = "foch", ty_python: str = "3.11"
) -> None:
	files: dict[str, str] = {
		"pyproject.toml": f'[project]\nname = "{name}"\nlicense = "{LICENSE}"\n'
		f'[tool.ty.environment]\npython-version = "{ty_python}"\n',
		"Cargo.toml": '[workspace.package]\nrepository = "https://github.com/Acture/foch"\n',
		"src/packages/foch/Cargo.toml": f'[package]\nlicense = "{LICENSE}"\n',
		"src/tools/foch-dev/pyproject.toml": '[project]\nrequires-python = ">=3.11"\n',
	}
	for relative, text in files.items():
		path = repo_root / relative
		path.parent.mkdir(parents=True, exist_ok=True)
		path.write_text(text, encoding="utf-8")


class DistributionMetadataTests(unittest.TestCase):
	def test_accepts_one_identity_across_channels(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			write_distribution_files(Path(directory))
			self.assertEqual(distribution_problems(Path(directory)), [])

	def test_reports_every_drifted_spelling(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			write_distribution_files(Path(directory), name="foch-cli", ty_python="3.8")
			problems = distribution_problems(Path(directory))
		self.assertEqual(len(problems), 2)
		self.assertIn("[project] name is 'foch-cli', expected 'foch'", problems[0])
		self.assertIn("python-version is '3.8', expected '3.11'", problems[1])


if __name__ == "__main__":
	unittest.main()
