"""Stage eu4-read-decide through Foch without starting eu4.exe.

Checks the plugin DLL exports `foch_plugin_get_api` and none of the
VERSION host names, then runs `foch plugin import`, `enable`, `plan`,
and `launch --prepare-only` in a fresh data directory. A new eu4.exe
process fails the run.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
import subprocess
import tempfile
from pathlib import Path

ENTRY = "foch_plugin_get_api"
VERSION_EXPORTS = (
	"GetFileVersionInfoA",
	"GetFileVersionInfoByHandle",
	"GetFileVersionInfoExA",
	"GetFileVersionInfoExW",
	"GetFileVersionInfoSizeA",
	"GetFileVersionInfoSizeExA",
	"GetFileVersionInfoSizeExW",
	"GetFileVersionInfoSizeW",
	"GetFileVersionInfoW",
	"VerFindFileA",
	"VerFindFileW",
	"VerInstallFileA",
	"VerInstallFileW",
	"VerLanguageNameA",
	"VerLanguageNameW",
	"VerQueryValueA",
	"VerQueryValueW",
)
PLUGIN_ID = "dev.foch.read-decide"


def export_names(blob: bytes) -> set[str]:
	if blob[:2] != b"MZ":
		raise ValueError("not a PE image")
	pe = struct.unpack_from("<I", blob, 0x3C)[0]
	if blob[pe : pe + 4] != b"PE\0\0":
		raise ValueError("not a PE image")
	magic = struct.unpack_from("<H", blob, pe + 24)[0]
	if magic != 0x20B:
		raise ValueError("expected PE32+")
	export_rva, export_size = struct.unpack_from("<II", blob, pe + 24 + 112)
	if export_rva == 0 or export_size == 0:
		return set()
	sections = _sections(blob, pe)
	export = _read_rva(blob, sections, export_rva, 40)
	count, _functions, name_rvas = struct.unpack_from("<III", export, 24)
	name_table = _read_rva(blob, sections, name_rvas, count * 4)
	found: set[str] = set()
	for index in range(count):
		(rva,) = struct.unpack_from("<I", name_table, index * 4)
		raw = _read_c_string(blob, sections, rva)
		found.add(raw.decode("ascii"))
	return found


def _sections(blob: bytes, pe: int) -> list[tuple[int, int, int]]:
	count = struct.unpack_from("<H", blob, pe + 6)[0]
	optional = struct.unpack_from("<H", blob, pe + 20)[0]
	start = pe + 24 + optional
	sections: list[tuple[int, int, int]] = []
	for index in range(count):
		offset = start + index * 40
		virtual_size, rva, raw_size, raw_ptr = struct.unpack_from("<IIII", blob, offset + 8)
		sections.append((rva, max(virtual_size, raw_size), raw_ptr))
	return sections


def _section_offset(sections: list[tuple[int, int, int]], rva: int) -> int:
	for base, size, raw in sections:
		if base <= rva < base + size:
			return raw + (rva - base)
	raise ValueError(f"RVA {rva:#x} is outside the image")


def _read_rva(blob: bytes, sections: list[tuple[int, int, int]], rva: int, size: int) -> bytes:
	offset = _section_offset(sections, rva)
	return blob[offset : offset + size]


def _read_c_string(blob: bytes, sections: list[tuple[int, int, int]], rva: int) -> bytes:
	offset = _section_offset(sections, rva)
	end = blob.find(b"\0", offset, offset + 256)
	if end < 0:
		raise ValueError("unterminated export name")
	return blob[offset:end]


def eu4_pids() -> set[int]:
	listing = subprocess.run(
		["tasklist", "/FI", "IMAGENAME eq eu4.exe", "/FO", "CSV", "/NH"],
		check=True,
		capture_output=True,
		text=True,
	)
	pids: set[int] = set()
	for line in listing.stdout.splitlines():
		if "eu4.exe" not in line.lower():
			continue
		parts = [part.strip('"') for part in line.split(",")]
		if len(parts) >= 2 and parts[1].isdigit():
			pids.add(int(parts[1]))
	return pids


def package(dll: Path, output: Path) -> None:
	data = dll.read_bytes()
	names = export_names(data)
	if ENTRY not in names:
		raise SystemExit(f"{dll} is missing {ENTRY}")
	leaked = sorted(names.intersection(VERSION_EXPORTS))
	if leaked:
		raise SystemExit(f"{dll} exports VERSION host symbols: {leaked}")
	template = Path(__file__).resolve().parents[1] / "foch-plugin.toml.in"
	manifest = template.read_text(encoding="utf-8").replace(
		"@DLL_SHA256@", hashlib.sha256(data).hexdigest()
	)
	plugins = output / "plugins"
	plugins.mkdir(parents=True)
	(plugins / "foch_plugin_read_decide.dll").write_bytes(data)
	(output / "foch-plugin.toml").write_text(manifest, encoding="utf-8", newline="\n")


def run(foch: Path, args: list[str], env: dict[str, str]) -> subprocess.CompletedProcess[str]:
	if any(arg == "eu4.exe" or arg.endswith("\\eu4.exe") or arg.endswith("/eu4.exe") for arg in args):
		raise SystemExit("refusing to invoke eu4.exe")
	return subprocess.run(
		[str(foch), *args],
		check=False,
		capture_output=True,
		text=True,
		env=env,
	)


def require(result: subprocess.CompletedProcess[str], step: str) -> str:
	if result.returncode != 0:
		raise SystemExit(
			f"{step} failed ({result.returncode})\n{result.stdout}\n{result.stderr}"
		)
	return result.stdout


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--foch", type=Path, required=True)
	parser.add_argument("--dll", type=Path, required=True)
	parser.add_argument("--host", type=Path, required=True)
	parser.add_argument("--game", type=Path, required=True)
	args = parser.parse_args()
	for path in (args.foch, args.dll, args.host, args.game):
		if not path.exists():
			raise SystemExit(f"missing {path}")
	before = eu4_pids()
	env = os.environ.copy()
	with tempfile.TemporaryDirectory(prefix="foch-read-decide-") as temporary:
		root = Path(temporary)
		env["FOCH_DATA_DIR"] = str(root / "data")
		staged = root / "package"
		package(args.dll, staged)
		user = root / "user"
		user.mkdir()
		runtime = root / "runtime"
		require(
			run(args.foch, ["plugin", "import", str(staged)], env),
			"import",
		)
		require(
			run(
				args.foch,
				["plugin", "enable", PLUGIN_ID, "--playset", "offline"],
				env,
			),
			"enable",
		)
		plan = require(
			run(
				args.foch,
				[
					"plugin",
					"plan",
					"--playset",
					"offline",
					"--game-path",
					str(args.game),
					"--format",
					"json",
				],
				env,
			),
			"plan",
		)
		launch = require(
			run(
				args.foch,
				[
					"plugin",
					"launch",
					"--prepare-only",
					"--playset",
					"offline",
					"--game-path",
					str(args.game),
					"--host-dll",
					str(args.host),
					"--runtime-base",
					str(runtime),
					"--user-dir",
					str(user),
					"--format",
					"json",
				],
				env,
			),
			"launch --prepare-only",
		)
		record = json.loads(launch)
		if record.get("prepared_only") is not True or record.get("pid") is not None:
			raise SystemExit(f"launch started a process: {launch}")
		if "Started EU4" in launch:
			raise SystemExit(launch)
		plugins = json.dumps(record.get("artifacts"))
		if "foch_plugin_read_decide.dll" not in plugins and PLUGIN_ID not in plan:
			raise SystemExit(f"plan/launch did not stage the plugin\n{plan}\n{launch}")
	after = eu4_pids()
	started = after - before
	if started:
		raise SystemExit(f"eu4.exe started: {sorted(started)}")
	print("offline staging ok; eu4.exe was not started")


if __name__ == "__main__":
	main()
