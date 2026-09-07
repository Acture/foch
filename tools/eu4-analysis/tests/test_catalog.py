from __future__ import annotations

import unittest
from dataclasses import replace

from eu4_analysis.__main__ import CatalogQuery, parse_args
from eu4_analysis.catalog import CatalogDiscovery, CatalogReport, discover_catalog
from eu4_analysis.models import Function, FunctionEvidence
from eu4_analysis.symbols import MemberName, member_name, registration_record
from test_analysis import FakeAnalysis, database, function


def symbol(owner: str, method: str, parameters: str = "v") -> str:
	return (
		"__ZN"
		+ "".join(f"{len(part)}{part}" for part in owner.split("::"))
		+ f"{len(method)}{method}E{parameters}"
	)


def fixture() -> FakeAnalysis:
	analysis: FakeAnalysis = database()

	def rename(name: str) -> str:
		if name.startswith("CThingDatabase_"):
			method: str = name.removeprefix("CThingDatabase_")
			return symbol(
				"CThingDatabase", method, "P6CThing" if method == "AddThing" else "v"
			)
		return name

	for address, item in tuple(analysis.functions.items()):
		analysis.functions[address] = replace(
			item,
			function=replace(
				item.function, names=tuple(rename(name) for name in item.function.names)
			),
			instructions=tuple(
				replace(
					instruction,
					call=replace(
						instruction.call,
						names=tuple(rename(name) for name in instruction.call.names),
					),
				)
				if instruction.call
				else instruction
				for instruction in item.instructions
			),
		)
	# Neither appears in the bootstrap; both must remain in the inventory.
	analysis.functions[0x900] = function(
		symbol("COtherDatabase", "InitFromDirectory"), 0x900
	)
	analysis.functions[0xA00] = function(
		symbol("CUnsupportedDatabase", "InitFromFile"), 0xA00
	)
	return analysis


class BrokenAnalysis(FakeAnalysis):
	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence:
		if function.address == 0x900:
			raise RuntimeError("bounded disassembly failed")
		return super().inspect(function, pseudocode=pseudocode)


class CatalogTests(unittest.TestCase):
	def test_default_command_discovers_all_without_registry_or_record(self) -> None:
		options = parse_args(
			[
				"discover",
				"--binary",
				"/tmp/eu4",
				"--ghidra",
				"/tmp/ghidra",
				"--game-version",
				"test",
			]
		)
		self.assertIsInstance(options.query, CatalogQuery)
		assert isinstance(options.query, CatalogQuery)
		self.assertIsNone(options.query.limit)

	def test_all_candidates_are_processed_and_unknowns_remain_visible(self) -> None:
		result: CatalogDiscovery = discover_catalog(
			fixture(), "a" * 64, game_version="test"
		)
		self.assertTrue(result.report.scan_complete)
		self.assertEqual(len(result.report.entries), 3)
		states: dict[str, str] = {
			item.registry: item.status for item in result.report.entries
		}
		self.assertEqual(
			states,
			{
				"CThingDatabase": "candidate",
				"COtherDatabase": "unknown",
				"CUnsupportedDatabase": "unknown",
			},
		)
		addresses: list[int] = [item.function.address for item in result.functions]
		self.assertEqual(addresses.count(0x100), 1)

	def test_limit_preserves_the_full_inventory_and_pending_entries(self) -> None:
		snapshots: list[CatalogReport] = []
		result: CatalogDiscovery = discover_catalog(
			fixture(), "a" * 64, game_version="test", limit=1, progress=snapshots.append
		)
		self.assertFalse(result.report.scan_complete)
		self.assertEqual(len(result.report.entries), 3)
		self.assertEqual(
			sum(item.status == "pending" for item in result.report.entries), 2
		)
		self.assertTrue(all(item.status == "pending" for item in snapshots[0].entries))
		self.assertEqual(snapshots[-1], result.report)

	def test_failed_family_does_not_drop_itself_or_other_families(self) -> None:
		analysis: BrokenAnalysis = BrokenAnalysis(tuple(fixture().functions.values()))
		result: CatalogDiscovery = discover_catalog(
			analysis, "a" * 64, game_version="test"
		)
		self.assertTrue(result.report.scan_complete)
		self.assertEqual(len(result.report.entries), 3)
		failed = next(
			item for item in result.report.entries if item.registry == "COtherDatabase"
		)
		self.assertEqual(failed.status, "error")
		self.assertEqual(failed.error, "bounded disassembly failed")
		self.assertEqual(
			next(
				item.status
				for item in result.report.entries
				if item.registry == "CThingDatabase"
			),
			"candidate",
		)

	def test_linker_aliases_do_not_duplicate_registries(self) -> None:
		analysis: FakeAnalysis = fixture()
		item: FunctionEvidence = analysis.functions[0x900]
		analysis.functions[0x900] = replace(
			item, function=replace(item.function, names=item.function.names * 2)
		)
		self.assertEqual(
			len(
				discover_catalog(analysis, "a" * 64, game_version="test").report.entries
			),
			3,
		)


class SymbolTests(unittest.TestCase):
	def test_length_encoded_names_preserve_namespaces_and_argument_types(self) -> None:
		member: MemberName | None = member_name(
			symbol("NExample::CRegistry", "AddThing", "P6CThing")
		)
		self.assertEqual(
			member, MemberName("NExample::CRegistry", "AddThing", "P6CThing")
		)
		assert member is not None
		self.assertEqual(registration_record(member), "CThing")

	def test_template_and_malformed_owners_are_not_guessed(self) -> None:
		for name in (
			"__ZN999CRegistry",
			"__ZN3BoxI6CThingE3AddEv",
			"guard_for_CDatabase_InitFromFile",
		):
			with self.subTest(name=name):
				self.assertIsNone(member_name(name))

	def test_record_names_are_not_guessed_from_add_method_spelling(self) -> None:
		self.assertIsNone(registration_record(MemberName("CRegistry", "AddThing", "i")))
		self.assertIsNone(
			registration_record(MemberName("CRegistry", "AddThing", "P99CThing"))
		)


if __name__ == "__main__":
	unittest.main()
