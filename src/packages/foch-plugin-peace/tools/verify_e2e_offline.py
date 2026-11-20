"""Verify the peace package, companion mod, and offline Foch staging.

This never starts eu4.exe. `foch plugin launch` is only invoked with
`--prepare-only`.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
PACKAGE = REPO / "src" / "packages" / "foch-plugin-peace"
WORKSHOP = PACKAGE / "workshop"
FOCH = REPO / "target" / "debug" / "foch.exe"
PACKAGE_PY = PACKAGE / "tools" / "package.py"
PKG = REPO / "target" / "e2e-pkg" / "peace-bilateral"
HOME = REPO / "target" / "e2e-home"
DLL_NAME = "foch_plugin_peace.dll"
PLUGIN_ID = "io.github.acture.foch-peace"


def fail(message: str) -> None:
	raise SystemExit(message)


def run(cmd: list[str], env: dict[str, str], cwd: Path) -> subprocess.CompletedProcess[str]:
	print("+", " ".join(cmd), flush=True)
	completed = subprocess.run(
		cmd,
		cwd=cwd,
		env=env,
		text=True,
		encoding="utf-8",
		errors="replace",
		capture_output=True,
		check=False,
	)
	if completed.stdout:
		print(completed.stdout, end="" if completed.stdout.endswith("\n") else "\n")
	if completed.stderr:
		print(completed.stderr, end="" if completed.stderr.endswith("\n") else "\n", file=sys.stderr)
	return completed


def isolated_env() -> dict[str, str]:
	env = os.environ.copy()
	env["FOCH_DATA_DIR"] = str(HOME / "data")
	env["FOCH_CONFIG_DIR"] = str(HOME / "config")
	env["FOCH_CACHE_ROOT"] = str(HOME / "cache")
	env["CARGO_BUILD_JOBS"] = "2"
	return env


def report_counts(text: str) -> dict[str, int]:
	counts: dict[str, int] = {}
	for key in ("fatal_errors", "findings", "strict_findings", "advisory_findings"):
		marker = f"{key}: "
		found = [line for line in text.splitlines() if line.startswith(marker)]
		if len(found) != 1:
			fail(f"check report is missing {key}")
		counts[key] = int(found[0].removeprefix(marker))
	parse_errors = [line for line in text.splitlines() if "parse_errors=" in line]
	if len(parse_errors) != 1:
		fail("check report is missing parse_errors")
	counts["parse_errors"] = int(parse_errors[0].split("parse_errors=", 1)[1].split()[0])
	return counts


def require_clean(text: str, label: str) -> None:
	counts = report_counts(text)
	print(f"{label}: {counts}")
	if any(value != 0 for value in counts.values()):
		fail(f"{label} is not clean: {counts}")


def check_static() -> None:
	for name in ("STEAM_WORKSHOP_EN.bbcode", "STEAM_WORKSHOP_ZH.bbcode"):
		text = (WORKSHOP / name).read_text(encoding="utf-8")
		if "`" in text:
			fail(f"{name} contains a Markdown backtick")
		for needle in ("foch plugin install", "package.py", "[code]gh[/code]", "HTTPS"):
			if needle not in text:
				fail(f"{name} is missing {needle}")
	english = (WORKSHOP / "STEAM_WORKSHOP_EN.bbcode").read_text(encoding="utf-8")
	chinese = (WORKSHOP / "STEAM_WORKSHOP_ZH.bbcode").read_text(encoding="utf-8")
	if "China-friendly" not in english or "镜像" not in chinese:
		fail("workshop pages do not mention the China-friendly mirror")

	descriptor = (WORKSHOP / "descriptor.mod").read_text(encoding="utf-8")
	for needle in (
		'name="Bilateral Peace (Foch Companion)"',
		'supported_version="1.37.*"',
		'picture="thumbnail.png"',
		'"Diplomacy"',
		'"Gameplay"',
		'"Balance"',
		'"Utilities"',
	):
		if needle not in descriptor:
			fail(f"descriptor.mod is missing {needle}")
	if not (WORKSHOP / "thumbnail.png").is_file():
		fail("thumbnail.png is missing")

	companion = (WORKSHOP / "foch-companion.toml").read_text(encoding="utf-8")
	for needle in (
		"schema = 1",
		f'plugin_id = "{PLUGIN_ID}"',
		'game = "eu4"',
		'game_versions = "=1.37.5"',
		"requires_foch = true",
	):
		if needle not in companion:
			fail(f"foch-companion.toml is missing {needle}")

	decision = (WORKSHOP / "decisions" / "foch_bp_companion.txt").read_text(encoding="utf-8")
	event = (WORKSHOP / "events" / "foch_bp_companion.txt").read_text(encoding="utf-8")
	if "foch_bp_companion_info" not in decision or "foch_bp_info.1" not in decision:
		fail("companion decision does not explain bilateral peace")
	if "namespace = foch_bp_info" not in event or "is_triggered_only = yes" not in event:
		fail("companion event is not the informational foch_bp_info event")
	for banned in ("add_", "set_", "clr_", "remove_", "change_", "override"):
		if banned in decision or banned in event:
			fail(f"companion script contains gameplay mutation {banned}")

	for name in (
		"foch_bp_companion_l_english.yml",
		"foch_bp_companion_l_simp_chinese.yml",
	):
		data = (WORKSHOP / "localisation" / name).read_bytes()
		if not data.startswith(b"\xef\xbb\xbf"):
			fail(f"{name} is missing the UTF-8 BOM")
		text = data.decode("utf-8-sig")
		for key in (
			"foch_bp_companion_info_title",
			"foch_bp_companion_info_desc",
			"foch_bp_info.1.t",
			"foch_bp_info.1.d",
			"foch_bp_info.1.a",
		):
			if key not in text:
				fail(f"{name} is missing {key}")


def prepare_home() -> None:
	if HOME.exists() and not HOME.is_symlink():
		shutil.rmtree(HOME)
	for relative in ("data", "config", "cache", "game", "user", "runtime"):
		(HOME / relative).mkdir(parents=True)
	(HOME / "game" / "version.txt").write_text("1.37.5\n", encoding="utf-8", newline="\n")
	# A stand-in executable. Launch is prepare-only and must not execute it.
	(HOME / "game" / "eu4.exe").write_bytes(b"MZ" + b"\0" * 64)


def package_plugin() -> None:
	if PKG.is_symlink():
		fail(f"refusing to replace symlink {PKG}")
	if PKG.exists():
		shutil.rmtree(PKG)
	packaged = run([sys.executable, str(PACKAGE_PY), str(PKG)], os.environ.copy(), REPO)
	if packaged.returncode != 0:
		fail(f"package.py failed with {packaged.returncode}")
	dll = (PKG / DLL_NAME).read_bytes()
	if not dll.startswith(b"MZ"):
		fail("packaged DLL is not a PE image")
	digest = hashlib.sha256(dll).hexdigest()
	manifest = (PKG / "foch-plugin.toml").read_text(encoding="utf-8")
	if f'sha256 = "{digest}"' not in manifest:
		fail("packaged manifest does not bind the DLL SHA-256")
	if (PKG / "LICENSE").read_bytes() != (REPO / "LICENSE").read_bytes():
		fail("packaged LICENSE does not match the repository license")
	occupied = tempfile.mkdtemp(prefix="peace-package-refuse-")
	try:
		refused = run(
			[sys.executable, str(PACKAGE_PY), occupied],
			os.environ.copy(),
			REPO,
		)
		if refused.returncode == 0:
			fail("package.py accepted an existing directory")
	finally:
		shutil.rmtree(occupied, ignore_errors=True)


def check_workshop(env: dict[str, str]) -> None:
	bare = run(
		[str(FOCH), "check", str(WORKSHOP), "--no-color"],
		env,
		REPO,
	)
	isolated = run(
		[str(FOCH), "check", str(WORKSHOP), "--no-color", "--no-game-base"],
		env,
		REPO,
	)
	if isolated.returncode != 0:
		fail(f"foch check --no-game-base exited {isolated.returncode}")
	require_clean(isolated.stdout, "foch check --no-game-base")
	bare_text = bare.stdout + bare.stderr
	if bare.returncode == 0:
		require_clean(bare.stdout, "foch check")
		return
	environment = (
		"missing installed base data" in bare_text
		or "failed to locate" in bare_text
		or "failed to detect" in bare_text
	)
	if not environment:
		fail(f"foch check exited {bare.returncode}")
	print("foch check needs an installed EU4 base snapshot in this isolated data dir")
	if "findings: 0" not in bare.stdout or "advisory_findings: 0" not in bare.stdout:
		fail("foch check reported content diagnostics before the missing base snapshot")


def stage_plugin(env: dict[str, str]) -> None:
	game = HOME / "game"
	user = HOME / "user"
	runtime = HOME / "runtime"
	imported = run([str(FOCH), "plugin", "import", str(PKG)], env, REPO)
	if imported.returncode != 0:
		fail(f"foch plugin import exited {imported.returncode}")
	enabled = run(
		[
			str(FOCH),
			"plugin",
			"enable",
			PLUGIN_ID,
			"--playset",
			"e2e-test",
		],
		env,
		REPO,
	)
	if enabled.returncode != 0:
		fail(f"foch plugin enable exited {enabled.returncode}")
	planned = run(
		[
			str(FOCH),
			"plugin",
			"plan",
			"--playset",
			"e2e-test",
			"--game-path",
			str(game),
			"--format",
			"json",
		],
		env,
		REPO,
	)
	if planned.returncode != 0:
		fail(f"foch plugin plan exited {planned.returncode}")
	plan = json.loads(planned.stdout)
	if plan.get("launchable") is not True:
		fail(f"plan is not launchable: {plan}")
	if plan.get("game_version") != "1.37.5":
		fail(f"plan game version is {plan.get('game_version')}")
	launched = run(
		[
			str(FOCH),
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
		REPO,
	)
	if launched.returncode != 0:
		fail(f"foch plugin launch --prepare-only exited {launched.returncode}")
	record = json.loads(launched.stdout)
	if record.get("prepared_only") is not True or record.get("pid") is not None:
		fail(f"launch record is not prepare-only: {record}")
	layer = Path(record["runtime"])
	if not (layer / "VERSION.dll").is_file():
		fail("VERSION.dll was not staged")
	staged = list(layer.rglob(DLL_NAME))
	if len(staged) != 1:
		fail(f"expected one staged {DLL_NAME}, found {staged}")
	run_json = json.loads((layer / "foch-host" / "run.json").read_text(encoding="utf-8"))
	if run_json.get("prepared_only") is not True or run_json.get("pid") is not None:
		fail(f"foch-host/run.json is not prepare-only: {run_json}")
	if PLUGIN_ID not in json.dumps(run_json):
		fail("run.json does not name the peace plugin")
	print(f"staged {staged[0]}")


def main() -> None:
	if not FOCH.is_file():
		fail(f"missing {FOCH}; build foch-cli first")
	check_static()
	prepare_home()
	package_plugin()
	env = isolated_env()
	check_workshop(env)
	stage_plugin(env)
	print("offline e2e ok")


if __name__ == "__main__":
	main()
