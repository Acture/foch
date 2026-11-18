"""Validate independent Windows DLL roles and package the optional Foch host.

No DLL is loaded or executed. PE offsets follow Microsoft's PE/COFF export
directory specification; the same roles are checked before runtime staging.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import stat
import time
import zipfile
from pathlib import Path

import pefile

from .dist import source_date_epoch
from .repository import find_repository
from .versions import releasable_version, workspace_version

PROXY_EXPORTS: tuple[str, ...] = (
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
ENTRY: str = "foch_plugin_get_api"
DISTRIBUTED_DLL: str = "foch_runtime.dll"
LICENSES: tuple[str, ...] = (
	"LICENSE",
	"LICENSE-MERGIRAF.txt",
	"NOTICE.md",
	"THIRD-PARTY-LICENSES.txt",
)


def exports(data: bytes) -> dict[str, int]:
	"""Read export roles through pefile; no native DLL loader is involved."""
	try:
		image = pefile.PE(data=data, fast_load=True)
	except pefile.PEFormatError as error:
		raise ValueError(f"invalid PE image: {error}") from error
	try:
		if (
			image.FILE_HEADER.Machine != 0x8664
			or not image.FILE_HEADER.Characteristics & 0x2000
			or image.OPTIONAL_HEADER.Magic != 0x20B
		):
			raise ValueError("DLL must be an AMD64 PE32+ image")
		if image.OPTIONAL_HEADER.SizeOfHeaders > len(data):
			raise ValueError("truncated PE sections")
		for section in image.sections:
			raw, size = section.PointerToRawData, section.SizeOfRawData
			if (
				not isinstance(raw, int)
				or not isinstance(size, int)
				or raw < 0
				or size < 0
				or raw + size > len(data)
			):
				raise ValueError("truncated PE sections")
		if not image.OPTIONAL_HEADER.DATA_DIRECTORY:
			raise ValueError("DLL has no export directory")
		directory = image.OPTIONAL_HEADER.DATA_DIRECTORY[0]
		if not directory.VirtualAddress or directory.Size < 40:
			raise ValueError("DLL has no export directory")
		# Read the export directory directly: a valid export-only PE need not
		# carry an import directory before pefile's directory-dispatch loop.
		table = image.parse_export_directory(directory.VirtualAddress, directory.Size)
		if (
			table is None
			or not 0
			< table.struct.NumberOfNames
			<= table.struct.NumberOfFunctions
			<= 65536
		):
			raise ValueError("DLL has no valid export table")
		result: dict[str, int] = {}
		for symbol in table.symbols:
			if symbol.name is None:
				continue
			name = symbol.name.decode("ascii")
			section = image.get_section_by_rva(symbol.address)
			if (
				symbol.forwarder is not None
				or section is None
				or not section.Characteristics & 0x20000000
				or not 0
				<= symbol.address - section.VirtualAddress
				< section.SizeOfRawData
			):
				raise ValueError("DLL export is forwarded or outside executable bytes")
			if name in result or not 0 <= symbol.ordinal <= 0xFFFFFFFF:
				raise ValueError("duplicate PE export name or invalid ordinal")
			result[name] = symbol.ordinal
		if len(result) != table.struct.NumberOfNames:
			raise ValueError("incomplete PE export names")
		return result
	except pefile.PEFormatError as error:
		raise ValueError(f"invalid PE exports: {error}") from error
	finally:
		image.close()


def validate_host(data: bytes) -> None:
	symbols = exports(data)
	if ENTRY in symbols:
		raise ValueError("host DLL also contains a plugin entry")
	for ordinal, name in enumerate(PROXY_EXPORTS, 1):
		if symbols.get(name) != ordinal:
			raise ValueError(f"host DLL is missing {name} at ordinal {ordinal}")


def validate_plugin(data: bytes) -> None:
	symbols = exports(data)
	if ENTRY not in symbols:
		raise ValueError("plugin DLL is missing foch_plugin_get_api")
	if any(name in symbols for name in PROXY_EXPORTS):
		raise ValueError("plugin DLL contains VERSION host exports")


def write_bundle(host: Path, output: Path, repo: Path, version: str) -> Path:
	version = releasable_version(version)
	data = host.read_bytes()
	validate_host(data)
	files = {name: (repo / name).read_bytes() for name in LICENSES}
	files[DISTRIBUTED_DLL] = data
	files["README.md"] = (
		repo / "src/packages/foch-runtime-host/README.md"
	).read_bytes()
	files["SHA256SUMS.txt"] = "".join(
		f"{hashlib.sha256(content).hexdigest()}  {name}\n"
		for name, content in sorted(files.items())
	).encode()
	output.mkdir(parents=True, exist_ok=True)
	archive = output / f"foch-runtime-host-{version}-win32-x64.zip"
	date = time.gmtime(source_date_epoch(os.environ))[:6]
	with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as bundle:
		for name, content in sorted(files.items()):
			info = zipfile.ZipInfo(name, date_time=date)
			info.create_system = 3
			info.external_attr = (stat.S_IFREG | 0o644) << 16
			info.compress_type = zipfile.ZIP_DEFLATED
			bundle.writestr(info, content)
	return archive


def add_arguments(parser: argparse.ArgumentParser) -> None:
	commands = parser.add_subparsers(dest="host_command", required=True)
	check = commands.add_parser(
		"check", help="Validate host/plugin PE exports without executing DLLs"
	)
	check.add_argument("--host", type=Path)
	check.add_argument("--plugin", type=Path, action="append", default=[])
	bundle = commands.add_parser(
		"bundle", help="Package an independently built Windows host"
	)
	bundle.add_argument("--host", type=Path, required=True)
	bundle.add_argument("--out", type=Path, required=True)
	bundle.add_argument("--repo", type=Path)


def run(args: argparse.Namespace) -> int:
	if args.host_command == "check":
		if not args.host and not args.plugin:
			raise ValueError("pass --host and/or --plugin")
		if args.host:
			validate_host(args.host.read_bytes())
		for plugin in args.plugin:
			validate_plugin(plugin.read_bytes())
		print("Host/plugin DLL exports validated")
	else:
		repo = find_repository(args.repo)
		print(write_bundle(args.host, args.out, repo, workspace_version(repo)))
	return 0
