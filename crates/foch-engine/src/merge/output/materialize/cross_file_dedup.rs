use super::super::super::error::MergeError;
use super::super::super::namespace::{FamilyKeyIndex, build_family_key_index, group_by_family};
use super::super::super::normalize::normalize_defines_file;
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace};
use foch_core::model::{HandlerResolutionRecord, MergeReport};
use foch_language::analyzer::content_family::{GameProfile, MergeKeySource};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, is_decision_container_key, parse_script_file_with_profile,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CrossFileKeyValue {
	key: String,
	fingerprint: String,
}

#[derive(Default)]
struct FamilyValueFingerprintIndex {
	file_entries: HashMap<String, Vec<CrossFileKeyValue>>,
	path_key_fingerprints: HashMap<(String, String), Vec<String>>,
}

pub(super) fn prune_cross_file_noop_duplicates(
	out_dir: &Path,
	generated_paths: &mut BTreeSet<String>,
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
	report: &mut MergeReport,
) -> Result<(), MergeError> {
	if generated_paths.is_empty() {
		return Ok(());
	}

	let effective_inventory = build_effective_merged_inventory(out_dir, generated_paths, workspace);
	let grouped = group_by_family(&effective_inventory, profile);
	let mut dropped_paths = BTreeSet::new();

	for (family_id, paths_by_file) in &grouped {
		let Some(descriptor) = profile.descriptor_for_root_family(family_id) else {
			continue;
		};
		if !descriptor.capabilities.dedup_policy.cross_file_safe() {
			continue;
		}
		let Some(merge_key_source) = descriptor.merge_key_source else {
			continue;
		};

		let generated_paths_in_family = generated_paths
			.iter()
			.filter(|path| paths_by_file.contains_key(path.as_str()))
			.cloned()
			.collect::<BTreeSet<_>>();
		if generated_paths_in_family.is_empty() {
			continue;
		}

		let key_index = build_family_key_index(family_id, merge_key_source, paths_by_file, profile);
		let value_index =
			build_family_value_fingerprint_index(paths_by_file, merge_key_source, profile);

		for path in &generated_paths_in_family {
			let Some(entries) = value_index.file_entries.get(path) else {
				continue;
			};
			if entries.is_empty() {
				continue;
			}

			// Deterministic tie-break: a generated file may be covered by vanilla or
			// any non-generated kept output file, but among generated files only a
			// lexicographically earlier surviving path may cover a later one. This
			// keeps the first path when two generated files cross-cover each other.
			let fully_covered = entries.iter().all(|entry| {
				has_cross_file_identical_match(
					&key_index,
					&value_index,
					path,
					entry,
					&generated_paths_in_family,
					&dropped_paths,
				)
			});

			if fully_covered {
				drop_cross_file_noop_path(out_dir, path, family_id, generated_paths, report)?;
				dropped_paths.insert(path.clone());
			}
		}
	}

	Ok(())
}

fn build_effective_merged_inventory(
	out_dir: &Path,
	generated_paths: &BTreeSet<String>,
	workspace: &ResolvedWorkspace,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut all_paths = workspace
		.file_inventory
		.keys()
		.cloned()
		.collect::<BTreeSet<_>>();
	all_paths.extend(generated_paths.iter().cloned());

	let mut inventory = BTreeMap::new();
	for path in all_paths {
		let output_path = out_dir.join(&path);
		if output_path.is_file() {
			inventory.insert(
				path.clone(),
				vec![ResolvedFileContributor {
					mod_id: "__foch_merged_output__".to_string(),
					root_path: out_dir.to_path_buf(),
					absolute_path: output_path,
					precedence: usize::MAX,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint: None,
				}],
			);
			continue;
		}

		let Some(contributors) = workspace.file_inventory.get(&path) else {
			continue;
		};
		if let Some(base) = contributors
			.iter()
			.find(|contributor| contributor.is_base_game)
		{
			inventory.insert(path, vec![base.clone()]);
		}
	}

	inventory
}

fn build_family_value_fingerprint_index(
	paths_by_file: &BTreeMap<String, Vec<ResolvedFileContributor>>,
	merge_key_source: MergeKeySource,
	profile: &dyn GameProfile,
) -> FamilyValueFingerprintIndex {
	let mut index = FamilyValueFingerprintIndex::default();
	for (rel_path, contributors) in paths_by_file {
		for contributor in contributors {
			let Some(parsed) = parse_script_file_with_profile(
				&contributor.mod_id,
				&contributor.root_path,
				&contributor.absolute_path,
				profile,
			) else {
				continue;
			};
			let entries = extract_key_value_fingerprints(&parsed, merge_key_source);
			for entry in &entries {
				index
					.path_key_fingerprints
					.entry((rel_path.clone(), entry.key.clone()))
					.or_default()
					.push(entry.fingerprint.clone());
			}
			index
				.file_entries
				.entry(rel_path.clone())
				.or_default()
				.extend(entries);
		}
	}
	index
}

fn has_cross_file_identical_match(
	key_index: &FamilyKeyIndex,
	value_index: &FamilyValueFingerprintIndex,
	current_path: &str,
	entry: &CrossFileKeyValue,
	generated_paths_in_family: &BTreeSet<String>,
	dropped_paths: &BTreeSet<String>,
) -> bool {
	let Some(contributors) = key_index.entries.get(&entry.key) else {
		return false;
	};

	contributors.iter().any(|contributor| {
		let other_path = contributor.file_path.as_str();
		if other_path == current_path {
			return false;
		}
		if !covering_path_survives(
			current_path,
			other_path,
			generated_paths_in_family,
			dropped_paths,
		) {
			return false;
		}
		value_index
			.path_key_fingerprints
			.get(&(other_path.to_string(), entry.key.clone()))
			.is_some_and(|fingerprints| fingerprints.iter().any(|fp| fp == &entry.fingerprint))
	})
}

fn covering_path_survives(
	current_path: &str,
	other_path: &str,
	generated_paths_in_family: &BTreeSet<String>,
	dropped_paths: &BTreeSet<String>,
) -> bool {
	if !generated_paths_in_family.contains(other_path) {
		return true;
	}
	other_path < current_path && !dropped_paths.contains(other_path)
}

fn drop_cross_file_noop_path(
	out_dir: &Path,
	path: &str,
	family_id: &str,
	generated_paths: &mut BTreeSet<String>,
	report: &mut MergeReport,
) -> Result<(), MergeError> {
	let target = out_dir.join(path);
	match fs::remove_file(&target) {
		Ok(()) => {}
		Err(err) if err.kind() == io::ErrorKind::NotFound => {}
		Err(err) => return Err(MergeError::Io(err)),
	}
	generated_paths.remove(path);
	report.generated_file_count = report.generated_file_count.saturating_sub(1);
	report.cross_file_noop_skipped_file_count += 1;
	report.handler_resolutions.push(HandlerResolutionRecord {
        path: path.to_string(),
        action: "cross_file_noop_skipped".to_string(),
        source: None,
        rationale: Some(format!(
            "all merge keys are already defined identically in another kept file in the {family_id} namespace"
        )),
    });
	Ok(())
}

fn extract_key_value_fingerprints(
	parsed: &ParsedScriptFile,
	merge_key_source: MergeKeySource,
) -> Vec<CrossFileKeyValue> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => extract_assignment_key_values(parsed),
		MergeKeySource::FieldValue(field) => extract_field_value_key_values(parsed, field),
		MergeKeySource::ContainerChildKey => extract_container_child_key_values(parsed),
		MergeKeySource::ContainerChildFieldValue {
			container,
			child_key_field,
			child_types,
		} => extract_container_child_field_value_key_values(
			parsed,
			container,
			child_key_field,
			child_types,
		),
		MergeKeySource::LeafPath => normalize_defines_file(parsed)
			.map(|fragments| {
				fragments
					.into_iter()
					.map(|fragment| CrossFileKeyValue {
						key: fragment.merge_key,
						fingerprint: statement_fingerprint(&fragment.statement),
					})
					.collect()
			})
			.unwrap_or_default(),
	}
}

fn extract_assignment_key_values(parsed: &ParsedScriptFile) -> Vec<CrossFileKeyValue> {
	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => Some(CrossFileKeyValue {
				key: key.clone(),
				fingerprint: statement_fingerprint(stmt),
			}),
			_ => None,
		})
		.collect()
}

fn extract_field_value_key_values(
	parsed: &ParsedScriptFile,
	field: &str,
) -> Vec<CrossFileKeyValue> {
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
			scalar_assignment_value(items, field).map(|key| CrossFileKeyValue {
				key,
				fingerprint: statement_fingerprint(stmt),
			})
		})
		.collect()
}

fn extract_container_child_key_values(parsed: &ParsedScriptFile) -> Vec<CrossFileKeyValue> {
	let mut entries = Vec::new();
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
				entries.push(CrossFileKeyValue {
					key: child_key.clone(),
					fingerprint: container_child_fingerprint(key, item),
				});
			}
		}
	}
	entries
}

fn extract_container_child_field_value_key_values(
	parsed: &ParsedScriptFile,
	container: &str,
	child_key_field: &str,
	child_types: &[&str],
) -> Vec<CrossFileKeyValue> {
	let mut entries = Vec::new();
	for stmt in &parsed.ast.statements {
		let AstStatement::Assignment { key, value, .. } = stmt else {
			continue;
		};
		if key != container {
			entries.push(CrossFileKeyValue {
				key: key.clone(),
				fingerprint: statement_fingerprint(stmt),
			});
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for child in items {
			if let Some(child_key) =
				container_child_field_value_key(child, child_key_field, child_types)
			{
				entries.push(CrossFileKeyValue {
					key: child_key,
					fingerprint: container_child_fingerprint(key, child),
				});
			}
		}
	}
	entries
}

pub(super) fn container_child_field_value_key(
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

pub(super) fn scalar_assignment_value(
	items: &[AstStatement],
	expected_key: &str,
) -> Option<String> {
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

fn container_child_fingerprint(container: &str, child: &AstStatement) -> String {
	let mut out = String::new();
	out.push_str("container:");
	out.push_str(container);
	out.push(';');
	fingerprint_statement_into(child, &mut out);
	out
}

fn statement_fingerprint(statement: &AstStatement) -> String {
	let mut out = String::new();
	fingerprint_statement_into(statement, &mut out);
	out
}

fn fingerprint_statement_into(statement: &AstStatement, out: &mut String) {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			out.push('a');
			out.push_str(key);
			out.push('=');
			fingerprint_value_into(value, out);
			out.push(';');
		}
		AstStatement::Item { value, .. } => {
			out.push('i');
			fingerprint_value_into(value, out);
			out.push(';');
		}
		AstStatement::Comment { .. } => {}
	}
}

fn fingerprint_value_into(value: &AstValue, out: &mut String) {
	match value {
		AstValue::Scalar { value: scalar, .. } => {
			out.push('s');
			out.push(':');
			out.push_str(&scalar.as_text());
		}
		AstValue::Block { items, .. } => {
			out.push('b');
			out.push('[');
			for item in items {
				fingerprint_statement_into(item, out);
			}
			out.push(']');
		}
	}
}
