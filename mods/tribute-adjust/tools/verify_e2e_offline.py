"""Offline Tribute Adjust package, companion-mod, and Foch staging check.

Never starts eu4.exe. ``plugin launch`` is only invoked with ``--prepare-only``.
``FOCH_DATA_DIR`` and ``FOCH_CONFIG_DIR`` point at a temporary directory.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPO = Path(__file__).resolve().parents[3]
PACKAGE = Path(__file__).resolve().parent / "package.py"
WORKSHOP = ROOT / "workshop"
PLUGIN_ID = "dev.foch.eu4-tribute-adjust"


def run(command: list[str], env: dict[str, str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
	print("+", " ".join(command), flush=True)
	completed = subprocess.run(
		command,
		cwd=cwd or REPO,
		env=env,
		text=True,
		encoding="utf-8",
		errors="replace",
		capture_output=True,
	)
	if completed.stdout:
		print(completed.stdout, end="" if completed.stdout.endswith("\n") else "\n")
	if completed.stderr:
		print(completed.stderr, end="" if completed.stderr.endswith("\n") else "\n", file=sys.stderr)
	return completed


def require(completed: subprocess.CompletedProcess[str], what: str) -> subprocess.CompletedProcess[str]:
	if completed.returncode != 0:
		raise SystemExit(f"{what} failed with exit {completed.returncode}")
	return completed


def assert_bbcode_and_companion() -> None:
	for name in ("STEAM_WORKSHOP_EN.bbcode", "STEAM_WORKSHOP_ZH.bbcode"):
		text = (WORKSHOP / name).read_text(encoding="utf-8")
		if "`" in text:
			raise SystemExit(f"{name} still contains a Markdown backtick")
		if "tools/package.py" not in text or "foch plugin install" not in text:
			raise SystemExit(f"{name} is missing the packager or foch plugin install section")
	descriptor = (WORKSHOP / "descriptor.mod").read_text(encoding="utf-8")
	for needle in (
		'name="Tribute Adjust (Foch Companion)"',
		'supported_version="1.37.*"',
		'picture="thumbnail.png"',
		'"Balance"',
		'"Gameplay"',
		'"Utilities"',
	):
		if needle not in descriptor:
			raise SystemExit(f"descriptor.mod is missing {needle}")
	companion = (WORKSHOP / "foch-companion.toml").read_text(encoding="utf-8")
	for needle in (
		"schema = 1",
		'plugin_id = "dev.foch.eu4-tribute-adjust"',
		'game = "eu4"',
		'game_versions = "=1.37.5"',
		"requires_foch = true",
	):
		if needle not in companion:
			raise SystemExit(f"foch-companion.toml is missing {needle}")
	for name in (
		"localisation/foch_ta_companion_l_english.yml",
		"localisation/foch_ta_companion_l_simp_chinese.yml",
	):
		raw = (WORKSHOP / name).read_bytes()
		if not raw.startswith(b"\xef\xbb\xbf"):
			raise SystemExit(f"{name} is missing the UTF-8 BOM")
	if not (WORKSHOP / "thumbnail.png").is_file():
		raise SystemExit("thumbnail.png is missing")
	for relative in (
		"decisions/foch_ta_companion.txt",
		"events/foch_ta_companion.txt",
	):
		text = (WORKSHOP / relative).read_text(encoding="utf-8")
		if "foch_ta_" not in text:
			raise SystemExit(f"{relative} is not namespaced")


def assert_package(package_dir: Path, panel_expected: bool) -> None:
	required = [
		"foch-plugin.toml",
		"foch_eu4_tribute.dll",
		"signatures/signatures-1.37.5.toml",
		"LICENSE",
		"SHA256SUMS.txt",
	]
	if panel_expected:
		required.append("tribute-panel.exe")
	for relative in required:
		if not (package_dir / relative).is_file():
			raise SystemExit(f"package is missing {relative}")
	sums = (package_dir / "SHA256SUMS.txt").read_text(encoding="utf-8")
	lines = [line for line in sums.splitlines() if line]
	listed = {line.split("  ", 1)[1] for line in lines}
	if listed != set(required) - {"SHA256SUMS.txt"}:
		raise SystemExit(f"SHA256SUMS.txt entries {sorted(listed)} do not match the package")
	import hashlib

	for line in lines:
		digest, name = line.split("  ", 1)
		actual = hashlib.sha256((package_dir / name).read_bytes()).hexdigest()
		if actual != digest:
			raise SystemExit(f"checksum mismatch for {name}")


def main() -> None:
	assert_bbcode_and_companion()
	env = os.environ.copy()
	env["CARGO_BUILD_JOBS"] = "2"
	require(
		run(
			[
				"cargo",
				"build",
				"-p",
				"foch-cli",
				"-p",
				"foch-runtime-host",
				"-p",
				"foch-eu4-tribute",
			],
			env,
		),
		"cargo build",
	)
	debug = REPO / "target/debug"
	foch = debug / "foch.exe"
	dll = debug / "foch_eu4_tribute.dll"
	host = debug / "foch_runtime_host.dll"
	panel = debug / "tribute-panel.exe"
	for path in (foch, dll, host):
		if not path.is_file():
			raise SystemExit(f"missing build output {path}")

	package_dir = REPO / "target/e2e-pkg/tribute-adjust"
	if package_dir.exists() or package_dir.is_symlink():
		shutil.rmtree(package_dir)
	package_cmd = [sys.executable, str(PACKAGE), str(package_dir), "--dll", str(dll)]
	if panel.is_file():
		package_cmd.extend(["--panel", str(panel)])
	require(run(package_cmd, env), "package.py")
	assert_package(package_dir, panel.is_file())

	refused = run([sys.executable, str(PACKAGE), str(package_dir), "--dll", str(dll)], env)
	if refused.returncode == 0:
		raise SystemExit("package.py accepted an existing output directory")
	with tempfile.TemporaryDirectory(prefix="ta-bad-") as bad_name:
		bad = Path(bad_name) / "host-as-plugin"
		rejected = run(
			[sys.executable, str(PACKAGE), str(bad), "--dll", str(host)],
			env,
		)
		if rejected.returncode == 0 or bad.exists():
			raise SystemExit("package.py accepted the host DLL as a plugin")

	isolated = Path(tempfile.mkdtemp(prefix="ta-e2e-"))
	try:
		data = isolated / "data"
		config = isolated / "config"
		game = isolated / "game"
		runtime = isolated / "rt"
		user = isolated / "user"
		for directory in (data, config, game, runtime, user):
			directory.mkdir()
		version = game / "version.txt"
		executable = game / "eu4.exe"
		version.write_text("1.37.5\n", encoding="utf-8", newline="\n")
		executable.write_bytes(b"MZ-not-a-real-eu4-binary\n")
		before_game = {
			path.name: path.read_bytes() for path in (version, executable)
		}
		env["FOCH_DATA_DIR"] = str(data)
		env["FOCH_CONFIG_DIR"] = str(config)

		check = require(
			run(
				[
					str(foch),
					"check",
					str(WORKSHOP),
					"--no-game-base",
					"--no-color",
				],
				env,
			),
			"foch check",
		)
		if "fatal_errors: 0" not in check.stdout or "findings: 0" not in check.stdout:
			raise SystemExit("foch check reported diagnostics")
		if "strict_findings: 0" not in check.stdout or "advisory_findings: 0" not in check.stdout:
			raise SystemExit("foch check reported strict or advisory findings")

		require(run([str(foch), "plugin", "import", str(package_dir)], env), "plugin import")
		require(
			run(
				[
					str(foch),
					"plugin",
					"enable",
					PLUGIN_ID,
					"--playset",
					"e2e-test",
				],
				env,
			),
			"plugin enable",
		)
		plan = require(
			run(
				[
					str(foch),
					"plugin",
					"plan",
					"--playset",
					"e2e-test",
					"--game-path",
					str(game),
				],
				env,
			),
			"plugin plan",
		)
		if "launchable" not in plan.stdout or "not launchable" in plan.stdout:
			raise SystemExit("plugin plan is not launchable")
		launch = require(
			run(
				[
					str(foch),
					"plugin",
					"launch",
					"--playset",
					"e2e-test",
					"--prepare-only",
					"--runtime-base",
					str(runtime),
					"--user-dir",
					str(user),
					"--game-path",
					str(game),
					"--format",
					"json",
				],
				env,
			),
			"plugin launch --prepare-only",
		)
		record = json.loads(launch.stdout)
		if record.get("prepared_only") is not True or record.get("pid") is not None:
			raise SystemExit(f"launch record is not prepare-only: {record}")
		runtime_dir = Path(record["runtime"])
		if not (runtime_dir / "VERSION.dll").is_file():
			raise SystemExit("VERSION.dll was not staged")
		staged = list(runtime_dir.rglob("foch_eu4_tribute.dll"))
		if not staged:
			raise SystemExit("foch_eu4_tribute.dll was not staged")
		run_json = json.loads((runtime_dir / "foch-host/run.json").read_text(encoding="utf-8"))
		if run_json.get("prepared_only") is not True or run_json.get("pid") is not None:
			raise SystemExit(f"run.json is not prepare-only: {run_json}")
		after_game = {path.name: path.read_bytes() for path in (version, executable)}
		if after_game != before_game:
			raise SystemExit("synthetic game files were modified")
		print("offline e2e ok")
		print(f"package: {package_dir}")
		print(f"runtime: {runtime_dir}")
	finally:
		shutil.rmtree(isolated, ignore_errors=True)


if __name__ == "__main__":
	main()
