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
	0x2736: "clr_country_flag",
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
		self.assertEqual(report["comparison"]["treatment"]["factory_pinned"], 0)
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


def load_bytes(blob: bytes):
	with tempfile.TemporaryDirectory() as temporary:
		path = Path(temporary) / "eu4.exe"
		path.write_bytes(blob)
		return read_binary(path)


def jump_dispatcher(base: int, limit: int, table_rva: int, default_rva: int) -> bytes:
	"""RIP-relative dword jump table placed at ``TEXT_RVA``."""
	lea_at = TEXT_RVA + 19
	ja_next = TEXT_RVA + 16
	return b"".join(
		(
			b"\x2d" + struct.pack("<I", base),
			b"\x3d" + struct.pack("<I", limit),
			b"\x0f\x87" + struct.pack("<i", default_rva - ja_next),
			b"\x48\x63\xc8",
			b"\x48\x8d\x15" + struct.pack("<i", table_rva - (lea_at + 7)),
			b"\x8b\x0c\x8a",
			b"\x48\x03\xca",
			b"\xff\xe1",
		)
	)


def jump_image(slot_targets: tuple[int, ...], *, limit: int) -> bytes:
	tokens = token_records()
	table_rva = RDATA_RVA + len(tokens)
	default = TEXT_RVA + 0x100
	code = jump_dispatcher(0x2710, limit, table_rva, default)
	table = b"".join(struct.pack("<i", target - table_rva) for target in slot_targets)
	functions: list[tuple[int, bytes]] = [(TEXT_RVA, code), (default, b"\xc3")]
	for target in dict.fromkeys(slot_targets):
		# A misaligned slot is the unverified case: it must not become a start.
		if target % 0x10 == 0:
			functions.append((target, b"\xc3"))
	return build_pe((), *functions, extra_rdata=tokens + table)


def factory_image(*, shared: bool = False, console: bool = False) -> bytes:
	functions = [TEXT_RVA + 0x40 + index * 0x20 for index in range(8)]
	tokens = token_records()
	body = bytearray()
	for index, function in enumerate(functions):
		target = functions[0] if shared else function
		body.extend(struct.pack("<IIQ", 0x2710 + index, 0, IMAGE_BASE + target))
	extra = bytearray(tokens)
	extra.extend(body)
	text = [(function, b"\xc3") for function in functions]
	if console:
		name_rva = RDATA_RVA + 4
		hint = b"<Amount>\0"
		hint_rva = RDATA_RVA + len(extra)
		extra.extend(hint)
		entry = bytearray(0x48)
		struct.pack_into("<Q", entry, 0, IMAGE_BASE + name_rva)
		struct.pack_into("<Q", entry, 0x30, IMAGE_BASE + functions[0])
		struct.pack_into("<Q", entry, 0x40, IMAGE_BASE + hint_rva)
		extra.extend(entry)
	return build_pe((), *text, extra_rdata=bytes(extra))


def thunk(token: int) -> bytes:
	return b"\xb8" + struct.pack("<I", token) + b"\xc3"


def vtable_image() -> bytes:
	flag = b"CSetCountryFlagEffect::Execute %s\0"
	clr = b"CClrCountryFlagEffect::Execute\0"
	# A leading NUL keeps each debug string a real C string. The previous
	# section's padding is 0x90, which would otherwise glue the first string on.
	prefix = bytearray(b"\0")
	flag_at = len(prefix)
	prefix.extend(flag)
	clr_at = len(prefix)
	prefix.extend(clr)
	prefix.extend(token_records())
	prefix.extend(b"\x00" * ((8 - (len(prefix) % 8)) % 8))
	thunks = (TEXT_RVA, TEXT_RVA + 0x20, TEXT_RVA + 0x40)
	execs = (TEXT_RVA + 0x100, TEXT_RVA + 0x140, TEXT_RVA + 0x180)
	shared = TEXT_RVA + 0x1C0
	tokens = (0x2735, 0x2736, 0x2710)
	table = bytearray()
	for thunk_rva, exec_rva in zip(thunks, execs):
		table.extend(
			struct.pack(
				"<QQQ",
				IMAGE_BASE + thunk_rva,
				IMAGE_BASE + shared,
				IMAGE_BASE + exec_rva,
			)
		)
	flag_rva = RDATA_RVA + flag_at
	clr_rva = RDATA_RVA + clr_at
	text = [
		(thunks[0], thunk(tokens[0])),
		(thunks[1], thunk(tokens[1])),
		(thunks[2], thunk(tokens[2])),
		(execs[0], lea_rdx(flag_rva, execs[0]) + b"\xc3"),
		(execs[1], lea_rdx(clr_rva, execs[1]) + b"\xc3"),
		(execs[2], b"\xc3"),
		(shared, b"\xc3"),
	]
	return build_pe((), *text, extra_rdata=bytes(prefix) + bytes(table))


class TokenDispatch(unittest.TestCase):
	def test_a_rip_relative_jump_table_pins_the_slot(self) -> None:
		handlers = (TEXT_RVA + 0x140, TEXT_RVA + 0x180)
		image = load_bytes(jump_image(handlers, limit=1))
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "pinned")
		self.assertEqual(treasury["failure"], None)
		self.assertEqual(treasury["handler"]["evidence"], "token_jump_table")
		self.assertEqual(treasury["handler"]["address"], IMAGE_BASE + handlers[0])
		self.assertEqual(report["comparison"]["treatment"]["pinned"], 1)
		self.assertEqual(
			report["primitives"]["set_country_flag"]["treatment"], "token_only"
		)

	def test_a_shared_jump_slot_is_not_a_handler(self) -> None:
		image = load_bytes(jump_image((TEXT_RVA + 0x140, TEXT_RVA + 0x140), limit=1))
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "dispatcher_ambiguous")
		self.assertIsNone(treasury["handler"])

	def test_a_jump_slot_that_is_not_a_function_refuses_the_table(self) -> None:
		image = load_bytes(jump_image((TEXT_RVA + 0x141, TEXT_RVA + 0x180), limit=1))
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "dispatcher_unverified")
		self.assertIsNone(treasury["handler"])

	def test_an_unbased_register_table_is_not_read(self) -> None:
		# `sub ecx, token; mov ecx, [r15+rax*4+disp]; add rcx, r15; jmp rcx`
		# names no RIP-relative table, so the base register stays unknown.
		tokens = token_records()
		code = bytes.fromhex(
			"81e91027000083f9010f87000000004863c1418b8c87880000004903cfffe1"
		)
		image = load_bytes(
			build_pe(
				(),
				(TEXT_RVA, code),
				(TEXT_RVA + 0x80, b"\xc3"),
				extra_rdata=tokens,
			)
		)
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "handler_unproven")

	def test_a_unique_factory_pair_pins_the_factory(self) -> None:
		image = load_bytes(factory_image())
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "factory_pinned")
		self.assertEqual(treasury["handler"]["evidence"], "token_factory_pair")
		self.assertEqual(treasury["handler"]["address"], IMAGE_BASE + TEXT_RVA + 0x40)
		self.assertEqual(report["comparison"]["treatment"]["factory_pinned"], 1)
		self.assertEqual(
			report["primitives"]["set_country_flag"]["treatment"], "token_only"
		)

	def test_a_shared_factory_function_is_ambiguous(self) -> None:
		image = load_bytes(factory_image(shared=True))
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "dispatcher_ambiguous")
		self.assertIsNone(treasury["handler"])

	def test_a_factory_that_is_a_console_command_is_refused(self) -> None:
		image = load_bytes(factory_image(console=True))
		report = review_catalog(image, catalog(), game_version="1.37.5")
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(treasury["baseline"], "rejected_console_cmd")
		self.assertEqual(treasury["treatment"], "token_only")
		self.assertEqual(treasury["failure"], "dispatcher_console")
		self.assertIsNone(treasury["handler"])
		self.assertEqual(treasury["console_handlers"], [IMAGE_BASE + TEXT_RVA + 0x40])

	def test_a_vtable_slot_learned_from_debug_strings_pins_the_third(self) -> None:
		image = load_bytes(vtable_image())
		source = catalog()
		source["primitives"]["clr_country_flag"] = {"kind": "effect", "facts": {}}
		report = review_catalog(image, source, game_version="1.37.5")
		flag = report["primitives"]["set_country_flag"]
		clr = report["primitives"]["clr_country_flag"]
		treasury = report["primitives"]["add_treasury"]
		self.assertEqual(flag["treatment"], "pinned")
		self.assertEqual(flag["handler"]["evidence"], "debug_string")
		self.assertEqual(clr["treatment"], "pinned")
		self.assertEqual(clr["handler"]["evidence"], "debug_string")
		self.assertEqual(treasury["treatment"], "pinned")
		self.assertEqual(treasury["failure"], None)
		self.assertEqual(treasury["handler"]["evidence"], "token_vtable")
		self.assertEqual(treasury["handler"]["address"], IMAGE_BASE + TEXT_RVA + 0x180)
		self.assertNotIn(treasury["handler"]["address"], treasury["console_handlers"])


if __name__ == "__main__":
	unittest.main()
