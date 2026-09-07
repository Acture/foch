from __future__ import annotations

import unittest
from dataclasses import asdict, replace

from eu4_analysis.catalog import CatalogDiscovery, discover_catalog
from eu4_analysis.load_rules import (
	FileSelection,
	LoadRules,
	directory_roots,
	discover_load_rules,
)
from eu4_analysis.models import FunctionEvidence
from test_analysis import FakeAnalysis, function, instruction, invoke, literal
from test_catalog import fixture, symbol


def directories() -> tuple[FunctionEvidence, FunctionEvidence]:
	constructor: FunctionEvidence = function(
		"__ZN18CDirectorySettingsC2Ev",
		0xB00,
		instruction("MOV", "RBX", "RDI"),
		instruction("LEA", "RDI", "[RBX + 0x80]"),
		literal("RSI", "common"),
		invoke("basic_string_6assignEPKc"),
		instruction("LEA", "RDI", "[RBX + 0xe0]"),
		literal("RSI", "interface"),
		invoke("basic_string_6assignEPKc"),
	)
	getter: FunctionEvidence = function(
		symbol("CDirectorySettings", "GetOriginalDirectory"),
		0xC00,
		instruction("MOV", "EAX", "EDX"),
		instruction("LEA", "RAX", "[RAX + RAX*0x2]"),
		instruction("LEA", "RSI", "[RSI + RAX*0x8 + 0x80]"),
		invoke("7CStringC2ERKS_"),
	)
	return constructor, getter


def analysis_fixture() -> FakeAnalysis:
	analysis: FakeAnalysis = fixture()
	for item in directories():
		analysis.functions[item.function.address] = item
	return analysis


def discover(analysis: FakeAnalysis) -> LoadRules:
	catalog: CatalogDiscovery = discover_catalog(
		analysis, "a" * 64, game_version="test"
	)
	return discover_load_rules(analysis, catalog)


class LoadRuleTests(unittest.TestCase):
	def test_directory_enum_is_recovered_from_getter_and_assignments(self) -> None:
		self.assertEqual(
			directory_roots(*directories()),
			{
				"CDirectorySettings::GetOriginalDirectory(0)": "common",
				"CDirectorySettings::GetOriginalDirectory(4)": "interface",
			},
		)

	def test_output_contains_only_database_directory_and_file_relations(self) -> None:
		rules: LoadRules = discover(analysis_fixture())
		self.assertEqual(
			rules.databases,
			{
				"CThingDatabase": (
					FileSelection("common/first", "*.txt"),
					FileSelection("common/second", "*.txt"),
				)
			},
		)
		self.assertEqual(
			set(asdict(rules)), {"game_version", "binary_sha256", "databases"}
		)
		self.assertEqual(
			set(asdict(rules)["databases"]["CThingDatabase"][0]), {"directory", "files"}
		)

	def test_object_identity_and_per_file_parser_are_not_prerequisites(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		del analysis.functions[
			0x300
		]  # No AppendFromFile or identity chain is available.
		self.assertEqual(len(discover(analysis).databases["CThingDatabase"]), 2)

	def test_unknown_filter_is_not_published_as_a_default_txt_rule(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		analysis.functions[0x200] = function(
			symbol("CThingDatabase", "InitFromDirectory"), 0x200
		)
		self.assertEqual(discover(analysis).databases, {})

	def test_direct_vfs_enumeration_has_the_same_filter_contract(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		loader: FunctionEvidence = analysis.functions[0x200]
		analysis.functions[0x200] = replace(
			loader,
			instructions=tuple(
				replace(item, call=replace(item.call, names=("VFSEnumerateFiles",)))
				if item.call is not None
				and item.call.names == ("VFSGetEnumeratedFiles",)
				else item
				for item in loader.instructions
			),
		)
		self.assertEqual(len(discover(analysis).databases["CThingDatabase"]), 2)

	def test_rules_are_matched_to_each_loader_call_not_only_to_database_name(
		self,
	) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		method: str = symbol("CThingDatabase", "ReadExtraFromDirectory")
		analysis.functions[0xD00] = function(
			method,
			0xD00,
			literal("RDX", ".csv"),
			invoke("VFSGetEnumeratedFiles"),
		)
		bootstrap: FunctionEvidence = analysis.functions[0x100]
		analysis.functions[0x100] = function(
			bootstrap.function.names[0],
			0x100,
			*bootstrap.instructions,
			instruction("LEA", "RDI", "[RBP + -0x50]"),
			instruction("MOV", "EDX", "0x0"),
			invoke("CDirectorySettings_GetOriginalDirectory"),
			instruction("LEA", "RDI", "[RBP + -0x50]"),
			literal("RSI", "/third"),
			invoke("7CStringpLEPKc"),
			invoke(symbol("CThingDatabase", "AccessInstance")),
			instruction("MOV", "RDI", "RAX"),
			instruction("LEA", "RSI", "[RBP + -0x50]"),
			invoke(method, 0xD00),
		)
		self.assertEqual(
			discover(analysis).databases["CThingDatabase"],
			(
				FileSelection("common/first", "*.txt"),
				FileSelection("common/second", "*.txt"),
				FileSelection("common/third", "*.csv"),
			),
		)

	def test_partial_inventory_cannot_replace_repository_rules(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		catalog: CatalogDiscovery = discover_catalog(
			analysis, "a" * 64, game_version="test", limit=1
		)
		with self.assertRaisesRegex(ValueError, "partial scan"):
			discover_load_rules(analysis, catalog)

	def test_unresolved_enum_layout_is_not_guessed(self) -> None:
		constructor, getter = directories()
		with self.assertRaisesRegex(ValueError, "one string member"):
			directory_roots(constructor, replace(getter, instructions=()))


if __name__ == "__main__":
	unittest.main()
