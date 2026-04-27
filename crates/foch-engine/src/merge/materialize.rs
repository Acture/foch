#![allow(dead_code)]

use super::emit::{emit_clausewitz_statements, emit_structural_file};
use super::error::MergeError;
use super::ir::{MergeIrStructuralFile, build_merge_ir_from_workspace_and_plan};
#[allow(unused_imports)]
use super::namespace::{build_family_key_index, detect_key_conflicts, group_by_family};
use super::patch_apply::apply_patches;
use super::patch_deps::compute_chained_patches;
use super::patch_merge::{PatchMergeResult, PatchResolution, merge_patch_sets};
use super::plan::build_merge_plan_from_workspace;
use crate::request::{CheckRequest, MergePlanOptions};
use crate::workspace::{ResolvedFileContributor, ResolvedWorkspace, resolve_workspace};
use foch_core::model::{
	MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergePlanStrategy, MergeReport,
	MergeReportRename, MergeReportStatus,
};
use foch_language::analyzer::content_family::{
	ContentFamilyDescriptor, GameProfile, MergeKeySource,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::semantic_index::parse_script_file;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub(crate) struct MergeMaterializeOptions {
	pub include_game_base: bool,
	pub force: bool,
}

impl Default for MergeMaterializeOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
			force: false,
		}
	}
}

pub(crate) fn materialize_merge_internal(
	request: CheckRequest,
	out_dir: &Path,
	options: MergeMaterializeOptions,
) -> Result<MergeReport, MergeError> {
	let mut report = MergeReport::default();
	let mut generated_paths = BTreeSet::new();

	let plan = match resolve_workspace(&request, options.include_game_base) {
		Ok(workspace) => build_merge_plan_from_workspace(&workspace, options.include_game_base),
		Err(_) => crate::run_merge_plan_with_options(
			request.clone(),
			MergePlanOptions {
				include_game_base: options.include_game_base,
			},
		),
	};
	report.manual_conflict_count = plan.strategies.manual_conflict;

	if plan.has_fatal_errors() {
		report.status = MergeReportStatus::Fatal;
		write_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	let workspace = resolve_workspace(&request, options.include_game_base)?;

	// Only build the legacy overlay IR when base game is unavailable
	// (patch engine needs base game files to compute diffs).
	let (ir_structural_files, ir_fatal) = if !options.include_game_base {
		let ir = build_merge_ir_from_workspace_and_plan(&workspace, &plan);
		let fatal = ir.has_fatal_errors();

		// Collect conflict renames for the report
		for file in &ir.structural_files {
			for node in &file.nodes {
				if node.conflict_rename
					&& let Some(original) = &node.original_merge_key
				{
					report.renames.push(MergeReportRename {
						family_id: file.family_id.clone(),
						original_key: original.clone(),
						renamed_key: node.merge_key.clone(),
						mod_id: node.winner.mod_id.clone(),
					});
				}
			}
		}

		let files: BTreeMap<_, _> = ir
			.structural_files
			.into_iter()
			.map(|file| (file.target_path.clone(), file))
			.collect();
		(files, fatal)
	} else {
		(BTreeMap::new(), false)
	};

	if report.manual_conflict_count > 0 && !options.force {
		report.status = MergeReportStatus::Blocked;
		write_metadata_only(out_dir, &plan, &report)?;
		return Ok(report);
	}

	fs::create_dir_all(out_dir)?;
	let descriptor_root = out_dir
		.canonicalize()
		.unwrap_or_else(|_| out_dir.to_path_buf());

	let profile = eu4_profile();

	for entry in &plan.paths {
		match entry.strategy {
			MergePlanStrategy::CopyThrough => {
				copy_winner_file(&workspace, entry, out_dir)?;
				report.copied_file_count += 1;
			}
			MergePlanStrategy::LastWriterOverlay => {
				copy_winner_file(&workspace, entry, out_dir)?;
				report.overlay_file_count += 1;
			}
			MergePlanStrategy::StructuralMerge => {
				let contributors = workspace.file_inventory.get(&entry.path);
				let has_base_game = contributors
					.map(|cs| cs.iter().any(|c| c.is_base_game))
					.unwrap_or(false);

				if has_base_game && let Some(contributors) = contributors {
					// Only use patch engine when 2+ non-base mods contribute
					// (single-mod overlap with base is just last-writer).
					let non_base_count = contributors.iter().filter(|c| !c.is_base_game).count();

					if non_base_count >= 2 {
						let descriptor = profile.classify_content_family(Path::new(&entry.path));
						let merge_key_source = descriptor.and_then(|d| d.merge_key_source);

						if let (Some(descriptor), Some(merge_key_source)) =
							(descriptor, merge_key_source)
						{
							let target = entry.path.clone();
							let contribs = contributors.clone();
							let desc = *descriptor;
							let result = std::panic::catch_unwind(|| {
								patch_based_structural_merge(
									&target,
									&contribs,
									&desc,
									merge_key_source,
								)
							});
							match result {
								Ok(Ok(rendered)) => {
									write_rendered_output(&entry.path, &rendered, out_dir)?;
									generated_paths.insert(entry.path.clone());
									report.generated_file_count += 1;
									continue;
								}
								_ => {
									// Fall through to copy winner
								}
							}
						}
					}

					// Single non-base mod or patch engine failed: copy winner
					copy_winner_file(&workspace, entry, out_dir)?;
					generated_paths.insert(entry.path.clone());
					report.generated_file_count += 1;
				} else {
					// No base game — use overlay IR fallback
					if !ir_fatal && let Some(file) = ir_structural_files.get(&entry.path) {
						write_structural_output(file, out_dir)?;
						generated_paths.insert(entry.path.clone());
						report.generated_file_count += 1;
					}
				}
			}
			MergePlanStrategy::ManualConflict => {
				if options.force {
					if is_text_placeholder_path(&entry.path) {
						write_conflict_placeholder(entry, out_dir)?;
						generated_paths.insert(entry.path.clone());
						report.generated_file_count += 1;
					} else if let Some(contributors) = workspace.file_inventory.get(&entry.path) {
						// Binary conflict: copy highest-precedence (last) mod's version
						if let Some(best) = contributors
							.iter()
							.filter(|c| !c.is_base_game)
							.max_by_key(|c| c.precedence)
						{
							let target = out_dir.join(&entry.path);
							if let Some(parent) = target.parent() {
								fs::create_dir_all(parent)?;
							}
							fs::copy(&best.absolute_path, target)?;
							generated_paths.insert(entry.path.clone());
							report.generated_file_count += 1;
						}
					}
				}
			}
		}
	}

	// Namespace conflict warnings (skipped for large workspaces to avoid
	// excessive parsing; will be done incrementally by the LSP).
	// TODO: re-enable once parse_script_file uses iterative instead of
	// recursive parsing for deeply nested files.
	/*
	let grouped = group_by_family(&workspace.file_inventory, profile);
	for (family_id, paths_by_file) in &grouped {
		let descriptor = profile.descriptor_for_root_family(family_id);
		let merge_key_source = descriptor.and_then(|d| d.merge_key_source);
		if let (Some(_descriptor), Some(merge_key_source)) = (descriptor, merge_key_source) {
			let index =
				build_family_key_index(family_id, merge_key_source, paths_by_file, profile);
			let conflicts = detect_key_conflicts(&index);
			for conflict in &conflicts {
				let mod_ids: Vec<_> = conflict
					.contributors
					.iter()
					.filter(|c| !c.is_base_game)
					.map(|c| format!("{}({})", c.mod_id, c.file_path))
					.collect();
				report.warnings.push(format!(
					"namespace conflict: key '{}' in family '{}' defined by multiple mods: {}",
					conflict.key,
					conflict.family_id,
					mod_ids.join(", "),
				));
			}
		}
	}
	*/

	write_generated_descriptor(
		&descriptor_root,
		&request.playset_path,
		&plan.playset_name,
		&out_dir.join(MERGED_MOD_DESCRIPTOR_PATH),
	)?;

	let mut persisted_plan = plan.clone();
	for entry in &mut persisted_plan.paths {
		entry.generated = generated_paths.contains(&entry.path);
	}
	report.status = if report.manual_conflict_count > 0 && !options.force {
		MergeReportStatus::Blocked
	} else if report.manual_conflict_count > 0 {
		MergeReportStatus::PartialSuccess
	} else {
		MergeReportStatus::Ready
	};
	write_metadata_only(out_dir, &persisted_plan, &report)?;
	Ok(report)
}

/// Patch-based structural merge: diff each mod against its predecessor,
/// merge patch sets, apply to base.  Falls back to overlay merge if base
/// game files are unavailable.
fn patch_based_structural_merge(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
	descriptor: &ContentFamilyDescriptor,
	merge_key_source: MergeKeySource,
) -> Result<String, MergeError> {
	// 1. Compute chained patches for every mod contributor
	let mod_patches = compute_chained_patches(target_path, contributors, merge_key_source)
		.map_err(|err| MergeError::Validation {
			path: Some(target_path.to_string()),
			message: format!("patch computation failed: {err}"),
		})?;

	// 2. Merge all mod patch sets with the family's policies
	let merge_result = merge_patch_sets(mod_patches, &descriptor.merge_policies);

	// 3. Abort if unresolved conflicts exist
	if !merge_result.conflicts.is_empty() {
		let conflict_keys: Vec<_> = merge_result
			.conflicts
			.iter()
			.filter_map(|r| match r {
				PatchResolution::Conflict {
					address, reason, ..
				} => Some(format!("{}: {}", address.key, reason)),
				_ => None,
			})
			.collect();
		return Err(MergeError::Validation {
			path: Some(target_path.to_string()),
			message: format!(
				"patch merge has {} unresolved conflict(s): {}",
				conflict_keys.len(),
				conflict_keys.join("; "),
			),
		});
	}

	// 4. Parse the base (first contributor) file to get the base AST
	let base_contributor = contributors
		.iter()
		.find(|c| c.is_base_game)
		.or_else(|| contributors.first())
		.ok_or_else(|| MergeError::Validation {
			path: Some(target_path.to_string()),
			message: "no contributors for patch merge".to_string(),
		})?;

	let base_parsed = parse_script_file(
		&base_contributor.mod_id,
		&base_contributor.root_path,
		&base_contributor.absolute_path,
	)
	.ok_or_else(|| MergeError::Validation {
		path: Some(target_path.to_string()),
		message: format!(
			"failed to parse base file {} for patch merge",
			base_contributor.absolute_path.display(),
		),
	})?;

	// 5. Collect resolved patches
	let resolved_patches = extract_resolved_patches(&merge_result);

	// 6. Apply patches to base AST
	let merged_statements = apply_patches(
		&base_parsed.ast.statements,
		&resolved_patches,
		merge_key_source,
	);

	// 7. Emit Clausewitz output
	emit_clausewitz_statements(&merged_statements)
}

/// Extract the resolved `ClausewitzPatch` operations from a `PatchMergeResult`.
fn extract_resolved_patches(merge_result: &PatchMergeResult) -> Vec<super::patch::ClausewitzPatch> {
	merge_result
		.resolved
		.iter()
		.filter_map(|resolution| match resolution {
			PatchResolution::Resolved(patch) => Some(patch.clone()),
			PatchResolution::AutoMerged { result, .. } => Some(result.clone()),
			PatchResolution::Conflict { .. } => None,
		})
		.collect()
}

fn write_rendered_output(
	target_path: &str,
	rendered: &str,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let target = out_dir.join(target_path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(target, rendered)?;
	Ok(())
}

fn write_metadata_only(
	out_dir: &Path,
	plan: &MergePlanResult,
	report: &MergeReport,
) -> Result<(), MergeError> {
	fs::create_dir_all(out_dir.join(".foch"))?;
	write_json_artifact(&out_dir.join(MERGE_PLAN_ARTIFACT_PATH), plan)?;
	write_json_artifact(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH), report)?;
	Ok(())
}

fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> Result<(), MergeError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let bytes = serde_json::to_vec_pretty(value).map_err(|err| {
		MergeError::Io(io::Error::other(format!(
			"failed to serialize {}: {err}",
			path.display()
		)))
	})?;
	fs::write(path, bytes)?;
	Ok(())
}

fn copy_winner_file(
	workspace: &ResolvedWorkspace,
	entry: &MergePlanEntry,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let source = winner_source_path(workspace, entry)?;
	let target = out_dir.join(&entry.path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::copy(source, target)?;
	Ok(())
}

fn write_structural_output(file: &MergeIrStructuralFile, out_dir: &Path) -> Result<(), MergeError> {
	let rendered = emit_structural_file(file)?;
	let target = out_dir.join(&file.target_path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(target, rendered)?;
	Ok(())
}

fn write_conflict_placeholder(entry: &MergePlanEntry, out_dir: &Path) -> Result<(), MergeError> {
	let target = out_dir.join(&entry.path);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	let mut lines = vec![
		"FOCH_MERGE_CONFLICT".to_string(),
		format!("path = {}", entry.path),
	];
	if !entry.notes.is_empty() {
		lines.push(format!("notes = {}", entry.notes.join(" | ")));
	}
	lines.push("contributors =".to_string());
	for contributor in &entry.contributors {
		lines.push(format!(
			"- {} [{}] {}",
			contributor.mod_id, contributor.precedence, contributor.source_path
		));
	}
	lines.push(String::new());
	fs::write(target, lines.join("\n"))?;
	Ok(())
}

fn write_generated_descriptor(
	out_dir: &Path,
	playset_path: &Path,
	playset_name: &str,
	descriptor_path: &Path,
) -> Result<(), MergeError> {
	if let Some(parent) = descriptor_path.parent() {
		fs::create_dir_all(parent)?;
	}
	let normalized_out_dir = normalize_path_string(out_dir);
	let normalized_playset_path = normalize_path_string(playset_path);
	let escaped_name = escape_descriptor_value(&format!("{playset_name} (Merged)"));
	let escaped_path = escape_descriptor_value(&normalized_out_dir);
	let escaped_playset = escape_descriptor_value(&normalized_playset_path);
	let descriptor = format!(
		"# Source playset: {escaped_playset}\nname=\"{escaped_name}\"\npath=\"{escaped_path}\"\n"
	);
	fs::write(descriptor_path, descriptor)?;
	Ok(())
}

fn winner_source_path<'a>(
	workspace: &'a ResolvedWorkspace,
	entry: &MergePlanEntry,
) -> Result<&'a Path, MergeError> {
	let winner = entry
		.winner
		.as_ref()
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.path.clone()),
			message: format!("merge plan entry {} is missing a winner", entry.path),
		})?;
	let contributors =
		workspace
			.file_inventory
			.get(&entry.path)
			.ok_or_else(|| MergeError::Validation {
				path: Some(entry.path.clone()),
				message: format!(
					"workspace is missing contributor inventory for {}",
					entry.path
				),
			})?;
	find_contributor_path(contributors, winner)
		.map(|path| path.as_path())
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.path.clone()),
			message: format!(
				"winner source {} is missing from workspace inventory for {}",
				winner.source_path, entry.path
			),
		})
}

fn find_contributor_path<'a>(
	contributors: &'a [ResolvedFileContributor],
	winner: &MergePlanContributor,
) -> Option<&'a PathBuf> {
	contributors
		.iter()
		.find(|contributor| normalized_contributor_path(contributor) == winner.source_path)
		.map(|contributor| &contributor.absolute_path)
}

fn normalized_contributor_path(contributor: &ResolvedFileContributor) -> String {
	normalize_path_string(&contributor.absolute_path)
}

fn normalize_path_string(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn escape_descriptor_value(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_text_placeholder_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};
	matches!(
		ext,
		"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
	)
}

#[cfg(test)]
mod tests {
	use super::{MergeMaterializeOptions, materialize_merge_internal};
	use crate::config::Config;
	use crate::request::CheckRequest;
	use foch_core::model::{
		MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH, MERGED_MOD_DESCRIPTOR_PATH,
		MergePlanEntry, MergePlanResult, MergeReport, MergeReportStatus,
	};
	use serde_json::json;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	fn write_playlist(path: &Path, mods: serde_json::Value) {
		let playlist = json!({
			"game": "eu4",
			"name": "materialize-playset",
			"mods": mods,
		});
		fs::write(
			path,
			serde_json::to_string_pretty(&playlist).expect("serialize playlist"),
		)
		.expect("write playlist");
	}

	fn write_descriptor(mod_root: &Path, name: &str) {
		fs::create_dir_all(mod_root).expect("create mod root");
		fs::write(
			mod_root.join("descriptor.mod"),
			format!("name=\"{name}\"\nversion=\"1.0.0\"\n"),
		)
		.expect("write descriptor");
	}

	fn write_file(mod_root: &Path, relative: &str, content: impl AsRef<[u8]>) {
		let path = mod_root.join(relative);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).expect("create parent");
		}
		fs::write(path, content).expect("write file");
	}

	fn request_for(playlist_path: &Path) -> CheckRequest {
		let game_root = playlist_path
			.parent()
			.expect("playlist parent")
			.join("eu4-game");
		fs::create_dir_all(&game_root).expect("create game root");
		let mut game_path = std::collections::HashMap::new();
		game_path.insert("eu4".to_string(), game_root);
		CheckRequest {
			playset_path: playlist_path.to_path_buf(),
			config: Config {
				steam_root_path: None,
				paradox_data_path: None,
				game_path,
				extra_ignore_patterns: Vec::new(),
			},
		}
	}

	fn no_base_options(force: bool) -> MergeMaterializeOptions {
		MergeMaterializeOptions {
			include_game_base: false,
			force,
		}
	}

	fn read_plan(out_dir: &Path) -> MergePlanResult {
		let bytes =
			fs::read(out_dir.join(MERGE_PLAN_ARTIFACT_PATH)).expect("read merge plan artifact");
		serde_json::from_slice(&bytes).expect("deserialize merge plan artifact")
	}

	fn read_report(out_dir: &Path) -> MergeReport {
		let bytes =
			fs::read(out_dir.join(MERGE_REPORT_ARTIFACT_PATH)).expect("read merge report artifact");
		serde_json::from_slice(&bytes).expect("deserialize merge report artifact")
	}

	fn plan_entry_for<'a>(plan: &'a MergePlanResult, path: &str) -> &'a MergePlanEntry {
		plan.paths
			.iter()
			.find(|entry| entry.path == path)
			.expect("merge plan entry exists")
	}

	#[test]
	fn copy_through_materialization_writes_descriptor_sidecars_and_source_file() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_root = temp.path().join("1001");
		let out_dir = temp.path().join("out");

		write_playlist(
			&playlist_path,
			json!([{ "displayName": "A", "enabled": true, "position": 0, "steamId": "1001" }]),
		);
		write_descriptor(&mod_root, "mod-a");
		write_file(&mod_root, "common/only.txt", "from-a\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.manual_conflict_count, 0);
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(report.copied_file_count, 1);
		assert_eq!(report.overlay_file_count, 0);

		let descriptor =
			fs::read_to_string(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH)).expect("read descriptor");
		assert!(descriptor.contains("name=\"materialize-playset (Merged)\""));
		assert!(descriptor.contains("# Source playset: "));
		assert!(!descriptor.contains("remote_file_id"));
		assert!(!descriptor.contains("supported_version"));
		assert_eq!(
			fs::read_to_string(out_dir.join("common/only.txt")).expect("read copied file"),
			"from-a\n"
		);

		let plan = read_plan(&out_dir);
		assert!(!plan_entry_for(&plan, "common/only.txt").generated);
		let persisted_report = read_report(&out_dir);
		assert_eq!(persisted_report.status, report.status);
		assert_eq!(persisted_report.copied_file_count, 1);
	}

	#[test]
	fn overlay_materialization_copies_only_the_highest_precedence_file() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("2001");
		let mod_b = temp.path().join("2002");
		let out_dir = temp.path().join("out");

		write_playlist(
			&playlist_path,
			json!([
				{ "displayName": "A", "enabled": true, "position": 0, "steamId": "2001" },
				{ "displayName": "B", "enabled": true, "position": 1, "steamId": "2002" }
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_file(&mod_a, "common/overlay.txt", "from-a\n");
		write_file(&mod_b, "common/overlay.txt", "from-b\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.overlay_file_count, 1);
		assert_eq!(report.copied_file_count, 0);
		assert_eq!(report.generated_file_count, 0);
		assert_eq!(
			fs::read_to_string(out_dir.join("common/overlay.txt")).expect("read overlay output"),
			"from-b\n"
		);
	}

	#[test]
	fn structural_materialization_emits_normalized_outputs_and_marks_generated_paths() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("3001");
		let mod_b = temp.path().join("3002");
		let out_dir = temp.path().join("out");

		write_playlist(
			&playlist_path,
			json!([
				{ "displayName": "A", "enabled": true, "position": 0, "steamId": "3001" },
				{ "displayName": "B", "enabled": true, "position": 1, "steamId": "3002" }
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		write_file(
			&mod_a,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.2\n\ttitle = title_b\n}\ncountry_event = {\n\tid = test.1\n\ttitle = title_a\n}\n",
		);
		write_file(
			&mod_b,
			"events/shared.txt",
			"namespace = test\ncountry_event = {\n\tid = test.1\n\ttitle = title_override\n}\n",
		);
		write_file(
			&mod_a,
			"decisions/shared.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = a }\n\t}\n\tunique_decision = {\n\t\teffect = { log = b }\n\t}\n}\n",
		);
		write_file(
			&mod_b,
			"decisions/shared.txt",
			"country_decisions = {\n\ttest_decision = {\n\t\teffect = { log = override }\n\t}\n}\n",
		);
		write_file(
			&mod_a,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = a\n}\nunique_effect = {\n\tlog = only_a\n}\n",
		);
		write_file(
			&mod_b,
			"common/scripted_effects/effects.txt",
			"shared_effect = {\n\tlog = b\n}\n",
		);
		write_file(
			&mod_a,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1444\n\tEND_YEAR = 1821\n\tNCountry = {\n\t\tMAX_IDEA_GROUPS = 8\n\t}\n}\n",
		);
		write_file(
			&mod_b,
			"common/defines/test.txt",
			"NGame = {\n\tSTART_YEAR = 1600\n}\n",
		);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Ready);
		assert_eq!(report.generated_file_count, 4);
		assert_eq!(report.copied_file_count, 0);
		assert_eq!(report.overlay_file_count, 0);

		assert_eq!(
			fs::read_to_string(out_dir.join("events/shared.txt")).expect("read emitted events"),
			"namespace = test\ncountry_event = {\n\tid = test.2\n\ttitle = title_b\n}\ncountry_event = {\n\tid = test.1_3001\n\ttitle = title_a\n}\ncountry_event = {\n\tid = test.1_3002\n\ttitle = title_override\n}\n"
		);
		assert_eq!(
			fs::read_to_string(out_dir.join("decisions/shared.txt"))
				.expect("read emitted decisions"),
			"country_decisions = {\n\ttest_decision_3001 = {\n\t\teffect = {\n\t\t\tlog = a\n\t\t}\n\t}\n\ttest_decision_3002 = {\n\t\teffect = {\n\t\t\tlog = override\n\t\t}\n\t}\n\tunique_decision = {\n\t\teffect = {\n\t\t\tlog = b\n\t\t}\n\t}\n}\n"
		);
		assert_eq!(
			fs::read_to_string(out_dir.join("common/scripted_effects/effects.txt"))
				.expect("read emitted effects"),
			"shared_effect_3001 = {\n\tlog = a\n}\nshared_effect_3002 = {\n\tlog = b\n}\nunique_effect = {\n\tlog = only_a\n}\n"
		);
		assert_eq!(
			fs::read_to_string(out_dir.join("common/defines/test.txt"))
				.expect("read emitted defines"),
			"NGame = {\n\tEND_YEAR = 1821\n\tNCountry = {\n\t\tMAX_IDEA_GROUPS = 8\n\t}\n\tSTART_YEAR = 1600\n}\n"
		);

		let plan = read_plan(&out_dir);
		assert!(plan_entry_for(&plan, "events/shared.txt").generated);
		assert!(plan_entry_for(&plan, "decisions/shared.txt").generated);
		assert!(plan_entry_for(&plan, "common/scripted_effects/effects.txt").generated);
		assert!(plan_entry_for(&plan, "common/defines/test.txt").generated);
	}

	#[test]
	fn manual_conflicts_without_force_write_only_sidecars_and_block_report() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("4001");
		let mod_b = temp.path().join("4002");
		let out_dir = temp.path().join("out");

		write_playlist(
			&playlist_path,
			json!([
				{ "displayName": "A", "enabled": true, "position": 0, "steamId": "4001" },
				{ "displayName": "B", "enabled": true, "position": 1, "steamId": "4002" }
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		// Non-descriptor binary overlap → ManualConflict
		write_file(&mod_a, "pdx_browser/overlap.bin", [0u8, 1, 2, 3]);
		write_file(&mod_b, "pdx_browser/overlap.bin", [4u8, 5, 6, 7]);

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(false),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::Blocked);
		assert_eq!(report.manual_conflict_count, 1);
		assert_eq!(report.generated_file_count, 0);
		assert!(!out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert!(!out_dir.join("pdx_browser/overlap.bin").exists());
		assert!(out_dir.join(MERGE_PLAN_ARTIFACT_PATH).exists());
		assert!(out_dir.join(MERGE_REPORT_ARTIFACT_PATH).exists());

		let plan = read_plan(&out_dir);
		let entry = plan_entry_for(&plan, "pdx_browser/overlap.bin");
		assert!(entry.winner.is_none());
		assert!(!entry.generated);
	}

	#[test]
	fn force_mode_writes_text_placeholders_skips_binary_conflicts_and_keeps_descriptor() {
		let temp = TempDir::new().expect("temp dir");
		let playlist_path = temp.path().join("playlist.json");
		let mod_a = temp.path().join("5001");
		let mod_b = temp.path().join("5002");
		let out_dir = temp.path().join("out");

		write_playlist(
			&playlist_path,
			json!([
				{ "displayName": "A", "enabled": true, "position": 0, "steamId": "5001" },
				{ "displayName": "B", "enabled": true, "position": 1, "steamId": "5002" }
			]),
		);
		write_descriptor(&mod_a, "mod-a");
		write_descriptor(&mod_b, "mod-b");
		// Non-descriptor binary overlaps → ManualConflict
		write_file(&mod_a, "pdx_browser/overlap.bin", [0u8, 1, 2, 3]);
		write_file(&mod_b, "pdx_browser/overlap.bin", [4u8, 5, 6, 7]);
		write_file(&mod_a, "pdx_browser/icon.png", [8u8, 9, 10]);
		write_file(&mod_b, "pdx_browser/icon.png", [11u8, 12, 13]);
		write_file(&mod_b, "common/safe.txt", "safe\n");

		let report = materialize_merge_internal(
			request_for(&playlist_path),
			&out_dir,
			no_base_options(true),
		)
		.expect("materialize");
		assert_eq!(report.status, MergeReportStatus::PartialSuccess);
		assert_eq!(report.manual_conflict_count, 2);
		assert_eq!(report.generated_file_count, 2);
		assert_eq!(report.copied_file_count, 1);
		assert!(out_dir.join(MERGED_MOD_DESCRIPTOR_PATH).exists());
		assert_eq!(
			fs::read_to_string(out_dir.join("common/safe.txt")).expect("read copied safe file"),
			"safe\n"
		);
		// Binary conflicts resolved by copying highest-precedence mod's version
		assert!(out_dir.join("pdx_browser/overlap.bin").exists());
		assert!(out_dir.join("pdx_browser/icon.png").exists());

		let plan = read_plan(&out_dir);
		assert!(plan_entry_for(&plan, "pdx_browser/overlap.bin").generated);
		assert!(plan_entry_for(&plan, "pdx_browser/icon.png").generated);
		assert!(!plan_entry_for(&plan, "common/safe.txt").generated);
	}
}
