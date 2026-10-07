use super::super::super::error::{MergeError, MergeErrorSubject};
use super::{StructuralMergeOutput, provenance_tooltip::PROVENANCE_KEY_PREFIX};
use crate::input::{ResolvedInput, ResolvedInputContributor};
use crate::merge::model::ExternalFileResolution;
use crate::model::{
	GamePath, GamePathBuf, HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH,
	MERGE_REPORT_ARTIFACT_PATH, MergePlanContributor, MergePlanEntry, MergePlanResult, MergeReport,
};
use crate::playset::descriptor::{
	descriptor_comment_text, descriptor_path_text, escape_descriptor_value, replace_path_text,
	slash_path_text,
};
use crate::project::{ResolutionDecision, ResolutionMap};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fs;
use std::io;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StructuralOutputMaterialization {
	NormalWrite,
	ExternalWrite,
	KeptExisting,
	NoopSkippedVsVanilla,
}

impl StructuralOutputMaterialization {
	pub(super) fn counts_as_generated(self) -> bool {
		matches!(self, Self::NormalWrite | Self::ExternalWrite)
	}

	pub(super) fn counts_as_noop_skipped(self) -> bool {
		matches!(self, Self::NoopSkippedVsVanilla)
	}

	pub(super) fn commits_output(self) -> bool {
		matches!(
			self,
			Self::NormalWrite | Self::ExternalWrite | Self::KeptExisting
		)
	}

	pub(super) fn uses_rendered_output(self) -> bool {
		matches!(self, Self::NormalWrite | Self::NoopSkippedVsVanilla)
	}
}

pub(super) fn write_structural_merge_output(
	target_path: &GamePath,
	merge_output: &mut StructuralMergeOutput,
	out_dir: &Path,
	prior_out_dir: Option<&Path>,
	resolution_map: &ResolutionMap,
	frozen_external_files: &BTreeMap<PathBuf, Vec<u8>>,
	report: &mut MergeReport,
) -> Result<StructuralOutputMaterialization, MergeError> {
	let target = target_path.to_path(out_dir);

	if matches!(
		resolution_map.lookup(target_path, "", ""),
		Some(ResolutionDecision::KeepExisting)
	) {
		merge_output
			.keep_existing_paths
			.insert(target_path.to_owned());
	}

	if merge_output.keep_existing_paths.contains(target_path) {
		let prior_target = prior_out_dir.map(|prior| target_path.to_path(prior));
		if let Some(prior_target) = prior_target.as_ref().filter(|path| path.is_file()) {
			let prior_bytes = fs::read(prior_target)?;
			if prior_bytes
				.windows(PROVENANCE_KEY_PREFIX.len())
				.any(|window| window == PROVENANCE_KEY_PREFIX.as_bytes())
			{
				return Err(MergeError::Validation {
					subject: Some(MergeErrorSubject::Game(target_path.to_owned())),
					message: format!(
						"keep_existing cannot carry a script containing {PROVENANCE_KEY_PREFIX} references without its exact generated localisation dependencies",
					),
				});
			}
			if prior_target != &target {
				if let Some(parent) = target.parent() {
					fs::create_dir_all(parent)?;
				}
				fs::copy(prior_target, &target).map_err(|error| {
					MergeError::Io(io::Error::new(
						error.kind(),
						format!(
							"failed to carry kept output {} into staging at {}: {error}",
							prior_target.display(),
							target.display()
						),
					))
				})?;
			}
			report.handler_resolutions.push(HandlerResolutionRecord {
				path: target_path.to_owned(),
				action: "kept_existing".to_string(),
				source: None,
				rationale: None,
			});
			return Ok(StructuralOutputMaterialization::KeptExisting);
		}

		let missing_path = prior_target.as_deref().unwrap_or(&target);
		report.warnings.push(format!(
			"keep_existing_failed: file does not exist in prior output: {}",
			missing_path.display()
		));
	}

	if let Some(external_resolution) = merge_output.external_file_resolutions.get(target_path) {
		let source_path = external_resolution.source_path();
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		match external_resolution {
			ExternalFileResolution::Frozen(_) => {
				let bytes = frozen_external_files.get(source_path).ok_or_else(|| {
					MergeError::Validation {
						subject: Some(MergeErrorSubject::Host(source_path.to_path_buf())),
						message: format!(
							"prepared external resolution payload is missing for {target_path}"
						),
					}
				})?;
				fs::write(&target, bytes)?;
			}
			ExternalFileResolution::Live(_) => {
				let bytes = fs::read(source_path).map_err(|err| {
					MergeError::Io(io::Error::new(
						err.kind(),
						format!(
							"failed to read external resolution source {} for {}: {err}",
							source_path.display(),
							target_path
						),
					))
				})?;
				fs::write(&target, bytes)?;
			}
		}
		report.handler_resolutions.push(HandlerResolutionRecord {
			path: target_path.to_owned(),
			action: "external".to_string(),
			source: Some(source_path.display().to_string()),
			rationale: None,
		});
		return Ok(StructuralOutputMaterialization::ExternalWrite);
	}

	if merge_output.noop_vs_vanilla {
		// Shipping content equivalent to vanilla would only shadow the game file.
		report.handler_resolutions.push(HandlerResolutionRecord {
			path: target_path.to_owned(),
			action: "noop_skipped_vs_vanilla".to_string(),
			source: None,
			rationale: Some(
				"merged content is AST-equal to vanilla; not shipping a redundant copy".to_string(),
			),
		});
		return Ok(StructuralOutputMaterialization::NoopSkippedVsVanilla);
	}

	write_rendered_output(target_path, &merge_output.rendered, out_dir)?;
	report
		.handler_resolutions
		.extend(merge_output.handler_resolutions.iter().cloned());
	Ok(StructuralOutputMaterialization::NormalWrite)
}

fn write_rendered_output(
	target_path: &GamePath,
	rendered: &str,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let target = target_path.to_path(out_dir);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(target, rendered)?;
	Ok(())
}

pub(super) fn write_metadata_only(
	out_dir: &Path,
	plan: &MergePlanResult,
	report: &MergeReport,
) -> Result<(), MergeError> {
	fs::create_dir_all(out_dir.join(".foch"))?;
	write_json_artifact(&out_dir.join(MERGE_PLAN_ARTIFACT_PATH), plan)?;
	write_json_artifact(&out_dir.join(MERGE_REPORT_ARTIFACT_PATH), report)?;
	Ok(())
}

pub(super) fn write_clean_metadata_only(
	out_dir: &Path,
	plan: &MergePlanResult,
	report: &MergeReport,
) -> Result<(), MergeError> {
	clear_output_directory(out_dir)?;
	write_metadata_only(out_dir, plan, report)
}

fn clear_output_directory(path: &Path) -> Result<(), MergeError> {
	match fs::symlink_metadata(path) {
		Ok(metadata) if metadata.file_type().is_dir() => {}
		Ok(_) => {
			return Err(MergeError::Io(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!("output staging root is not a directory: {}", path.display()),
			)));
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			fs::create_dir_all(path)?;
			return Ok(());
		}
		Err(error) => return Err(MergeError::Io(error)),
	}
	for entry in fs::read_dir(path)? {
		remove_output_path(&entry?.path())?;
	}
	Ok(())
}

fn remove_output_path(path: &Path) -> io::Result<()> {
	let metadata = match fs::symlink_metadata(path) {
		Ok(metadata) => metadata,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(error),
	};
	if metadata.file_type().is_dir() {
		fs::remove_dir_all(path)
	} else {
		fs::remove_file(path)
	}
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

pub(super) fn copy_winner_file(
	input: &ResolvedInput,
	entry: &MergePlanEntry,
	out_dir: &Path,
) -> Result<bool, MergeError> {
	let source = winner_source_path(input, entry)?;
	let target = entry.output_path().to_path(out_dir);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	copy_file(&source, &target).map_err(MergeError::from)
}

fn copy_file(source: &Path, target: &Path) -> io::Result<bool> {
	#[cfg(target_os = "macos")]
	if clone_file(source, target)? {
		return Ok(true);
	}

	fs::copy(source, target)?;
	Ok(false)
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, target: &Path) -> io::Result<bool> {
	let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"source path contains a NUL byte",
		)
	})?;
	let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"target path contains a NUL byte",
		)
	})?;
	// SAFETY: both pointers come from live CStrings and clonefile does not retain
	// them after returning. A failure is recoverable via the ordinary copy path.
	let result = unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), 0) };
	Ok(result == 0)
}

pub(super) fn write_conflict_placeholder(
	entry: &MergePlanEntry,
	out_dir: &Path,
) -> Result<(), MergeError> {
	let target = entry.output_path().to_path(out_dir);
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	let mut lines = vec![
		"FOCH_MERGE_CONFLICT".to_string(),
		format!("path = {}", entry.output_path()),
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

/// Write the merged mod's `descriptor.mod`. `path` names `out_dir` for the
/// launcher on this host and each `replace_path` names a replaced directory,
/// both written verbatim, and a directory the format cannot name fails the
/// merge. The source playset is only a comment: it keeps its `/`-joined
/// spelling, escaped, even with a `"` a `path` value could not hold, and a
/// path with no such spelling is shown as the host displays it. A line break
/// in either is escaped so it cannot end the comment.
pub(super) fn write_generated_descriptor(
	out_dir: &Path,
	playset_path: &Path,
	playset_name: &str,
	replace_prefixes: &BTreeSet<GamePathBuf>,
	descriptor_path: &Path,
) -> Result<(), MergeError> {
	if let Some(parent) = descriptor_path.parent() {
		fs::create_dir_all(parent)?;
	}
	let out_dir_text = descriptor_path_text(out_dir).map_err(|reason| MergeError::Validation {
		subject: Some(MergeErrorSubject::Host(out_dir.to_path_buf())),
		message: format!(
			"merged output {} cannot be named in a descriptor: {reason}",
			out_dir.display()
		),
	})?;
	let playset_text =
		slash_path_text(playset_path).unwrap_or_else(|_| playset_path.display().to_string());
	let escaped_name = escape_descriptor_value(&format!("{playset_name} (Merged)"));
	let escaped_playset = descriptor_comment_text(&escape_descriptor_value(&playset_text));
	let mut descriptor = format!(
		"# Source playset: {escaped_playset}\nname=\"{escaped_name}\"\npath=\"{out_dir_text}\"\n"
	);
	for prefix in replace_prefixes {
		let prefix_text = replace_path_text(prefix).map_err(|reason| MergeError::Validation {
			subject: Some(MergeErrorSubject::Game(prefix.clone())),
			message: format!(
				"replaced directory {prefix} cannot be named in a descriptor: {reason}"
			),
		})?;
		descriptor.push_str(&format!("replace_path=\"{prefix_text}\"\n"));
	}
	fs::write(descriptor_path, descriptor)?;
	Ok(())
}

fn winner_source_path(
	input: &ResolvedInput,
	entry: &MergePlanEntry,
) -> Result<PathBuf, MergeError> {
	let winner = entry
		.winner
		.as_ref()
		.ok_or_else(|| MergeError::Validation {
			subject: Some(MergeErrorSubject::Game(entry.output_path().to_owned())),
			message: format!(
				"merge plan entry {} is missing a winner",
				entry.output_path()
			),
		})?;
	let contributors = input
		.file_inventory
		.get(entry.output_path())
		.ok_or_else(|| MergeError::Validation {
			subject: Some(MergeErrorSubject::Game(entry.output_path().to_owned())),
			message: format!(
				"input is missing contributor inventory for {}",
				entry.output_path()
			),
		})?;
	find_contributor_path(contributors, winner).ok_or_else(|| MergeError::Validation {
		subject: Some(MergeErrorSubject::Game(entry.output_path().to_owned())),
		message: format!(
			"winner {}{} at precedence {} (source {}) is missing from input inventory for {}",
			winner.mod_id,
			if winner.is_base_game {
				" (base game)"
			} else {
				""
			},
			winner.precedence,
			winner.source_path,
			entry.output_path()
		),
	})
}

/// The physical file of the contributor the plan names as `winner`. The plan
/// names it by mod, precedence and base-game flag; its displayed source path
/// never selects a file.
fn find_contributor_path(
	contributors: &[ResolvedInputContributor],
	winner: &MergePlanContributor,
) -> Option<PathBuf> {
	contributors
		.iter()
		.find(|contributor| contributor.is_planned_as(winner))
		.map(ResolvedInputContributor::absolute_path)
}

/// Whether a text conflict marker can stand in for `path`, judged by its
/// file extension without regard to case.
pub(super) fn is_text_placeholder_path(path: &GamePath) -> bool {
	path.extension().is_some_and(|extension| {
		matches!(
			extension.to_ascii_lowercase().as_str(),
			"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
		)
	})
}

#[cfg(test)]
mod tests {
	use super::{find_contributor_path, is_text_placeholder_path, write_generated_descriptor};
	use crate::input::ResolvedInputContributor;
	use crate::model::{GamePath, GamePathBuf, MergePlanContributor};
	use crate::playset::descriptor::load_descriptor;
	use std::collections::BTreeSet;
	use std::path::{Path, PathBuf};

	/// The winner is found by the plan's structured identity and copied from the
	/// contributor's own physical file, a verbatim Windows root included. The
	/// plan's displayed source path neither selects nor spells that file.
	#[test]
	fn winner_lookup_preserves_windows_extended_path_identity() {
		for (root, source, plan_path) in [
			(
				r"\\?\D:\mods\tax",
				r"\\?\D:\mods\tax\localisation\p553.yml",
				"D:/mods/tax/localisation/p553.yml",
			),
			(
				r"\\?\UNC\server\mods\tax",
				r"\\?\UNC\server\mods\tax\localisation\p553.yml",
				"//server/mods/tax/localisation/p553.yml",
			),
		] {
			let contributor: ResolvedInputContributor = ResolvedInputContributor {
				mod_id: "tax".to_string(),
				root_path: PathBuf::from(root),
				relative_path: GamePathBuf::parse("localisation/p553.yml")
					.expect("valid game path"),
				precedence: 1,
				is_base_game: false,
				is_synthetic_base: false,
				parse_ok_hint: None,
				mod_hash: None,
			};
			let winner: MergePlanContributor = MergePlanContributor {
				mod_id: "tax".to_string(),
				source_path: plan_path.to_string(),
				precedence: 1,
				is_base_game: false,
			};
			let other: ResolvedInputContributor = ResolvedInputContributor {
				mod_id: "other".to_string(),
				root_path: PathBuf::from(r"D:\mods\other"),
				precedence: 2,
				..contributor.clone()
			};
			assert_eq!(
				find_contributor_path(&[other, contributor.clone()], &winner),
				Some(contributor.absolute_path()),
				"copy lookup must retain the original filesystem path"
			);
			// The game path's components are pushed onto the root unchanged.
			assert_eq!(
				contributor.absolute_path(),
				PathBuf::from(root).join("localisation").join("p553.yml")
			);
			// Joining the game path onto the verbatim root spells the original
			// file; only Windows reads `\\?\` as a prefix.
			#[cfg(windows)]
			assert_eq!(contributor.absolute_path(), PathBuf::from(source));
			#[cfg(not(windows))]
			let _ = source;
		}
	}

	/// A displayed source path is text for people: two files can render alike
	/// (a lossy name, a `\` read as a separator), so it cannot say which file
	/// wins. The plan's identity can.
	#[test]
	fn the_winner_is_found_by_identity_even_when_displayed_paths_collide() {
		let contributor = |mod_id: &str, precedence: usize| ResolvedInputContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from("/mods").join(mod_id),
			relative_path: GamePathBuf::parse("events/a.txt").expect("valid game path"),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: None,
		};
		let contributors = [contributor("low", 1), contributor("high", 2)];
		let winner = MergePlanContributor {
			mod_id: "high".to_string(),
			source_path: "/mods/low/events/a.txt".to_string(),
			precedence: 2,
			is_base_game: false,
		};

		assert_eq!(
			find_contributor_path(&contributors, &winner),
			Some(contributors[1].absolute_path())
		);
	}

	/// Two contributors whose physical files differ only in how a name is
	/// spelled, `a\b` as one Unix name against `a/b` as two, fold to the same
	/// text. The winner is still the one the plan names.
	#[cfg(unix)]
	#[test]
	fn the_winner_is_the_planned_contributor_when_physical_spellings_fold_together() {
		let contributor = |mod_id: &str, root: &str, precedence: usize| ResolvedInputContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(root),
			relative_path: GamePathBuf::parse("events/a.txt").expect("valid game path"),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: None,
		};
		let contributors = [
			contributor("backslash", r"/mods/a\b", 1),
			contributor("nested", "/mods/a/b", 2),
		];
		for (index, planned) in contributors.iter().enumerate() {
			let winner = MergePlanContributor {
				mod_id: planned.mod_id.clone(),
				source_path: "/mods/a/b/events/a.txt".to_string(),
				precedence: planned.precedence,
				is_base_game: false,
			};
			assert_eq!(
				find_contributor_path(&contributors, &winner),
				Some(contributors[index].absolute_path()),
				"{}",
				planned.mod_id
			);
		}
		assert_ne!(
			contributors[0].absolute_path(),
			contributors[1].absolute_path()
		);
	}

	/// The merged mod's descriptor names its output directory and namespace
	/// resets exactly: reading it back yields that directory and those game
	/// paths.
	#[test]
	fn the_generated_descriptor_reads_back_as_its_output_and_replaced_namespaces() {
		let temp = tempfile::tempdir().expect("temp dir");
		let out_dir = temp.path().join("merged output Ölände (1)");
		let descriptor = temp.path().join("descriptor.mod");
		let replaced: BTreeSet<GamePathBuf> = ["common/ideas", "common/static_modifiers"]
			.into_iter()
			.map(|prefix| GamePathBuf::parse(prefix).expect("valid game path"))
			.collect();

		write_generated_descriptor(
			&out_dir,
			&temp.path().join("dlc_load.json"),
			"playset",
			&replaced,
			&descriptor,
		)
		.expect("write descriptor");

		let read_back = load_descriptor(&descriptor).expect("read descriptor");
		assert_eq!(read_back.path, Some(out_dir));
		assert_eq!(
			read_back.replace_path,
			replaced.into_iter().collect::<Vec<_>>()
		);
	}

	/// A descriptor `path` has no spelling for a `"`, since it is read
	/// without escapes, or for a `\` inside a name, which off Windows is an
	/// ordinary name character: the merge fails instead of writing text that
	/// names another directory.
	#[cfg(unix)]
	#[test]
	fn an_output_directory_a_descriptor_cannot_name_fails_the_merge() {
		// In-memory output paths: only the descriptor file would be written.
		let temp = tempfile::tempdir().expect("temp dir");
		let descriptor = temp.path().join("descriptor.mod");
		for name in [r"merged\output", "merged \"quoted\" output"] {
			let error = write_generated_descriptor(
				&temp.path().join(name),
				Path::new("/playsets/dlc_load.json"),
				"playset",
				&BTreeSet::new(),
				&descriptor,
			)
			.expect_err(name);
			assert!(
				error
					.to_string()
					.contains("cannot be named in a descriptor"),
				"{error}"
			);
			assert!(!descriptor.exists());
		}
	}

	/// A `replace_path` is read without escapes, so a replaced directory
	/// holding a `"` has no spelling: the merge fails instead of writing a
	/// value that ends early and names another directory.
	#[test]
	fn a_replaced_directory_a_descriptor_cannot_name_fails_the_merge() {
		let temp = tempfile::tempdir().expect("temp dir");
		let descriptor = temp.path().join("descriptor.mod");
		let replaced: BTreeSet<GamePathBuf> = ["common/ideas", "common/a\"b"]
			.into_iter()
			.map(|prefix| GamePathBuf::parse(prefix).expect("valid game path"))
			.collect();
		let error = write_generated_descriptor(
			&temp.path().join("merged"),
			Path::new("/playsets/dlc_load.json"),
			"playset",
			&replaced,
			&descriptor,
		)
		.expect_err("a quote in a replaced directory");
		let message = error.to_string();
		assert!(message.contains("common/a\"b"), "{message}");
		assert!(
			message.contains("cannot be named in a descriptor"),
			"{message}"
		);
		assert!(!descriptor.exists());
	}

	/// The source playset comment keeps the `/`-joined spelling of the
	/// playset path, escaped, even when the path holds a `"` that a `path`
	/// value could not: repeated separators are not rendered as displayed.
	#[test]
	fn the_source_playset_comment_keeps_its_joined_spelling() {
		let temp = tempfile::tempdir().expect("temp dir");
		let descriptor = temp.path().join("descriptor.mod");
		write_generated_descriptor(
			&temp.path().join("merged"),
			Path::new("/tmp/p \"q\"//dlc_load.json"),
			"playset",
			&BTreeSet::new(),
			&descriptor,
		)
		.expect("write descriptor");
		let written = std::fs::read_to_string(&descriptor).expect("read descriptor");
		assert_eq!(
			written.lines().next(),
			Some(r#"# Source playset: /tmp/p \"q\"/dlc_load.json"#)
		);
	}

	/// A line break in the playset path would end the comment and turn the
	/// rest of the name into descriptor fields. It is escaped instead, so the
	/// descriptor still has exactly the fields Foch wrote.
	#[test]
	fn a_line_break_in_the_source_playset_cannot_add_descriptor_fields() {
		let temp = tempfile::tempdir().expect("temp dir");
		let descriptor = temp.path().join("descriptor.mod");
		write_generated_descriptor(
			&temp.path().join("merged"),
			Path::new("/tmp/x\nreplace_path=\"common\"\r\n#/dlc_load.json"),
			"playset",
			&BTreeSet::new(),
			&descriptor,
		)
		.expect("write descriptor");
		let written = std::fs::read_to_string(&descriptor).expect("read descriptor");
		assert_eq!(
			written.lines().next(),
			Some(r#"# Source playset: /tmp/x\nreplace_path=\"common\"\r\n#/dlc_load.json"#)
		);
		let parsed = crate::playset::descriptor::load_descriptor(&descriptor)
			.expect("parse generated descriptor");
		assert!(parsed.replace_path.is_empty(), "{written}");
	}

	#[test]
	fn text_placeholders_are_chosen_by_file_extension() {
		for (path, text) in [
			("common/ideas/00_ideas.txt", true),
			("interface/Custom.GUI", true),
			("localisation/replace/x_l_english.yml", true),
			("gfx/interface/icon.dds", false),
			("common/ideas.txt/extensionless", false),
			("music/songs", false),
			// A name that is only a dot and an extension, a dotfile, has no
			// extension; the old `rsplit('.')` text split read one.
			("common/ideas/.txt", false),
		] {
			assert_eq!(
				is_text_placeholder_path(GamePath::new(path).expect("valid game path")),
				text,
				"{path}"
			);
		}
	}
}
