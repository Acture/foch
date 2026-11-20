"""Package Tribute Adjust into a fresh directory for ``foch plugin import``.

The output directory is created only after the plugin DLL passes an offline
PE export check. An existing directory, file, or symlink is refused. No DLL
is loaded or executed.

Example::

    python mods/tribute-adjust/tools/package.py target/tribute-adjust-pkg
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "src/tools/foch-dev"))

PLUGIN_DLL = "foch_eu4_tribute.dll"
PANEL_EXE = "tribute-panel.exe"
SIGNATURES = Path("signatures/signatures-1.37.5.toml")
HOST_EXPORTS = (
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
ENTRY = "foch_plugin_get_api"


def sha256(data: bytes) -> str:
	return hashlib.sha256(data).hexdigest()


def newest(paths: list[Path]) -> Path | None:
	found = [path for path in paths if path.is_file()]
	if not found:
		return None
	return max(found, key=lambda path: path.stat().st_mtime)


def resolve_dll(explicit: Path | None) -> Path:
	if explicit is not None:
		if not explicit.is_file():
			raise SystemExit(f"plugin DLL not found: {explicit}")
		return explicit
	found = newest(
		[
			REPO / "target/release" / PLUGIN_DLL,
			REPO / "target/debug" / PLUGIN_DLL,
			ROOT / PLUGIN_DLL,
		]
	)
	if found is None:
		raise SystemExit(
			f"missing {PLUGIN_DLL}; build foch-eu4-tribute or pass --dll"
		)
	return found


def resolve_panel(explicit: Path | None, include: bool) -> Path | None:
	if not include:
		return None
	if explicit is not None:
		if not explicit.is_file():
			raise SystemExit(f"tribute panel not found: {explicit}")
		return explicit
	return newest(
		[
			REPO / "target/release" / PANEL_EXE,
			REPO / "target/debug" / PANEL_EXE,
		]
	)


def _u16(data: bytes, at: int) -> int:
	return int.from_bytes(data[at : at + 2], "little")


def _u32(data: bytes, at: int) -> int:
	return int.from_bytes(data[at : at + 4], "little")


def _slice(data: bytes, at: int, size: int) -> bytes:
	end = at + size
	if at < 0 or size < 0 or end > len(data):
		raise ValueError("truncated PE image")
	return data[at:end]


def _map_rva(sections: list[tuple[int, int, int]], headers: int, rva: int, size: int) -> int:
	matches: list[int] = []
	if rva < headers and rva + size <= headers:
		matches.append(rva)
	for virtual, raw, length in sections:
		offset = rva - virtual
		if 0 <= offset and offset + size <= length:
			matches.append(raw + offset)
	if len(matches) != 1:
		raise ValueError("PE RVA is absent, ambiguous or outside file-backed sections")
	return matches[0]


def export_names_stdlib(data: bytes) -> set[str]:
	"""Read AMD64 DLL export names without loading the image or using pefile."""
	if _slice(data, 0, 2) != b"MZ":
		raise ValueError("DLL has no DOS header")
	pe = _u32(data, 0x3C)
	if _slice(data, pe, 4) != b"PE\0\0" or _u16(data, pe + 4) != 0x8664:
		raise ValueError("DLL must be an AMD64 PE image")
	if _u16(data, pe + 22) & 0x2000 == 0:
		raise ValueError("DLL must be an AMD64 PE image")
	optional = pe + 24
	optional_size = _u16(data, pe + 20)
	header = _slice(data, optional, optional_size)
	if _u16(header, 0) != 0x20B or len(header) < 120 or _u32(header, 108) == 0:
		raise ValueError("DLL has no PE32+ export directory")
	count = _u16(data, pe + 6)
	if not 1 <= count <= 96:
		raise ValueError("invalid PE section count")
	headers = _u32(header, 60)
	_slice(data, 0, headers)
	sections: list[tuple[int, int, int]] = []
	table = optional + optional_size
	_slice(data, table, count * 40)
	for index in range(count):
		at = table + index * 40
		virtual = _u32(data, at + 12)
		raw = _u32(data, at + 20)
		length = _u32(data, at + 16)
		_slice(data, raw, length)
		sections.append((virtual, raw, length))
	export_rva = _u32(header, 112)
	export_size = _u32(header, 116)
	if export_rva == 0 or export_size < 40:
		raise ValueError("DLL has no export table")
	export = _map_rva(sections, headers, export_rva, export_size)
	functions = _u32(data, export + 20)
	names = _u32(data, export + 24)
	if functions == 0 or functions > 65536 or names == 0 or names > functions:
		raise ValueError("invalid PE export counts")
	name_pointers = _map_rva(sections, headers, _u32(data, export + 32), names * 4)
	result: set[str] = set()
	for index in range(names):
		name_rva = _u32(data, name_pointers + index * 4)
		at = _map_rva(sections, headers, name_rva, 1)
		end = data.find(b"\0", at, min(len(data), at + 256))
		if end <= at:
			raise ValueError("invalid PE export name")
		name = data[at:end]
		if not name.isascii():
			raise ValueError("invalid PE export name")
		text = name.decode("ascii")
		if text in result:
			raise ValueError("duplicate PE export name")
		result.add(text)
	return result


def validate_plugin_stdlib(data: bytes) -> None:
	symbols = export_names_stdlib(data)
	if ENTRY not in symbols:
		raise ValueError("plugin DLL is missing foch_plugin_get_api")
	if any(name in symbols for name in HOST_EXPORTS):
		raise ValueError("plugin DLL contains VERSION host exports")


def validate_plugin_bytes(data: bytes) -> None:
	try:
		from foch_dev.runtime_host import validate_plugin
	except ImportError:
		validate_plugin_stdlib(data)
		return
	validate_plugin(data)


def refuse_existing(path: Path) -> None:
	try:
		path.lstat()
	except FileNotFoundError:
		return
	kind = "symlink" if path.is_symlink() else "existing path"
	raise SystemExit(f"refusing {kind}: {path}")


def create_fresh(path: Path) -> None:
	refuse_existing(path)
	path.parent.mkdir(parents=True, exist_ok=True)
	# A directory created by a racing writer, including a symlink, fails here.
	path.mkdir(exist_ok=False)


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("output", type=Path, help="Fresh package directory to create")
	parser.add_argument("--dll", type=Path, help="foch_eu4_tribute.dll to package")
	parser.add_argument("--panel", type=Path, help="tribute-panel.exe to package")
	parser.add_argument(
		"--no-panel",
		action="store_true",
		help="Do not package tribute-panel.exe even if a build is present",
	)
	args = parser.parse_args()

	dll_path = resolve_dll(args.dll)
	dll = dll_path.read_bytes()
	if not dll.startswith(b"MZ"):
		parser.error("plugin input must be a Windows PE image")
	try:
		validate_plugin_bytes(dll)
	except ValueError as error:
		parser.error(str(error))

	panel_path = resolve_panel(args.panel, include=not args.no_panel)
	manifest_path = ROOT / "foch-plugin.toml"
	signature_path = ROOT / SIGNATURES
	license_path = REPO / "LICENSE"
	for required in (manifest_path, signature_path, license_path):
		if not required.is_file():
			parser.error(f"missing package input: {required}")

	files: list[tuple[str, bytes]] = [
		("foch-plugin.toml", manifest_path.read_bytes()),
		(PLUGIN_DLL, dll),
		(SIGNATURES.as_posix(), signature_path.read_bytes()),
		("LICENSE", license_path.read_bytes()),
	]
	if panel_path is not None:
		files.append((PANEL_EXE, panel_path.read_bytes()))

	create_fresh(args.output)
	for relative, data in files:
		destination = args.output / relative
		destination.parent.mkdir(parents=True, exist_ok=True)
		destination.write_bytes(data)
	sums = "".join(
		f"{sha256(data)}  {name}\n" for name, data in sorted(files, key=lambda item: item[0])
	)
	(args.output / "SHA256SUMS.txt").write_text(sums, encoding="utf-8", newline="\n")
	print(args.output.resolve())
	if panel_path is None:
		print("tribute-panel.exe was not built; package omits it")


if __name__ == "__main__":
	main()
