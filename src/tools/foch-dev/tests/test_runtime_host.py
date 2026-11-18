from __future__ import annotations

import struct
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from foch_dev.runtime_host import (
	PROXY_EXPORTS,
	exports,
	validate_host,
	validate_plugin,
	write_bundle,
)


def dll(names: list[str]) -> bytes:
	data = bytearray(0x1200)
	data[:2] = b"MZ"

	def u16(at: int, value: int) -> None:
		struct.pack_into("<H", data, at, value)

	def u32(at: int, value: int) -> None:
		struct.pack_into("<I", data, at, value)

	u32(0x3C, 0x80)
	data[0x80:0x84] = b"PE\0\0"
	u16(0x84, 0x8664)
	u16(0x86, 1)
	u16(0x94, 240)
	u16(0x96, 0x2000)
	u16(0x98, 0x20B)
	u32(0x98 + 16, 0x1900)
	u32(0x98 + 32, 0x1000)
	u32(0x98 + 36, 0x200)
	u32(0x98 + 56, 0x2000)
	u32(0x98 + 60, 0x200)
	u32(0x98 + 108, 1)
	u32(0x98 + 112, 0x1000)
	u32(0x98 + 116, 0x800)
	section = 0x98 + 240
	u32(section + 8, 0x1000)
	u32(section + 12, 0x1000)
	u32(section + 16, 0x1000)
	u32(section + 20, 0x200)
	u32(section + 36, 0x60000020)
	for offset, value in [
		(16, 1),
		(20, len(names)),
		(24, len(names)),
		(28, 0x1040),
		(32, 0x10C0),
		(36, 0x1140),
	]:
		u32(0x200 + offset, value)
	text = 0x400
	for index, name in enumerate(names):
		u32(0x240 + index * 4, 0x1900 + index)
		u32(0x2C0 + index * 4, text - 0x200 + 0x1000)
		u16(0x340 + index * 2, index)
		data[text : text + len(name)] = name.encode("ascii")
		text += len(name) + 1
	return bytes(data)


class RuntimeHostTests(unittest.TestCase):
	def test_actual_export_roles_must_be_distinct(self):
		host = dll(list(PROXY_EXPORTS))
		plugin = dll(["foch_plugin_get_api"])
		validate_host(host)
		validate_plugin(plugin)
		self.assertEqual(exports(host)["VerQueryValueW"], 17)
		for validator, data in [
			(validate_host, plugin),
			(validate_plugin, host),
			(validate_host, dll([*PROXY_EXPORTS, "foch_plugin_get_api"])),
			(validate_plugin, dll([*PROXY_EXPORTS, "foch_plugin_get_api"])),
		]:
			with (
				self.subTest(validator=validator.__name__),
				self.assertRaises(ValueError),
			):
				validator(data)

	def test_corrupt_or_forwarded_exports_cannot_be_packaged(self):
		data = dll(["foch_plugin_get_api"])
		for end in range(len(data)):
			with self.assertRaises(ValueError):
				validate_plugin(data[:end])
		for offset, value in [
			(0x240, 0x1100),
			(0x240, 0xFFFFFF00),
			(0x2C0, 0xFFFFFF00),
		]:
			corrupt = bytearray(data)
			struct.pack_into("<I", corrupt, offset, value)
			with self.assertRaises(ValueError):
				validate_plugin(bytes(corrupt))

	def test_bundle_maps_filename_binds_bytes_and_refuses_overwrite(self):
		with tempfile.TemporaryDirectory() as scratch:
			root = Path(scratch)
			for name in [
				"LICENSE",
				"LICENSE-MERGIRAF.txt",
				"NOTICE.md",
				"THIRD-PARTY-LICENSES.txt",
			]:
				(root / name).write_text(f"{name}\n")
			readme = root / "src/packages/foch-runtime-host/README.md"
			readme.parent.mkdir(parents=True)
			readme.write_text("Host installation\n")
			host = root / "foch_runtime_host.dll"
			host.write_bytes(dll(list(PROXY_EXPORTS)))
			with mock.patch.dict("os.environ", {"SOURCE_DATE_EPOCH": "315532800"}):
				archive = write_bundle(host, root / "out", root, "0.0.2")
				second = write_bundle(host, root / "other", root, "0.0.2")
			self.assertEqual(archive.name, "foch-runtime-host-0.0.2-win32-x64.zip")
			self.assertEqual(archive.read_bytes(), second.read_bytes())
			with zipfile.ZipFile(archive) as bundle:
				self.assertEqual(bundle.read("foch_runtime.dll"), host.read_bytes())
				self.assertNotIn("foch_runtime_host.dll", bundle.namelist())
				self.assertIn(
					"foch_runtime.dll", bundle.read("SHA256SUMS.txt").decode()
				)
			with self.assertRaises(FileExistsError):
				write_bundle(host, root / "out", root, "0.0.2")
			host.write_bytes(dll(["foch_plugin_get_api"]))
			with self.assertRaises(ValueError):
				write_bundle(host, root / "bad", root, "0.0.2")


if __name__ == "__main__":
	unittest.main()
