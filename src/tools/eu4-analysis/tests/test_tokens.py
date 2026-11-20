"""Token-table discovery, the gold-id gate, and the offline pin review."""

from __future__ import annotations

import json
import struct
import tempfile
import unittest
from pathlib import Path

from test_pe import IMAGE_BASE, RDATA_RVA, TEXT_RVA, build_pe, lea_rdx

from eu4_analysis.pe import read_binary
from eu4_analysis.semantics import review_catalog
from eu4_analysis.tokens import (
	GOLD_TOKENS,
	calibrate,
	discover_token_table,
	summarize_prologue,
)

STRIDE = 0x20
FIRST = 0x2710
LAST = 0x2AA9
NAMED = {
	0x2710: "add_treasury",
	0x2735: "set_country_flag",
	0x2800: "random",
	0x28A0: "army",
	0x28A2: "regiment",
	0x28A8: "morale",
	0x28A9: "strength",
	0x2AA8: "outgoing",
	0x2AA9: "members",
}


def token_records() -> bytes:
	blob = bytearray()
	for token in range(FIRST, LAST + 1):
		name = NAMED.get(token, f"t{token:04x}")
		record = struct.pack("<I", token) + name.encode("ascii") + b"\0"
		blob.extend(record.ljust(STRIDE, b"\0"))
	return bytes(blob)


def fixture_image():
	"""A PE whose .rdata holds the gold tokens plus one real debug string."""
	decoy = b"alpha\0beta\0gamma\0"
	table = token_records()
	pad = b"\x01"
	console_name = b"set_country_flag\0"
	hint = b"<Flag>\0"
	debug = b"CSetCountryFlagEffect::Execute %s, %s\0"
	misleading = b"CRandomVariableEffect::Execute\0"
	cursor = len(decoy) + len(table) + len(pad)
	console_at = cursor
	cursor += len(console_name)
	hint_at = cursor
	cursor += len(hint)
	debug_at = cursor
	cursor += len(debug)
	misleading_at = cursor
	cursor += len(misleading)
	entry = bytearray(0x50)
	struct.pack_into("<Q", entry, 8, IMAGE_BASE + RDATA_RVA + console_at)
	struct.pack_into("<Q", entry, 8 + 0x30, IMAGE_BASE + TEXT_RVA + 0x40)
	struct.pack_into("<Q", entry, 8 + 0x40, IMAGE_BASE + RDATA_RVA + hint_at)
	extra = (
		decoy + table + pad + console_name + hint + debug + misleading + bytes(entry)
	)
	blob = build_pe(
		(),
		(TEXT_RVA, lea_rdx(RDATA_RVA + debug_at, TEXT_RVA)),
		(TEXT_RVA + 0x20, lea_rdx(RDATA_RVA + misleading_at, TEXT_RVA + 0x20)),
		(TEXT_RVA + 0x40, b"\x90" * 0x10),
		extra_rdata=extra,
	)
	return blob


def catalog() -> dict:
	def primitive(kind: str) -> dict:
		return {"kind": kind, "facts": {}, "handler": None}

	return {
		"format": 1,
		"status": "unpinned",
		"primitives": {
			"set_country_flag": primitive("effect"),
			"add_treasury": primitive("effect"),
			"has_variable": primitive("trigger"),
			"random": primitive("effect"),
		},
	}


def load_fixture():
	with tempfile.TemporaryDirectory() as temporary:
		path = Path(temporary) / "eu4.exe"
		path.write_bytes(fixture_image())
		image = read_binary(path)
		# read_binary keeps the bytes; the file can disappear with the directory.
		return image


class TokenTableDiscovery(unittest.TestCase):
	def test_gold_ids_are_the_cross_repo_control(self) -> None:
		self.assertEqual(
			GOLD_TOKENS,
			{
				"outgoing": 0x2AA8,
				"members": 0x2AA9,
				"army": 0x28A0,
				"regiment": 0x28A2,
				"strength": 0x28A9,
				"morale": 0x28A8,
			},
		)

	def test_a_synthetic_table_calibrates_six_of_six(self) -> None:
		image = load_fixture()
		table = discover_token_table(image)
		self.assertIsNotNone(table)
		assert table is not None
		self.assertEqual(table.section, ".rdata")
		self.assertEqual(table.stride, STRIDE)
		self.assertEqual(table.id_width, 4)
		result = calibrate(table)
		self.assertTrue(result.passed)
		self.assertEqual(result.matched, 6)
		self.assertEqual(result.total, 6)
		for name, expected, found in result.checks:
			self.assertEqual(found, expected, name)

	def test_a_wrong_gold_id_fails_the_gate(self) -> None:
		image = load_fixture()
		table = discover_token_table(image)
		wrong = dict(GOLD_TOKENS)
		wrong["outgoing"] = 0x2AA7
		result = calibrate(table, wrong)
		self.assertFalse(result.passed)
		self.assertEqual(result.matched, 5)

	def test_prologue_of_the_known_effect_bytes(self) -> None:
		raw = bytes.fromhex(
			"48895c240855565741564157488dac2460ffffff4881eca0010000488bfa"
		)
		text = summarize_prologue(raw, limit=9)
		self.assertIn("mov qword ptr [rsp+0x8], rbx", text)
		self.assertIn("push rbp", text)
		self.assertIn("lea rbp, [rsp-0xa0]", text)
		self.assertIn("sub rsp, 0x1a0", text)
		self.assertIn("mov rdi, rdx", text)


class Review(unittest.TestCase):
	def test_token_id_alone_is_not_a_pin_and_console_is_rejected(self) -> None:
		image = load_fixture()
		report = review_catalog(image, catalog(), game_version="1.37.5")
		self.assertTrue(report["calibration"]["passed"])
		self.assertEqual(report["comparison"]["total"], 4)
		self.assertEqual(report["comparison"]["baseline"]["pinned"], 0)
		self.assertEqual(report["comparison"]["baseline"]["rejected_console_cmd"], 1)
		self.assertEqual(report["comparison"]["treatment"]["pinned"], 1)
		self.assertEqual(report["comparison"]["treatment"]["token_only"], 2)
		self.assertEqual(report["comparison"]["treatment"]["unpinned"], 1)

		pinned = report["primitives"]["set_country_flag"]
		self.assertEqual(pinned["treatment"], "pinned")
		self.assertEqual(pinned["baseline"], "rejected_console_cmd")
		self.assertEqual(pinned["token_id"], 0x2735)
		self.assertIsNone(pinned["failure"])
		handler = pinned["handler"]
		self.assertEqual(handler["address"], IMAGE_BASE + TEXT_RVA)
		self.assertEqual(handler["rva"], TEXT_RVA)
		self.assertEqual(handler["evidence"], "debug_string")
		self.assertIn("CSetCountryFlagEffect::Execute", handler["debug_string"])
		self.assertEqual(pinned["console_handlers"], [IMAGE_BASE + TEXT_RVA + 0x40])

		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "handler_unproven")
		self.assertEqual(treasury["token_id"], 0x2710)
		self.assertIsNone(treasury["handler"])

		# The class string names random_variable, not the catalog primitive.
		random = report["primitives"]["random"]
		self.assertEqual(random["treatment"], "token_only")
		self.assertEqual(random["failure"], "handler_unproven")
		self.assertEqual(random["token_id"], 0x2800)
		self.assertIsNone(random["handler"])

		missing = report["primitives"]["has_variable"]
		self.assertEqual(missing["treatment"], "unpinned")
		self.assertEqual(missing["failure"], "token_absent")
		self.assertIsNone(missing["token_id"])
		self.assertEqual(missing["baseline"], "string_absent")

	def test_an_uncalibrated_table_does_not_publish_token_ids(self) -> None:
		image = load_fixture()
		wrong = dict(GOLD_TOKENS)
		wrong["army"] = 0x28A1
		report = review_catalog(image, catalog(), game_version="1.37.5", gold=wrong)
		self.assertFalse(report["calibration"]["passed"])
		flag = report["primitives"]["set_country_flag"]
		self.assertEqual(flag["treatment"], "pinned")
		self.assertIsNone(flag["token_id"])
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "unpinned")
		self.assertEqual(treasury["failure"], "token_table_uncalibrated")
		self.assertIsNone(treasury["token_id"])


class OfflineCommand(unittest.TestCase):
	def test_the_cli_writes_the_review_without_ghidra(self) -> None:
		from eu4_analysis import __main__ as entry

		with tempfile.TemporaryDirectory() as temporary:
			base = Path(temporary)
			game = base / "game"
			game.mkdir()
			binary = game / "eu4.exe"
			binary.write_bytes(fixture_image())
			source = base / "semantics.json"
			source.write_text(json.dumps(catalog()), encoding="utf-8")
			output = base / "out" / "semantics-pin-review.json"
			options = entry.parse_args(
				[
					"semantics",
					"--offline",
					"--catalog",
					str(source),
					"--output",
					str(output),
					"--binary",
					str(binary),
					"--game-version",
					"1.37.5",
				]
			)
			entry.run(options)
			written = json.loads(output.read_text(encoding="utf-8"))
		self.assertEqual(written["kind"], "semantics-pin-review")
		self.assertTrue(written["calibration"]["passed"])
		self.assertEqual(written["comparison"]["treatment"]["pinned"], 1)


if __name__ == "__main__":
	unittest.main()
