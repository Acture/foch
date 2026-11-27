"""Verify the country-month companion mod and offline Foch staging.

This never starts eu4.exe. `foch plugin launch` is only invoked with
`--prepare-only`, against a stub game directory.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
PLUGIN = REPO / "src" / "plugins" / "eu4-country-month"
WORKSHOP = PLUGIN / "workshop"
FOCH = REPO / "target" / "debug" / "foch.exe"
HOST = REPO / "target" / "debug" / "foch_runtime_host.dll"
STAGED = REPO / "target" / "plugins" / "eu4-country-month"
HOME = REPO / "target" / "e2e-country-month"
DLL_NAME = "eu4_country_month.dll"
PLUGIN_ID = "dev.foch.eu4-country-month"
LOC_KEYS = (
	"foch_cm_companion_title",
	"foch_cm_companion_desc",
)


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


def eu4_pids() -> set[int]:
	completed = subprocess.run(
		["tasklist", "/FI", "IMAGENAME eq eu4.exe", "/FO", "CSV", "/NH"],
		text=True,
		encoding="utf-8",
		errors="replace",
		capture_output=True,
		check=False,
	)
	pids: set[int] = set()
	for line in completed.stdout.splitlines():
		if "eu4.exe" not in line.lower():
			continue
		parts = [part.strip().strip('"') for part in line.split(",")]
		if len(parts) >= 2 and parts[1].isdigit():
			pids.add(int(parts[1]))
	return pids


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
	descriptor = (WORKSHOP / "descriptor.mod").read_text(encoding="utf-8")
	for needle in (
		'name="EU4 Country Month (Foch Companion)"',
		'supported_version="1.37.*"',
	):
		if needle not in descriptor:
			fail(f"descriptor.mod is missing {needle}")
	companion = (WORKSHOP / "foch.toml").read_text(encoding="utf-8")
	for needle in (
		"[[plugins]]",
		f'id = "{PLUGIN_ID}"',
		'game = "eu4"',
		'game_versions = "=1.37.5"',
		"required = true",
	):
		if needle not in companion:
			fail(f"foch.toml is missing {needle}")
	for name in (
		"foch_cm_companion_l_english.yml",
		"foch_cm_companion_l_simp_chinese.yml",
	):
		data = (WORKSHOP / "localisation" / name).read_bytes()
		if not data.startswith(b"\xef\xbb\xbf"):
			fail(f"{name} is missing the UTF-8 BOM")
		text = data.decode("utf-8-sig")
		for key in LOC_KEYS:
			if not key.startswith("foch_cm_"):
				fail(f"{key} is not a foch_cm_* key")
			if key not in text:
				fail(f"{name} is missing {key}")
		for banned in ("add_", "set_", "clr_", "remove_", "change_", "override"):
			if banned in text:
				fail(f"{name} contains gameplay mutation {banned}")


def prepare_home() -> None:
	if HOME.exists() and not HOME.is_symlink():
		shutil.rmtree(HOME)
	for relative in ("data", "config", "cache", "game", "user", "runtime"):
		(HOME / relative).mkdir(parents=True)
	(HOME / "game" / "version.txt").write_text("1.37.5\n", encoding="utf-8", newline="\n")
	# A stand-in executable. Launch is prepare-only and must not execute it.
	(HOME / "game" / "eu4.exe").write_bytes(b"MZ" + b"\0" * 64)


def check_env() -> dict[str, str]:
	"""Use the installed base snapshot, not the empty plugin-staging root."""
	env = os.environ.copy()
	env["CARGO_BUILD_JOBS"] = "2"
	return env


def check_workshop() -> None:
	env = check_env()
	bare = run([str(FOCH), "check", str(WORKSHOP), "--no-color"], env, REPO)
	content = run(
		[str(FOCH), "check", str(WORKSHOP), "--no-color", "--no-game-base"],
		env,
		REPO,
	)
	if content.returncode != 0:
		fail(f"foch check --no-game-base exited {content.returncode}")
	require_clean(content.stdout, "foch check --no-game-base")
	if bare.returncode == 0:
		require_clean(bare.stdout, "foch check")
		return
	bare_text = bare.stdout + bare.stderr
	snapshot = (
		"analysis rules version mismatch" in bare_text
		or "missing installed base data" in bare_text
	)
	if not snapshot:
		fail(f"foch check exited {bare.returncode}")
	if "findings: 0" not in bare.stdout or "advisory_findings: 0" not in bare.stdout:
		fail("foch check reported content diagnostics before the base snapshot")
	print("foch check fatal is the installed base snapshot, not workshop content")


def stage_plugin(env: dict[str, str], before: set[int]) -> None:
	game = HOME / "game"
	user = HOME / "user"
	runtime = HOME / "runtime"
	imported = run([str(FOCH), "plugin", "import", str(STAGED)], env, REPO)
	if imported.returncode != 0:
		fail(f"foch plugin import exited {imported.returncode}")
	enabled = run(
		[str(FOCH), "plugin", "enable", PLUGIN_ID, "--playset", "e2e-test"],
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
		fail("run.json does not name the country-month plugin")
	after = eu4_pids()
	if after != before:
		fail(f"eu4.exe process set changed: before={before} after={after}")
	print(f"staged {staged[0]}")


def main() -> None:
	if not FOCH.is_file():
		fail(f"missing {FOCH}; build foch-cli first")
	if not HOST.is_file():
		fail(f"missing {HOST}; build foch-runtime-host first")
	dll = STAGED / DLL_NAME
	if not dll.is_file() or not dll.read_bytes().startswith(b"MZ"):
		fail(f"missing staged {dll}; run build.ps1 first")
	check_static()
	before = eu4_pids()
	prepare_home()
	env = isolated_env()
	check_workshop()
	stage_plugin(env, before)
	print("offline e2e ok")


if __name__ == "__main__":
	main()
