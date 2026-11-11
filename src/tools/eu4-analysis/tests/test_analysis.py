from __future__ import annotations

import fnmatch
import struct
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from eu4_analysis.binary import read_binary
from eu4_analysis.families import Discovery, FamilyQuery, discover_family
from eu4_analysis.ghidra import GhidraSession
from eu4_analysis.macho import function_starts, read_macho_image
from eu4_analysis.models import Function, FunctionEvidence, Instruction, Reference
from eu4_analysis.pe import read_pe_image
from eu4_analysis.x86 import Pointer, trace


def instruction(op: str, *operands: str, call: Reference | None = None) -> Instruction:
	return Instruction(0, op, operands, call)


def invoke(name: str, address: int = 0) -> Instruction:
	return instruction("CALL", call=Reference(address, (name,)))


def literal(register: str, value: str) -> Instruction:
	return Instruction(
		0, "LEA", (register, "[0x9000]"), data=(Reference(0x9000, string=value),)
	)


def function(name: str, address: int, *instructions: Instruction) -> FunctionEvidence:
	return FunctionEvidence(
		Function(address, address + 0x100, (name,)),
		tuple(
			replace(item, address=address + index)
			for index, item in enumerate(instructions)
		),
		None,
	)


class FakeAnalysis:
	def __init__(self, functions: tuple[FunctionEvidence, ...]) -> None:
		self.functions: dict[int, FunctionEvidence] = {
			item.function.address: item for item in functions
		}

	def find(self, pattern: str) -> tuple[Function, ...]:
		return tuple(
			item.function
			for item in self.functions.values()
			if any(fnmatch.fnmatchcase(name, pattern) for name in item.function.names)
		)

	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence:
		return self.functions[function.address]


def database(
	*, receiver: bool = True, key_offset: int = 0x138, registered: bool = True
) -> FakeAnalysis:
	paths: list[Instruction] = [
		instruction("LEA", "RDI", "[RBP + -0x10]"),
		instruction("MOV", "EDX", "0x0"),
		invoke("CDirectorySettings_GetOriginalDirectory"),
	]
	for path in ("/first", "/second"):
		paths.extend(
			(
				instruction("LEA", "RDI", "[RBP + -0x30]"),
				instruction("LEA", "RSI", "[RBP + -0x10]"),
				invoke("7CStringC1ERKS_"),
				instruction("LEA", "RDI", "[RBP + -0x30]"),
				literal("RSI", path),
				invoke("7CStringpLEPKc"),
				invoke("CThingDatabase_AccessInstance"),
				instruction("MOV", "RDI", "RAX"),
				instruction("LEA", "RSI", "[RBP + -0x30]"),
				invoke("CThingDatabase_InitFromDirectory", 0x200),
			)
		)
	return FakeAnalysis(
		(
			function("CEU4Application_LoadDatabasesEv", 0x100, *paths),
			function(
				"CThingDatabase_InitFromDirectory",
				0x200,
				instruction("MOV", "qword ptr [RBP + -0x10]", "RDI"),
				literal("RDX", ".txt"),
				invoke("VFSGetEnumeratedFiles"),
				instruction("JZ", "0x280"),
				instruction(
					"MOV", "RDI", "qword ptr [RBP + -0x10]" if receiver else "RAX"
				),
				invoke("CThingDatabase_AppendFromFile", 0x300),
			),
			function(
				"CThingDatabase_AppendFromFile",
				0x300,
				invoke("CThingDatabase_AddThing", 0x400)
				if registered
				else invoke("OtherParser"),
			),
			function(
				"CThingDatabase_AddThing",
				0x400,
				invoke("CHashTable_CThing_3Add", 0x500),
			),
			function(
				"CHashTable_CThing_3Add",
				0x500,
				instruction("MOV", "RDI", "RSI"),
				instruction("ADD", "RDI", "0x138"),
				invoke("CString_GetHashValue"),
			),
			function(
				"CHashTable_CThing_4Find",
				0x600,
				instruction("MOV", "R12", "RSI"),
				instruction("JZ", "0x620"),
				instruction("MOV", "RAX", "qword ptr [RBX + RDX*0x8]"),
				instruction("LEA", "RDI", f"[RAX + {key_offset:#x}]"),
				instruction("MOV", "RSI", "R12"),
				invoke("7CStringeq"),
			),
			function(
				"CThingDatabase_GetByKey",
				0x700,
				invoke("CHashTable_CThing_4Find", 0x600),
			),
			# A plausible display name must never be promoted to the identity key.
			function(
				"CThing_GetName", 0x800, instruction("LEA", "RAX", "[RDI + 0x10]")
			),
		)
	)


def discover(analysis: FakeAnalysis) -> Discovery:
	return discover_family(
		analysis, "a" * 64, FamilyQuery("CThingDatabase", "CThing"), game_version="test"
	)


class DiscoveryTests(unittest.TestCase):
	def test_shared_registry_and_hash_key_are_distinct_from_display_name(self) -> None:
		result: Discovery = discover(database())
		self.assertIsNotNone(result.rules.resource_scope)
		self.assertIsNotNone(result.rules.definition_identity)
		assert result.rules.resource_scope is not None
		assert result.rules.definition_identity is not None
		self.assertEqual(
			[path.suffix for path in result.rules.resource_scope.paths],
			["/first", "/second"],
		)
		self.assertEqual(result.rules.resource_scope.file_filter, ".txt")
		self.assertEqual(result.rules.definition_identity.key_member_offset, 0x138)
		self.assertIsNone(result.rules.definition_identity.source_key)
		self.assertEqual(result.rules.status, "candidate")
		self.assertEqual(result.rules.game_version, "test")
		self.assertNotIn(0x800, [item.function.address for item in result.functions])

	def test_shared_parser_without_shared_receiver_does_not_establish_scope(
		self,
	) -> None:
		self.assertIsNone(discover(database(receiver=False)).rules.resource_scope)

	def test_mismatched_hash_and_equality_members_do_not_establish_identity(
		self,
	) -> None:
		self.assertIsNone(discover(database(key_offset=0x10)).rules.definition_identity)

	def test_missing_registration_does_not_establish_identity(self) -> None:
		self.assertIsNone(
			discover(database(registered=False)).rules.definition_identity
		)

	def test_ambiguous_distinct_functions_remain_unknown(self) -> None:
		analysis: FakeAnalysis = database()
		analysis.functions[0x900] = function(
			"CThingDatabase_Other_InitFromDirectory", 0x900
		)
		self.assertIsNone(discover(analysis).rules.resource_scope)

	def test_symbol_aliases_at_one_address_are_not_ambiguous(self) -> None:
		analysis: FakeAnalysis = database()
		item: FunctionEvidence = analysis.functions[0x200]
		analysis.functions[0x200] = replace(
			item,
			function=replace(
				item.function,
				names=item.function.names + ("CThingDatabase_InitFromDirectory_alias",),
			),
		)
		self.assertIsNotNone(discover(analysis).rules.resource_scope)

	def test_missing_symbols_return_unknown_without_decompilation(self) -> None:
		result: Discovery = discover(FakeAnalysis(()))
		self.assertEqual(result.rules.status, "unknown")
		self.assertEqual(result.functions, ())

	def test_multiple_enumeration_filters_are_not_silently_collapsed(self) -> None:
		analysis: FakeAnalysis = database()
		loader: FunctionEvidence = analysis.functions[0x200]
		analysis.functions[0x200] = replace(
			loader,
			instructions=loader.instructions
			+ (
				literal("RDX", ".other"),
				invoke("VFSGetEnumeratedFiles"),
			),
		)
		self.assertIsNone(discover(analysis).rules.resource_scope)


class TracingTests(unittest.TestCase):
	def test_partial_store_invalidates_saved_pointer(self) -> None:
		item: FunctionEvidence = function(
			"example",
			0x100,
			instruction("MOV", "qword ptr [RBP + -0x10]", "RDI"),
			instruction("MOV", "byte ptr [RBP + -0x10]", "0x0"),
			instruction("JZ", "0x150"),
			instruction("MOV", "RDI", "qword ptr [RBP + -0x10]"),
			invoke("consumer"),
		)
		self.assertNotEqual(
			trace(item, "Registry").calls[-1].arguments[0], Pointer("arg0")
		)

	def test_reassigned_stack_input_does_not_survive_branch(self) -> None:
		item: FunctionEvidence = function(
			"example",
			0x100,
			instruction("MOV", "qword ptr [RBP + -0x10]", "RDI"),
			instruction("MOV", "qword ptr [RBP + -0x10]", "RSI"),
			instruction("JZ", "0x150"),
			instruction("MOV", "RDI", "qword ptr [RBP + -0x10]"),
			invoke("consumer"),
		)
		argument = trace(item, "Registry").calls[-1].arguments[0]
		self.assertNotIn(argument, (Pointer("arg0"), Pointer("arg1")))

	def test_mutated_directory_origin_cannot_reappear_after_branch(self) -> None:
		item: FunctionEvidence = function(
			"example",
			0x100,
			instruction("LEA", "RDI", "[RBP + -0x10]"),
			instruction("MOV", "EDX", "0x0"),
			invoke("CDirectorySettings_GetOriginalDirectory"),
			instruction("LEA", "RDI", "[RBP + -0x10]"),
			invoke("unknown_mutator"),
			instruction("JZ", "0x150"),
			instruction("LEA", "RSI", "[RBP + -0x10]"),
			invoke("consumer"),
		)
		self.assertEqual(
			trace(item, "Registry").calls[-1].arguments[1], Pointer("frame", -0x10)
		)

	def test_dereference_does_not_inherit_container_member_offset(self) -> None:
		item: FunctionEvidence = function(
			"example",
			0x100,
			instruction("MOV", "RDI", "qword ptr [RDI + 0x18]"),
			instruction("ADD", "RDI", "0x138"),
			invoke("consumer"),
		)
		argument = trace(item, "Registry").calls[-1].arguments[0]
		assert isinstance(argument, Pointer)
		self.assertEqual(argument.offset, 0x138)


def macho() -> bytes:
	segment: bytes = struct.pack(
		"<II16sQQQQIIII", 0x19, 152, b"__TEXT", 0x1000, 0x400, 0, 0x400, 7, 5, 1, 0
	)
	section: bytes = struct.pack(
		"<16s16sQQIIIIIIII", b"__text", b"__TEXT", 0x1100, 0x300, 0, 0, 0, 0, 0, 0, 0, 0
	)
	starts: bytes = b"\x80\x02\x20\x00"
	command: bytes = struct.pack("<IIII", 0x26, 16, 200, len(starts))
	header: bytes = struct.pack("<8I", 0xFEEDFACF, 0x01000007, 3, 2, 2, 168, 0, 0)
	return header + segment + section + command + starts


class MachOTests(unittest.TestCase):
	def test_workspace_cannot_write_inside_game_installation(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			binary: Path = root / "game/eu4.app/Contents/MacOS/eu4"
			binary.parent.mkdir(parents=True)
			binary.write_bytes(macho())
			with self.assertRaisesRegex(ValueError, "outside the game installation"):
				GhidraSession(binary, root / "ghidra", root / "game/analysis")
			self.assertFalse((root / "game/analysis").exists())

	def test_manual_function_range_cannot_override_binary_metadata(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			binary: Path = root / "game/eu4"
			binary.parent.mkdir()
			binary.write_bytes(macho())
			analysis: GhidraSession = GhidraSession(
				binary, root / "ghidra", root / "analysis"
			)
			with self.assertRaisesRegex(ValueError, "differs from binary metadata"):
				analysis.inspect(Function(0x1100, 0x1200, ("invalid",)))

	def test_exact_metadata_boundaries(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			path: Path = Path(directory) / "eu4"
			path.write_bytes(macho())
			image = read_macho_image(path)
		self.assertEqual(image.function_starts, (0x1100, 0x1120))
		self.assertEqual(image.function_end(0x1100), 0x1120)
		self.assertEqual(image.function_end(0x1120), 0x1400)
		with self.assertRaisesRegex(ValueError, "not a recorded function start"):
			image.function_end(0x1101)

	def test_rejects_malformed_binary_and_unsupported_cpu(self) -> None:
		valid: bytes = macho()
		for data in (
			b"short",
			valid[:100],
			valid[:4] + b"\0" * 4 + valid[8:],
			valid[:36] + b"\0" * 4 + valid[40:],
		):
			with (
				self.subTest(length=len(data)),
				tempfile.TemporaryDirectory() as directory,
			):
				path: Path = Path(directory) / "bad"
				path.write_bytes(data)
				with self.assertRaises(ValueError):
					read_macho_image(path)

	def test_invalid_function_table(self) -> None:
		for data in (b"\x80", b"\x80" * 10, b"\x01", b"\xff\x7f\x00"):
			with self.subTest(data=data), self.assertRaises(ValueError):
				function_starts(data, 0x1000, 0x1100)


def pe(
	entries: tuple[tuple[int, int], ...] = ((0x1100, 0x1120), (0x1120, 0x1180)),
	machine: int = 0x8664,
) -> bytes:
	"""One minimal x64 PE32+ image: headers, `.text` at 0x200, `.pdata` at 0x600."""
	pdata: bytes = b"".join(
		struct.pack("<III", begin, end, 0) for begin, end in entries
	)
	text: bytes = b"\x90" * 0x400
	directories: bytes = (
		b"\0" * 8 * 3 + struct.pack("<II", 0x1800, len(pdata)) + b"\0" * 8 * 12
	)
	optional: bytes = (
		struct.pack("<HBBIIIIIQ", 0x20B, 2, 0, 0, 0, 0, 0, 0x1000, 0x140000000)
		+ b"\0" * 76
		+ struct.pack("<I", 16)
		+ directories
	)
	assert len(optional) == 240, len(optional)
	sections: bytes = struct.pack(
		"<8sIIIIIIHHI",
		b".text\0\0\0",
		0x400,
		0x1000,
		0x400,
		0x200,
		0,
		0,
		0,
		0,
		0x60000020,
	) + struct.pack(
		"<8sIIIIIIHHI",
		b".pdata",
		len(pdata),
		0x1800,
		len(pdata),
		0x600,
		0,
		0,
		0,
		0,
		0x40000040,
	)
	coff: bytes = struct.pack("<HHIIIHH", machine, 2, 0, 0, 0, len(optional), 0)
	image: bytes = b"MZ" + b"\0" * 0x3A + struct.pack("<I", 0x40)
	image += b"PE\0\0" + coff + optional + sections
	assert len(image) <= 0x200, len(image)
	return image.ljust(0x200, b"\0") + text + pdata


class PETests(unittest.TestCase):
	def test_exact_metadata_boundaries(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			path: Path = Path(directory) / "eu4.exe"
			path.write_bytes(pe())
			image = read_pe_image(path)
		self.assertEqual(image.function_starts, (0x140001100, 0x140001120))
		self.assertEqual(image.function_ends, (0x140001120, 0x140001180))
		self.assertEqual(image.function_end(0x140001100), 0x140001120)
		self.assertEqual(image.function_end(0x140001120), 0x140001180)
		with self.assertRaisesRegex(ValueError, "not a recorded function start"):
			image.function_end(0x140001101)

	def test_read_binary_dispatches_on_the_dos_header(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			pe_path: Path = Path(directory) / "eu4.exe"
			pe_path.write_bytes(pe())
			self.assertEqual(
				read_binary(pe_path).function_ends, (0x140001120, 0x140001180)
			)
			macho_path: Path = Path(directory) / "eu4"
			macho_path.write_bytes(macho())
			self.assertIsNone(read_binary(macho_path).function_ends)

	def test_rejects_malformed_image_and_unsupported_cpu(self) -> None:
		valid: bytes = pe()
		bad_machine: bytes = pe(machine=0x14C)
		for data in (
			b"short",
			b"MZ" + b"\0" * 0x3E,
			valid[:0x100],
			bad_machine,
		):
			with (
				self.subTest(length=len(data)),
				tempfile.TemporaryDirectory() as directory,
			):
				path: Path = Path(directory) / "bad.exe"
				path.write_bytes(data)
				with self.assertRaises(ValueError):
					read_pe_image(path)

	def test_rejects_unsorted_or_empty_function_entries(self) -> None:
		for entries in ((), ((0x1120, 0x1180), (0x1100, 0x1120)), ((0x1100, 0x1100),)):
			with (
				self.subTest(entries=entries),
				tempfile.TemporaryDirectory() as directory,
			):
				path: Path = Path(directory) / "bad.exe"
				path.write_bytes(pe(entries=entries))
				with self.assertRaises(ValueError):
					read_pe_image(path)


if __name__ == "__main__":
	unittest.main()
