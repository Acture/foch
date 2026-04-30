// Cross-file global key namespace detection for the merge system.
//
// EU4 loads ALL files within a content family directory and builds a global
// namespace — two mods defining the same key in DIFFERENT files is still a
// conflict. This module detects such cross-file key conflicts.
#![allow(dead_code)]

use foch_language::analyzer::content_family::{GameProfile, MergeKeySource};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, is_decision_container_key};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::workspace::ResolvedFileContributor;

use super::normalize::normalize_defines_file;

/// A key contributor: which mod defined this key, in which file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyContributor {
	pub mod_id: String,
	pub file_path: String,
	pub precedence: usize,
	pub is_base_game: bool,
}

/// Result of scanning a family's global namespace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FamilyKeyConflict {
	pub key: String,
	pub family_id: String,
	pub contributors: Vec<KeyContributor>,
}

/// The global key index for a content family.
#[derive(Clone, Debug, Default)]
pub struct FamilyKeyIndex {
	pub family_id: String,
	/// key_name → list of contributors (mod_id, file_path, precedence).
	pub entries: HashMap<String, Vec<KeyContributor>>,
}

/// Extract merge keys from a parsed script file according to the given
/// `MergeKeySource`. Returns a list of `(merge_key, statement_key)` pairs.
fn extract_keys(parsed: &ParsedScriptFile, merge_key_source: MergeKeySource) -> Vec<String> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => extract_assignment_keys(parsed),
		MergeKeySource::FieldValue(field) => extract_field_value_keys(parsed, field),
		MergeKeySource::ContainerChildKey => extract_container_child_keys(parsed),
		MergeKeySource::ContainerChildFieldValue {
			container,
			child_key_field,
			child_types,
		} => extract_container_child_field_value_keys(
			parsed,
			container,
			child_key_field,
			child_types,
		),
		MergeKeySource::LeafPath => extract_defines_keys(parsed),
	}
}

fn extract_assignment_keys(parsed: &ParsedScriptFile) -> Vec<String> {
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => Some(key.clone()),
			_ => None,
		})
		.collect()
}

fn extract_field_value_keys(parsed: &ParsedScriptFile, field: &str) -> Vec<String> {
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = stmt
			else {
				return None;
			};
			scalar_assignment_value(items, field)
		})
		.collect()
}

fn extract_container_child_keys(parsed: &ParsedScriptFile) -> Vec<String> {
	let mut keys = Vec::new();
	for stmt in &parsed.ast.statements {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = stmt
		else {
			continue;
		};
		if !is_decision_container_key(key) {
			continue;
		}
		for item in items {
			if let AstStatement::Assignment {
				key: child_key,
				value: AstValue::Block { .. },
				..
			} = item
			{
				keys.push(child_key.clone());
			}
		}
	}
	keys
}

fn extract_container_child_field_value_keys(
	parsed: &ParsedScriptFile,
	container: &str,
	child_key_field: &str,
	child_types: &[&str],
) -> Vec<String> {
	let mut keys = Vec::new();
	for stmt in &parsed.ast.statements {
		let AstStatement::Assignment { key, value, .. } = stmt else {
			continue;
		};
		if key != container {
			keys.push(key.clone());
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for child in items {
			if let Some(key) = container_child_field_value_key(child, child_key_field, child_types)
			{
				keys.push(key);
			}
		}
	}
	keys
}

fn container_child_field_value_key(
	stmt: &AstStatement,
	child_key_field: &str,
	child_types: &[&str],
) -> Option<String> {
	let AstStatement::Assignment { key, value, .. } = stmt else {
		return None;
	};
	if (child_types.is_empty() || child_types.contains(&key.as_str()))
		&& let AstValue::Block { items, .. } = value
		&& let Some(field_value) = scalar_assignment_value(items, child_key_field)
	{
		return Some(format!("{key}:{field_value}"));
	}
	Some(key.clone())
}

fn extract_defines_keys(parsed: &ParsedScriptFile) -> Vec<String> {
	normalize_defines_file(parsed)
		.map(|fragments| fragments.into_iter().map(|f| f.merge_key).collect())
		.unwrap_or_default()
}

fn scalar_assignment_value(items: &[AstStatement], expected_key: &str) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != expected_key {
			continue;
		}
		if let AstValue::Scalar { value, .. } = value {
			return Some(value.as_text());
		}
	}
	None
}

/// Build a [`FamilyKeyIndex`] by scanning all files that belong to a content
/// family across every mod in the workspace.
///
/// `contributors_by_path` is the subset of the workspace file inventory that
/// belongs to this family (keyed by relative path).
pub(crate) fn build_family_key_index(
	family_id: &str,
	merge_key_source: MergeKeySource,
	contributors_by_path: &BTreeMap<String, Vec<ResolvedFileContributor>>,
	profile: &dyn GameProfile,
) -> FamilyKeyIndex {
	let mut index = FamilyKeyIndex {
		family_id: family_id.to_string(),
		..Default::default()
	};

	for (rel_path, contributors) in contributors_by_path {
		for contributor in contributors {
			let parsed = foch_language::analyzer::semantic_index::parse_script_file_with_profile(
				&contributor.mod_id,
				&contributor.root_path,
				&contributor.absolute_path,
				profile,
			);
			let Some(parsed) = parsed else {
				continue;
			};

			let keys = extract_keys(&parsed, merge_key_source);
			for key in keys {
				index.entries.entry(key).or_default().push(KeyContributor {
					mod_id: contributor.mod_id.clone(),
					file_path: rel_path.clone(),
					precedence: contributor.precedence,
					is_base_game: contributor.is_base_game,
				});
			}
		}
	}

	index
}

/// Find all keys defined by multiple non-base-game mods (cross-file or
/// same-file). A key is considered conflicting when two or more *distinct*
/// non-base-game mods contribute it.
pub fn detect_key_conflicts(index: &FamilyKeyIndex) -> Vec<FamilyKeyConflict> {
	let mut conflicts = Vec::new();

	for (key, contributors) in &index.entries {
		let distinct_mod_count = contributors
			.iter()
			.filter(|c| !c.is_base_game)
			.map(|c| &c.mod_id)
			.collect::<std::collections::HashSet<_>>()
			.len();

		if distinct_mod_count >= 2 {
			conflicts.push(FamilyKeyConflict {
				key: key.clone(),
				family_id: index.family_id.clone(),
				contributors: contributors.clone(),
			});
		}
	}

	conflicts.sort_by(|a, b| a.key.cmp(&b.key));
	conflicts
}

/// Group file-inventory entries by content family for namespace analysis.
///
/// Returns `family_id → { relative_path → contributors }`.
pub(crate) fn group_by_family(
	file_inventory: &BTreeMap<String, Vec<ResolvedFileContributor>>,
	profile: &dyn GameProfile,
) -> HashMap<String, BTreeMap<String, Vec<ResolvedFileContributor>>> {
	let mut grouped: HashMap<String, BTreeMap<String, Vec<ResolvedFileContributor>>> =
		HashMap::new();

	for (rel_path, contributors) in file_inventory {
		let path = Path::new(rel_path);
		if let Some(descriptor) = profile.classify_content_family(path) {
			grouped
				.entry(descriptor.id.to_string())
				.or_default()
				.insert(rel_path.clone(), contributors.clone());
		}
	}

	grouped
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	fn make_contributor(mod_id: &str, precedence: usize, is_base_game: bool) -> KeyContributor {
		KeyContributor {
			mod_id: mod_id.to_string(),
			file_path: format!("common/test/{mod_id}.txt"),
			precedence,
			is_base_game,
		}
	}

	fn make_index(family_id: &str, entries: Vec<(&str, Vec<KeyContributor>)>) -> FamilyKeyIndex {
		FamilyKeyIndex {
			family_id: family_id.to_string(),
			entries: entries
				.into_iter()
				.map(|(k, v)| (k.to_string(), v))
				.collect(),
		}
	}

	#[test]
	fn single_mod_no_conflicts() {
		let index = make_index(
			"scripted_triggers",
			vec![
				("trigger_a", vec![make_contributor("mod_a", 1, false)]),
				("trigger_b", vec![make_contributor("mod_a", 1, false)]),
			],
		);

		let conflicts = detect_key_conflicts(&index);
		assert!(
			conflicts.is_empty(),
			"single mod should produce no conflicts"
		);
	}

	#[test]
	fn two_mods_same_key_same_file_detected() {
		let index = make_index(
			"scripted_triggers",
			vec![(
				"my_trigger",
				vec![
					KeyContributor {
						mod_id: "mod_a".to_string(),
						file_path: "common/scripted_triggers/shared.txt".to_string(),
						precedence: 1,
						is_base_game: false,
					},
					KeyContributor {
						mod_id: "mod_b".to_string(),
						file_path: "common/scripted_triggers/shared.txt".to_string(),
						precedence: 2,
						is_base_game: false,
					},
				],
			)],
		);

		let conflicts = detect_key_conflicts(&index);
		assert_eq!(conflicts.len(), 1);
		assert_eq!(conflicts[0].key, "my_trigger");
		assert_eq!(conflicts[0].contributors.len(), 2);
	}

	#[test]
	fn two_mods_same_key_different_files_detected() {
		let index = make_index(
			"scripted_triggers",
			vec![(
				"my_trigger",
				vec![
					KeyContributor {
						mod_id: "mod_a".to_string(),
						file_path: "common/scripted_triggers/a.txt".to_string(),
						precedence: 1,
						is_base_game: false,
					},
					KeyContributor {
						mod_id: "mod_b".to_string(),
						file_path: "common/scripted_triggers/b.txt".to_string(),
						precedence: 2,
						is_base_game: false,
					},
				],
			)],
		);

		let conflicts = detect_key_conflicts(&index);
		assert_eq!(conflicts.len(), 1);
		assert_eq!(conflicts[0].key, "my_trigger");
		// Verify contributors come from different files
		let paths: Vec<&str> = conflicts[0]
			.contributors
			.iter()
			.map(|c| c.file_path.as_str())
			.collect();
		assert!(paths.contains(&"common/scripted_triggers/a.txt"));
		assert!(paths.contains(&"common/scripted_triggers/b.txt"));
	}

	#[test]
	fn base_game_plus_one_mod_not_a_conflict() {
		let index = make_index(
			"scripted_triggers",
			vec![(
				"vanilla_trigger",
				vec![
					make_contributor("base_game", 0, true),
					make_contributor("mod_a", 1, false),
				],
			)],
		);

		let conflicts = detect_key_conflicts(&index);
		assert!(
			conflicts.is_empty(),
			"base game + one mod should not be a conflict (mod overrides base)"
		);
	}

	#[test]
	fn base_game_plus_two_mods_is_conflict() {
		let index = make_index(
			"diplomatic_actions",
			vec![(
				"declarewar",
				vec![
					make_contributor("base_game", 0, true),
					make_contributor("imperial_circles", 1, false),
					make_contributor("europa_expanded", 2, false),
				],
			)],
		);

		let conflicts = detect_key_conflicts(&index);
		assert_eq!(conflicts.len(), 1);
		assert_eq!(conflicts[0].key, "declarewar");
		// All three contributors are present (base + two mods)
		assert_eq!(conflicts[0].contributors.len(), 3);
	}

	#[test]
	fn group_by_family_groups_correctly() {
		use foch_language::analyzer::eu4_profile::eu4_profile;

		let profile = eu4_profile();
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();

		let trigger_path = "common/scripted_triggers/my_mod.txt";
		let effect_path = "common/scripted_effects/my_mod.txt";
		let unclassified_path = "some/random/file.txt";

		let dummy_contributor = ResolvedFileContributor {
			mod_id: "test_mod".to_string(),
			root_path: PathBuf::from("/mods/test"),
			absolute_path: PathBuf::from("/mods/test/common/scripted_triggers/my_mod.txt"),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
		};

		inventory.insert(trigger_path.to_string(), vec![dummy_contributor.clone()]);
		inventory.insert(
			effect_path.to_string(),
			vec![ResolvedFileContributor {
				absolute_path: PathBuf::from("/mods/test/common/scripted_effects/my_mod.txt"),
				..dummy_contributor.clone()
			}],
		);
		inventory.insert(
			unclassified_path.to_string(),
			vec![ResolvedFileContributor {
				absolute_path: PathBuf::from("/mods/test/some/random/file.txt"),
				..dummy_contributor
			}],
		);

		let grouped = group_by_family(&inventory, profile);

		// Unclassified paths should not appear in any family
		for paths in grouped.values() {
			assert!(
				!paths.contains_key(unclassified_path),
				"unclassified path should not appear in any family"
			);
		}

		// scripted_triggers and scripted_effects should each have one entry
		let trigger_family = grouped
			.values()
			.find(|paths| paths.contains_key(trigger_path));
		assert!(
			trigger_family.is_some(),
			"scripted_triggers path should be grouped"
		);

		let effect_family = grouped
			.values()
			.find(|paths| paths.contains_key(effect_path));
		assert!(
			effect_family.is_some(),
			"scripted_effects path should be grouped"
		);
	}

	#[test]
	fn conflicts_are_sorted_by_key() {
		let index = make_index(
			"test",
			vec![
				(
					"zebra",
					vec![
						make_contributor("mod_a", 1, false),
						make_contributor("mod_b", 2, false),
					],
				),
				(
					"alpha",
					vec![
						make_contributor("mod_a", 1, false),
						make_contributor("mod_b", 2, false),
					],
				),
			],
		);

		let conflicts = detect_key_conflicts(&index);
		assert_eq!(conflicts.len(), 2);
		assert_eq!(conflicts[0].key, "alpha");
		assert_eq!(conflicts[1].key, "zebra");
	}
}
