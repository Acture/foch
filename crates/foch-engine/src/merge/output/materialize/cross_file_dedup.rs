use super::super::super::error::MergeError;
use super::super::super::namespace::{FamilyKeyIndex, build_family_key_index, group_by_family};
use super::super::super::normalize::normalize_defines_file;
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace};
use foch_core::model::{HandlerResolutionRecord, MergeReport};
use foch_language::analyzer::content_family::{GameProfile, MergeKeySource};
use foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue};
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CrossFileSemanticCompleteness {
	#[default]
	FullyTracked,
	ContainsUntracked,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CrossFileValueExtraction {
	entries: Vec<CrossFileKeyValue>,
	completeness: CrossFileSemanticCompleteness,
}

impl CrossFileValueExtraction {
	fn mark_untracked(&mut self) {
		self.completeness = CrossFileSemanticCompleteness::ContainsUntracked;
	}

	fn fully_tracked_entries(&self) -> Option<&[CrossFileKeyValue]> {
		(self.completeness == CrossFileSemanticCompleteness::FullyTracked
			&& !self.entries.is_empty())
		.then_some(self.entries.as_slice())
	}

	fn extend(&mut self, other: Self) {
		self.entries.extend(other.entries);
		if other.completeness == CrossFileSemanticCompleteness::ContainsUntracked {
			self.mark_untracked();
		}
	}
}

#[derive(Default)]
struct FamilyValueFingerprintIndex {
	file_extractions: HashMap<String, CrossFileValueExtraction>,
	path_key_fingerprints: HashMap<(String, String), Vec<String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct CrossFilePruneResult {
	pub surviving_generated_paths: BTreeSet<String>,
	pub pruned_paths: BTreeSet<String>,
}

pub(super) fn prune_cross_file_noop_duplicates(
	out_dir: &Path,
	mut generated_paths: BTreeSet<String>,
	workspace: &ResolvedWorkspace,
	profile: &dyn GameProfile,
	report: &mut MergeReport,
) -> Result<CrossFilePruneResult, MergeError> {
	if generated_paths.is_empty() {
		return Ok(CrossFilePruneResult::default());
	}

	let effective_inventory =
		build_effective_merged_inventory(out_dir, &generated_paths, workspace);
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
			let Some(extraction) = value_index.file_extractions.get(path) else {
				continue;
			};
			let Some(entries) = extraction.fully_tracked_entries() else {
				continue;
			};

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
				drop_cross_file_noop_path(out_dir, path, family_id, report)?;
				generated_paths.remove(path);
				dropped_paths.insert(path.clone());
			}
		}
	}

	Ok(CrossFilePruneResult {
		surviving_generated_paths: generated_paths,
		pruned_paths: dropped_paths,
	})
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
					mod_hash: None,
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
			let extraction = if let Some(parsed) = parse_script_file_with_profile(
				&contributor.mod_id,
				&contributor.root_path,
				&contributor.absolute_path,
				profile,
			) {
				extract_key_value_fingerprints(&parsed, merge_key_source)
			} else {
				CrossFileValueExtraction {
					entries: Vec::new(),
					completeness: CrossFileSemanticCompleteness::ContainsUntracked,
				}
			};
			for entry in &extraction.entries {
				index
					.path_key_fingerprints
					.entry((rel_path.clone(), entry.key.clone()))
					.or_default()
					.push(entry.fingerprint.clone());
			}
			index
				.file_extractions
				.entry(rel_path.clone())
				.or_default()
				.extend(extraction);
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
	report: &mut MergeReport,
) -> Result<(), MergeError> {
	let target = out_dir.join(path);
	match fs::remove_file(&target) {
		Ok(()) => {}
		Err(err) if err.kind() == io::ErrorKind::NotFound => {}
		Err(err) => return Err(MergeError::Io(err)),
	}
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
) -> CrossFileValueExtraction {
	let mut extraction = match merge_key_source {
		MergeKeySource::AssignmentKey => extract_assignment_key_values(parsed),
		MergeKeySource::FieldValue(field) => extract_field_value_key_values(parsed, field),
		MergeKeySource::ContainerChildKey => extract_container_child_key_values(parsed),
		MergeKeySource::ContainerChildFieldValue {
			containers,
			child_key_field,
			child_types,
		} => extract_container_child_field_value_key_values(
			parsed,
			containers,
			child_key_field,
			child_types,
		),
		MergeKeySource::ChildFieldValue { .. } => extract_assignment_key_values(parsed),
		MergeKeySource::LeafPath => match normalize_defines_file(parsed) {
			Ok(fragments) => CrossFileValueExtraction {
				entries: fragments
					.into_iter()
					.map(|fragment| CrossFileKeyValue {
						key: fragment.merge_key,
						fingerprint: statement_fingerprint(&fragment.statement),
					})
					.collect(),
				completeness: CrossFileSemanticCompleteness::FullyTracked,
			},
			Err(_) => CrossFileValueExtraction {
				entries: Vec::new(),
				completeness: CrossFileSemanticCompleteness::ContainsUntracked,
			},
		},
	};
	if !parsed.parse_issues.is_empty() {
		extraction.mark_untracked();
	}
	extraction
}

fn extract_assignment_key_values(parsed: &ParsedScriptFile) -> CrossFileValueExtraction {
	let mut extraction = CrossFileValueExtraction::default();
	for stmt in &parsed.ast.statements {
		match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { .. },
				..
			} => extraction.entries.push(CrossFileKeyValue {
				key: key.clone(),
				fingerprint: statement_fingerprint(stmt),
			}),
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { .. } | AstStatement::Item { .. } => {
				extraction.mark_untracked();
			}
		}
	}
	extraction
}

fn extract_field_value_key_values(
	parsed: &ParsedScriptFile,
	field: &str,
) -> CrossFileValueExtraction {
	let mut extraction = CrossFileValueExtraction::default();
	for stmt in &parsed.ast.statements {
		match stmt {
			AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} => {
				if let Some(key) = scalar_assignment_value(items, field) {
					extraction.entries.push(CrossFileKeyValue {
						key,
						fingerprint: statement_fingerprint(stmt),
					});
				} else {
					extraction.mark_untracked();
				}
			}
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { .. } | AstStatement::Item { .. } => {
				extraction.mark_untracked();
			}
		}
	}
	extraction
}

fn extract_container_child_key_values(parsed: &ParsedScriptFile) -> CrossFileValueExtraction {
	let mut extraction = CrossFileValueExtraction::default();
	for stmt in &parsed.ast.statements {
		match stmt {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if is_decision_container_key(key) => {
				for item in items {
					match item {
						AstStatement::Assignment {
							key: child_key,
							value: AstValue::Block { .. },
							..
						} => extraction.entries.push(CrossFileKeyValue {
							key: child_key.clone(),
							fingerprint: container_child_fingerprint(key, item),
						}),
						AstStatement::Comment { .. } => {}
						AstStatement::Assignment { .. } | AstStatement::Item { .. } => {
							extraction.mark_untracked();
						}
					}
				}
			}
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { .. } | AstStatement::Item { .. } => {
				extraction.mark_untracked();
			}
		}
	}
	extraction
}

fn extract_container_child_field_value_key_values(
	parsed: &ParsedScriptFile,
	containers: &[&str],
	child_key_field: &str,
	child_types: &[&str],
) -> CrossFileValueExtraction {
	let mut extraction = CrossFileValueExtraction::default();
	for stmt in &parsed.ast.statements {
		match stmt {
			AstStatement::Assignment { key, .. } if !containers.contains(&key.as_str()) => {
				extraction.entries.push(CrossFileKeyValue {
					key: key.clone(),
					fingerprint: statement_fingerprint(stmt),
				});
			}
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} => {
				for child in items {
					match child {
						AstStatement::Assignment { .. } => {
							if let Some(child_key) =
								container_child_field_value_key(child, child_key_field, child_types)
							{
								extraction.entries.push(CrossFileKeyValue {
									key: child_key,
									fingerprint: container_child_fingerprint(key, child),
								});
							} else {
								extraction.mark_untracked();
							}
						}
						AstStatement::Comment { .. } => {}
						AstStatement::Item { .. } => extraction.mark_untracked(),
					}
				}
			}
			AstStatement::Comment { .. } => {}
			AstStatement::Assignment { .. } | AstStatement::Item { .. } => {
				extraction.mark_untracked();
			}
		}
	}
	extraction
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
	let mut hasher = blake3::Hasher::new();
	hasher.update(&[0x01]);
	fingerprint_text_into(container, &mut hasher);
	fingerprint_statement_into(child, &mut hasher);
	hasher.finalize().to_hex().to_string()
}

fn statement_fingerprint(statement: &AstStatement) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(&[0x02]);
	fingerprint_statement_into(statement, &mut hasher);
	hasher.finalize().to_hex().to_string()
}

fn fingerprint_statement_into(statement: &AstStatement, hasher: &mut blake3::Hasher) {
	match statement {
		AstStatement::Assignment { key, value, .. } => {
			hasher.update(&[0x10]);
			fingerprint_text_into(key, hasher);
			fingerprint_value_into(value, hasher);
		}
		AstStatement::Item { value, .. } => {
			hasher.update(&[0x11]);
			fingerprint_value_into(value, hasher);
		}
		AstStatement::Comment { .. } => {}
	}
}

fn fingerprint_value_into(value: &AstValue, hasher: &mut blake3::Hasher) {
	match value {
		AstValue::Scalar { value: scalar, .. } => {
			hasher.update(&[0x20]);
			match scalar {
				ScalarValue::Identifier(value) => {
					hasher.update(&[0x21]);
					fingerprint_text_into(value, hasher);
				}
				ScalarValue::String(value) => {
					hasher.update(&[0x22]);
					fingerprint_text_into(value, hasher);
				}
				ScalarValue::Number(value) => {
					hasher.update(&[0x23]);
					fingerprint_text_into(value, hasher);
				}
				ScalarValue::Bool(value) => {
					hasher.update(&[0x24, u8::from(*value)]);
				}
			}
		}
		AstValue::Block { items, .. } => {
			hasher.update(&[0x30]);
			let semantic_item_count = items
				.iter()
				.filter(|item| !matches!(item, AstStatement::Comment { .. }))
				.count() as u64;
			hasher.update(&semantic_item_count.to_le_bytes());
			for item in items
				.iter()
				.filter(|item| !matches!(item, AstStatement::Comment { .. }))
			{
				fingerprint_statement_into(item, hasher);
			}
		}
	}
}

fn fingerprint_text_into(value: &str, hasher: &mut blake3::Hasher) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::Playlist;
	use foch_language::analyzer::content_family::CwtType;
	use foch_language::analyzer::parser::parse_clausewitz_content;
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;

	fn parsed(content: &str) -> ParsedScriptFile {
		let path = PathBuf::from("test.txt");
		let parse_result = parse_clausewitz_content(path.clone(), content);
		assert!(
			parse_result.diagnostics.is_empty(),
			"fixture must parse cleanly: {:?}",
			parse_result.diagnostics
		);
		ParsedScriptFile {
			mod_id: "test_mod".to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("test"),
			module_name: "test".to_string(),
			ast: parse_result.ast,
			source: content.to_string(),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	fn only_statement(content: &str) -> AstStatement {
		let mut statements = parsed(content).ast.statements;
		assert_eq!(statements.len(), 1);
		statements.remove(0)
	}

	fn write_file(root: &Path, relative_path: &str, content: &str) {
		let path = root.join(relative_path);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).expect("create fixture parent");
		}
		fs::write(path, content).expect("write fixture");
	}

	fn prune_workspace(
		test_root: &Path,
		vanilla_path: &str,
		vanilla_content: &str,
		generated_path: &str,
		generated_content: &str,
	) -> ResolvedWorkspace {
		let mut file_inventory = BTreeMap::new();
		for (mod_id, relative_path, content, precedence, is_base_game) in [
			("base_game", vanilla_path, vanilla_content, 0, true),
			("mod_a", generated_path, generated_content, 1, false),
		] {
			let root_path = test_root.join(mod_id);
			write_file(&root_path, relative_path, content);
			file_inventory
				.entry(relative_path.to_string())
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_id.to_string(),
					root_path: root_path.clone(),
					absolute_path: root_path.join(relative_path),
					precedence,
					is_base_game,
					is_synthetic_base: false,
					parse_ok_hint: None,
					mod_hash: None,
				});
		}

		ResolvedWorkspace {
			playlist_path: test_root.join("playlist.json"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "cross-file-completeness".to_string(),
				mods: Vec::new(),
			},
			mods: Vec::new(),
			installed_base_snapshot: None,
			cache_game_version: None,
			mod_snapshots: Vec::new(),
			script_cache: Default::default(),
			file_inventory,
			verified_absent_base_paths: BTreeSet::new(),
			requested_retained_paths: None,
			effective_retained_paths: None,
		}
	}

	fn assert_untracked_file_survives_prune(
		vanilla_path: &str,
		vanilla_content: &str,
		generated_path: &str,
		generated_content: &str,
	) {
		let temp = TempDir::new().expect("temp dir");
		let out_dir = temp.path().join("out");
		let workspace = prune_workspace(
			temp.path(),
			vanilla_path,
			vanilla_content,
			generated_path,
			generated_content,
		);
		write_file(&out_dir, generated_path, generated_content);
		let mut report = MergeReport::default();

		let result = prune_cross_file_noop_duplicates(
			&out_dir,
			BTreeSet::from([generated_path.to_string()]),
			&workspace,
			foch_language::analyzer::eu4_profile::eu4_profile(),
			&mut report,
		)
		.expect("prune completes");

		assert!(out_dir.join(generated_path).is_file());
		assert_eq!(
			result.surviving_generated_paths,
			BTreeSet::from([generated_path.to_string()])
		);
		assert!(result.pruned_paths.is_empty());
		assert!(report.handler_resolutions.is_empty());
	}

	#[test]
	fn event_namespace_makes_whole_file_prune_ineligible() {
		let parsed =
			parsed("namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = test.1.t\n}\n");

		let extraction = extract_key_value_fingerprints(&parsed, MergeKeySource::FieldValue("id"));

		assert_eq!(extraction.entries.len(), 1);
		assert_eq!(extraction.entries[0].key, "test.1");
		assert_eq!(
			extraction.completeness,
			CrossFileSemanticCompleteness::ContainsUntracked
		);
		assert!(extraction.fully_tracked_entries().is_none());
	}

	#[test]
	fn event_namespace_survives_cross_file_prune() {
		let event = "country_event = {\n\tid = test.1\n\ttitle = test.1.t\n}\n";
		assert_untracked_file_survives_prune(
			"events/00_vanilla.txt",
			event,
			"events/zz_generated.txt",
			&format!("namespace = test\n{event}"),
		);
	}

	#[test]
	fn assignment_key_scalar_makes_whole_file_prune_ineligible() {
		let parsed = parsed("scalar_setting = yes\nshared_effect = {\n\tadd_prestige = 1\n}\n");

		let extraction = extract_key_value_fingerprints(&parsed, MergeKeySource::AssignmentKey);

		assert_eq!(extraction.entries.len(), 1);
		assert_eq!(extraction.entries[0].key, "shared_effect");
		assert_eq!(
			extraction.completeness,
			CrossFileSemanticCompleteness::ContainsUntracked
		);
		assert!(extraction.fully_tracked_entries().is_none());
	}

	#[test]
	fn assignment_key_scalar_survives_cross_file_prune() {
		let effect = "shared_effect = {\n\tadd_prestige = 1\n}\n";
		assert_untracked_file_survives_prune(
			"common/scripted_effects/00_vanilla.txt",
			effect,
			"common/scripted_effects/zz_generated.txt",
			&format!("scalar_setting = yes\n{effect}"),
		);
	}

	#[test]
	fn decision_extra_statement_makes_whole_file_prune_ineligible() {
		let parsed = parsed(
			"country_decisions = {\n\tshared_decision = {\n\t\tpotential = { always = yes }\n\t}\n}\nextra_setting = yes\n",
		);

		let extraction = extract_key_value_fingerprints(&parsed, MergeKeySource::ContainerChildKey);

		assert_eq!(extraction.entries.len(), 1);
		assert_eq!(extraction.entries[0].key, "shared_decision");
		assert_eq!(
			extraction.completeness,
			CrossFileSemanticCompleteness::ContainsUntracked
		);
		assert!(extraction.fully_tracked_entries().is_none());
	}

	#[test]
	fn decision_extra_statement_survives_cross_file_prune() {
		let decision = "country_decisions = {\n\tshared_decision = {\n\t\tpotential = { always = yes }\n\t}\n}\n";
		assert_untracked_file_survives_prune(
			"decisions/00_vanilla.txt",
			decision,
			"decisions/zz_generated.txt",
			&format!("{decision}extra_setting = yes\n"),
		);
	}

	#[test]
	fn safe_assignment_file_is_fully_tracked() {
		let parsed = parsed(
			"# retained comments do not affect semantics\nshared_effect = { always = yes }\n",
		);

		let extraction = extract_key_value_fingerprints(&parsed, MergeKeySource::AssignmentKey);

		assert_eq!(
			extraction.completeness,
			CrossFileSemanticCompleteness::FullyTracked
		);
		assert_eq!(extraction.fully_tracked_entries().map(<[_]>::len), Some(1));
	}

	#[test]
	fn structured_fingerprint_distinguishes_scalar_types() {
		let quoted = only_statement("root = { flag = \"yes\" }\n");
		let boolean = only_statement("root = { flag = yes }\n");

		assert_ne!(
			statement_fingerprint(&quoted),
			statement_fingerprint(&boolean)
		);
	}

	#[test]
	fn structured_fingerprint_resists_delimiter_injection_collision() {
		let embedded = only_statement("root = { x = \"v;ay=s:w\" }\n");
		let split = only_statement("root = { x = v y = w }\n");

		assert_ne!(
			statement_fingerprint(&embedded),
			statement_fingerprint(&split)
		);
	}

	#[test]
	fn structured_fingerprint_is_deterministic_and_ignores_comments() {
		let with_comment = only_statement("root = { x = yes # comment\n}\n");
		let without_comment = only_statement("root = { x = yes }\n");

		assert_eq!(
			statement_fingerprint(&with_comment),
			statement_fingerprint(&without_comment)
		);
	}
}
