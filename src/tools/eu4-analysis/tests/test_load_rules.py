from __future__ import annotations

import json
import unittest
from dataclasses import asdict, replace

from test_analysis import FakeAnalysis, function, instruction, invoke, literal
from test_catalog import fixture, symbol

from eu4_analysis.catalog import CatalogDiscovery, discover_catalog
from eu4_analysis.load_rules import (
	DatabaseRule,
	FileSelection,
	LoadRules,
	RuleBuild,
	directory_roots,
	discover_load_rules,
	parse_rules,
	rules_to_json,
	with_prior_build,
)
from eu4_analysis.models import FunctionEvidence


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
	return discover_load_rules(analysis, catalog, platform="macos")


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
			{owner: rule.selections for owner, rule in rules.databases.items()},
			{
				"CThingDatabase": (
					FileSelection("common/first", "*.txt"),
					FileSelection("common/second", "*.txt"),
				)
			},
		)
		artifact = rules_to_json(rules)
		self.assertEqual(set(artifact), {"game_version", "builds", "databases"})
		self.assertEqual(
			set(artifact["databases"]["CThingDatabase"][0]), {"directory", "files"}
		)

	def test_object_identity_and_per_file_parser_are_not_prerequisites(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		del analysis.functions[
			0x300
		]  # No AppendFromFile or identity chain is available.
		self.assertEqual(len(discover(analysis).databases["CThingDatabase"].selections), 2)

	def test_unknown_filter_is_not_published_as_a_default_txt_rule(self) -> None:
		analysis: FakeAnalysis = analysis_fixture()
		analysis.functions[0x200] = function(
			symbol("CThingDatabase", "InitFromDirectory"), 0x200
		)
		self.assertEqual({owner: rule.selections for owner, rule in discover(analysis).databases.items()}, {})

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
		self.assertEqual(len(discover(analysis).databases["CThingDatabase"].selections), 2)

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
			discover(analysis).databases["CThingDatabase"].selections,
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
			discover_load_rules(analysis, catalog, platform="macos")

	def test_unresolved_enum_layout_is_not_guessed(self) -> None:
		constructor, getter = directories()
		with self.assertRaisesRegex(ValueError, "one string member"):
			directory_roots(constructor, replace(getter, instructions=()))

	def test_a_build_that_agrees_extends_the_provenance(self) -> None:
		rules: LoadRules = discover(analysis_fixture())
		prior: LoadRules = replace(rules, builds=(RuleBuild("macos", "b" * 64),))
		merged: LoadRules = with_prior_build(
			replace(rules, builds=(RuleBuild("windows", "c" * 64),)), prior
		)
		self.assertEqual(
			merged.builds,
			(RuleBuild("macos", "b" * 64), RuleBuild("windows", "c" * 64)),
		)
		self.assertEqual(parse_rules(json.dumps(rules_to_json(merged))), merged)

	def test_a_build_that_disagrees_is_not_written_over(self) -> None:
		rules: LoadRules = discover(analysis_fixture())
		prior: LoadRules = replace(rules, builds=(RuleBuild("macos", "b" * 64),))
		divergent: LoadRules = replace(
			rules, databases={**rules.databases, "COtherDatabase": DatabaseRule(())}
		)
		with self.assertRaisesRegex(ValueError, "disagree"):
			with_prior_build(divergent, prior)
		with self.assertRaisesRegex(ValueError, "another game version"):
			with_prior_build(replace(rules, game_version="other"), prior)


if __name__ == "__main__":
	unittest.main()


class SameNameRecordTests(unittest.TestCase):
	def test_both_artifact_shapes_parse_and_round_trip(self) -> None:
		bare = json.dumps(
			{
				"game_version": "test",
				"builds": [
					{"platform": "macos", "binary_sha256": "a" * 64},
				],
				"databases": {
					"CIdeaDataBase": [
						{"directory": "common/ideas", "files": "*.txt"},
					],
				},
			}
		)
		detailed = json.dumps(
			{
				"game_version": "test",
				"builds": [
					{"platform": "windows", "binary_sha256": "b" * 64},
				],
				"databases": {
					"CStaticModifierDataBase": {
						"directories": [
							{"directory": "common/static_modifiers", "files": "*.txt"},
							{"directory": "common/event_modifiers", "files": "*.txt"},
						],
						"same_name": {
							"resolution": "later_directory_wins",
							"registration_order": [
								"common/static_modifiers",
								"common/event_modifiers",
							],
							"evidence": "decompiled registration and lookup",
						},
					},
				},
			}
		)
		for text, same_name in (
			(bare, None),
			(detailed, "later_directory_wins"),
		):
			with self.subTest(same_name=same_name):
				rules = parse_rules(text)
				rule = rules.databases[
					"CIdeaDataBase" if same_name is None else "CStaticModifierDataBase"
				]
				self.assertEqual(rule.same_name.resolution if same_name else None, same_name)
				self.assertEqual(
					parse_rules(json.dumps(rules_to_json(rules))), rules
				)

	def test_prior_same_name_record_survives_a_new_extraction(self) -> None:
		prior = parse_rules(
			json.dumps(
				{
					"game_version": "test",
					"builds": [{"platform": "windows", "binary_sha256": "b" * 64}],
					"databases": {
						"CStaticModifierDataBase": {
							"directories": [
								{"directory": "common/static_modifiers", "files": "*.txt"},
								{"directory": "common/event_modifiers", "files": "*.txt"},
							],
							"same_name": {
								"resolution": "later_directory_wins",
								"registration_order": [
									"common/static_modifiers",
									"common/event_modifiers",
								],
								"evidence": "decompiled registration and lookup",
							},
						},
					},
				}
			)
		)
		fresh = replace(
			prior,
			builds=(RuleBuild("macos", "a" * 64),),
			databases={
				"CStaticModifierDataBase": DatabaseRule(prior.databases["CStaticModifierDataBase"].selections)
			},
		)
		merged = with_prior_build(fresh, prior)
		self.assertEqual(merged.databases["CStaticModifierDataBase"].same_name, prior.databases["CStaticModifierDataBase"].same_name)
		self.assertEqual(len(merged.builds), 2)
