from __future__ import annotations

import fnmatch
import hashlib
import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from typing import Self

from eu4_analysis.models import Function, FunctionEvidence, Instruction, Reference
from eu4_analysis.semantics import (
	DEFAULT_EFFECT_PATTERNS,
	SemanticsQuery,
	load_catalog,
	locate,
	pin,
	pin_catalog,
)


def instruction(op: str, *operands: str) -> Instruction:
	return Instruction(0, op, operands)


def literal(register: str, value: str) -> Instruction:
	return Instruction(
		0, "LEA", (register, "[0x9000]"), data=(Reference(0x9000, string=value),)
	)


def handler_address(register: str, address: int) -> Instruction:
	return Instruction(0, "LEA", (register, f"[{address:#x}]"))


def function(
	name: str, address: int, *instructions: Instruction, pseudocode: str | None = None
) -> FunctionEvidence:
	return FunctionEvidence(
		Function(address, address + 0x100, (name,)),
		tuple(
			replace(item, address=address + index)
			for index, item in enumerate(instructions)
		),
		pseudocode,
	)


class FakeAnalysis:
	def __init__(self, *functions: FunctionEvidence) -> None:
		self.functions: dict[int, FunctionEvidence] = {
			item.function.address: item for item in functions
		}

	def find(self, pattern: str) -> tuple[Function, ...]:
		return tuple(
			item.function
			for item in self.functions.values()
			if any(fnmatch.fnmatchcase(name, pattern) for name in item.function.names)
		)

	def at(self, address: int) -> Function:
		return self.functions[address].function

	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence:
		evidence = self.functions[function.address]
		if pseudocode and evidence.pseudocode is None:
			raise ValueError(f"no pseudocode at {function.address:#x}")
		return evidence


REGISTRY = function(
	"CEffectDatabase::RegisterAll",
	0x1000,
	literal("RDI", "set_country_flag"),
	handler_address("RDX", 0x2000),
	instruction("CALL", "RAX"),
)


def analysis() -> FakeAnalysis:
	return FakeAnalysis(
		REGISTRY,
		function(
			"CCountry::SetCountryFlag",
			0x2000,
			pseudocode="void SetCountryFlag(flags) { flags.insert(name); }",
		),
	)


class SymbollessAnalysis(FakeAnalysis):
	"""A stripped Windows image: no symbols, a PEImage with raw bytes."""

	def __init__(self, *functions: FunctionEvidence, image: object) -> None:
		super().__init__(*functions)
		self.image: object = image

	def find(self, pattern: str) -> tuple[Function, ...]:
		return ()


def catalog_file(directory: Path, primitives: dict) -> Path:
	path: Path = directory / "semantics.json"
	path.write_text(
		json.dumps({"format": 1, "status": "unpinned", "primitives": primitives}),
		encoding="utf-8",
	)
	return path


class CliEntry(unittest.TestCase):
	"""The registered `python -m eu4_analysis semantics` entry, end to end.

	The GhidraSession is mocked with the same fake the unit tests use; the
	argument parsing, the command dispatch and the artifact write are the
	real production path.
	"""

	def test_the_registered_command_pins_through_the_real_entry(self) -> None:
		import types
		import unittest.mock

		from eu4_analysis import __main__ as entry

		fake = analysis()
		with tempfile.TemporaryDirectory() as temporary:
			base: Path = Path(temporary)
			catalog: Path = catalog_file(
				base,
				{
					"set_country_flag": {"kind": "effect", "facts": {}, "handler": None},
					"clr_country_flag": {"kind": "effect", "facts": {}, "handler": None},
				},
			)
			output: Path = base / "out" / "semantics.json"

			class FakeSession:
				"""Forwards the analysis protocol and an image identity."""

				def __init__(self, *args: object) -> None:
					pass

				def __enter__(self) -> Self:
					return self

				def __exit__(self, *args: object) -> None:
					pass

				image = types.SimpleNamespace(sha256="ab" * 32)
				find = staticmethod(fake.find)
				at = staticmethod(fake.at)
				inspect = staticmethod(fake.inspect)

			with unittest.mock.patch.object(entry, "GhidraSession", FakeSession):
				options = entry.parse_args(
					[
						"semantics",
						"--catalog",
						str(catalog),
						"--output",
						str(output),
						"--binary",
						str(base / "eu4.exe"),
						"--game-version",
						"1.37.5",
						"--ghidra",
						str(base),
					]
				)
				entry.run(options)
			written: dict = json.loads(output.read_text(encoding="utf-8"))
			# set_country_flag has a registration; clr_country_flag does not,
			# so the artifact stays unpinned and names the missing one.
			self.assertEqual(written["status"], "unpinned")
			self.assertEqual(written["missing"], ["clr_country_flag"])
			handler: dict | None = written["primitives"]["set_country_flag"]["handler"]
			self.assertIsNotNone(handler)
			self.assertEqual(handler["address"], 0x2000)


class LocateHandlers(unittest.TestCase):
	def test_a_registration_pins_the_handler_argument(self) -> None:
		handler = locate(analysis(), "set_country_flag", DEFAULT_EFFECT_PATTERNS)
		self.assertIsNotNone(handler)
		assert handler is not None
		self.assertEqual(handler.address, 0x2000)
		self.assertEqual(handler.names, ("CCountry::SetCountryFlag",))

	def test_an_unknown_keyword_finds_nothing(self) -> None:
		self.assertIsNone(locate(analysis(), "absent", DEFAULT_EFFECT_PATTERNS))

	def test_a_stripped_pe_image_locates_through_string_references(self) -> None:
		"""The Windows path: no symbols, discovery by keyword reference."""
		from test_pe import RDATA_RVA, TEXT_RVA, build_pe, registry_function

		from eu4_analysis.pe import read_binary

		begin, code = registry_function("set_country_flag", RDATA_RVA, TEXT_RVA + 0x100)
		blob: bytes = build_pe(("set_country_flag",), (begin, code))
		with tempfile.TemporaryDirectory() as temporary:
			path = Path(temporary) / "eu4.exe"
			path.write_bytes(blob)
			image = read_binary(path)
		# The registration function is found through the PE string reference;
		# its traced instructions name the keyword and the handler address.
		from test_pe import IMAGE_BASE

		register = function(
			"sub_1000",
			IMAGE_BASE + TEXT_RVA,
			literal("RDI", "set_country_flag"),
			handler_address("RDX", IMAGE_BASE + TEXT_RVA + 0x100),
			instruction("CALL", "RAX"),
			pseudocode="void Register() { Add(set_country_flag, SetCountryFlag); }",
		)
		handler_body = function(
			"sub_1100",
			IMAGE_BASE + TEXT_RVA + 0x100,
			pseudocode="void SetCountryFlag(flags) { flags.insert(name); }",
		)
		symbolless = SymbollessAnalysis(register, handler_body, image=image)
		handler = locate(symbolless, "set_country_flag", DEFAULT_EFFECT_PATTERNS)
		self.assertIsNotNone(handler)
		assert handler is not None
		self.assertEqual(handler.address, IMAGE_BASE + TEXT_RVA + 0x100)

	def test_pin_records_the_pseudocode_digest(self) -> None:
		pinned = pin(
			analysis(), Function(0x2000, 0x2100, ("CCountry::SetCountryFlag",))
		)
		self.assertEqual(pinned["symbol"], "CCountry::SetCountryFlag")
		self.assertEqual(
			pinned["pseudocode_sha256"],
			hashlib.sha256(
				b"void SetCountryFlag(flags) { flags.insert(name); }"
			).hexdigest(),
		)


class PinCatalog(unittest.TestCase):
	def test_every_primitive_pinned_flips_the_catalog(self) -> None:
		with tempfile.TemporaryDirectory() as temporary:
			path = catalog_file(
				Path(temporary),
				{
					"set_country_flag": {
						"kind": "effect",
						"facts": {"storage": "country flag set"},
						"handler": None,
					}
				},
			)
			result = pin_catalog(
				analysis(),
				SemanticsQuery(path),
				game_version="1.37.5",
				binary_sha256="abc",
			)
		self.assertEqual(result.missing, ())
		self.assertEqual(result.catalog["status"], "pinned")
		self.assertEqual(result.catalog["game_version"], "1.37.5")
		self.assertEqual(result.catalog["binary_sha256"], "abc")
		pinned = result.catalog["primitives"]["set_country_flag"]["handler"]
		self.assertEqual(pinned["address"], 0x2000)
		self.assertEqual(pinned["end"], 0x2100)

	def test_missing_handlers_keep_the_catalog_unpinned(self) -> None:
		with tempfile.TemporaryDirectory() as temporary:
			path = catalog_file(
				Path(temporary),
				{
					"set_country_flag": {
						"kind": "effect",
						"facts": {},
						"handler": None,
					},
					"absent": {"kind": "trigger", "facts": {}, "handler": None},
				},
			)
			result = pin_catalog(
				analysis(),
				SemanticsQuery(path),
				game_version="1.37.5",
				binary_sha256="abc",
			)
		self.assertEqual(result.missing, ("absent",))
		self.assertEqual(result.catalog["status"], "unpinned")
		self.assertIsNone(result.catalog["primitives"]["absent"]["handler"])
		self.assertNotIn("game_version", result.catalog)
		# The pinned primitive keeps its pin even when another is missing.
		self.assertEqual(
			result.catalog["primitives"]["set_country_flag"]["handler"]["address"],
			0x2000,
		)

	def test_the_foch_test_catalog_loads(self) -> None:
		path = (
			Path(__file__).resolve().parents[3]
			/ "packages"
			/ "foch-test"
			/ "src"
			/ "interpret"
			/ "semantics.json"
		)
		if not path.exists():  # runs from a packaged tree without the repo
			self.skipTest("repository layout not available")
		catalog = load_catalog(path)
		self.assertEqual(catalog["format"], 1)
		self.assertTrue(catalog["primitives"])
		for name, primitive in catalog["primitives"].items():
			self.assertIn(primitive["kind"], {"effect", "trigger"}, name)


if __name__ == "__main__":
	unittest.main()
