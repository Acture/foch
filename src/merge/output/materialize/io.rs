use super::super::super::error::MergeError;
use super::{StructuralMergeOutput, provenance_tooltip::PROVENANCE_KEY_PREFIX};
use crate::input::{ResolvedInput, ResolvedInputContributor};
use crate::merge::model::ExternalFileResolution;
use crate::model::{
	HandlerResolutionRecord, MERGE_PLAN_ARTIFACT_PATH, MERGE_REPORT_ARTIFACT_PATH,
	MergePlanContributor, MergePlanEntry, MergePlanResult, MergeReport,
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
	target_path: &str,
	merge_output: &mut StructuralMergeOutput,
	out_dir: &Path,
	prior_out_dir: Option<&Path>,
	resolution_map: &ResolutionMap,
	frozen_external_files: &BTreeMap<PathBuf, Vec<u8>>,
	report: &mut MergeReport,
) -> Result<StructuralOutputMaterialization, MergeError> {
	let output_relative_path = PathBuf::from(target_path);
	let target = out_dir.join(target_path);

	if matches!(
		resolution_map.lookup(Path::new(target_path), "", ""),
		Some(ResolutionDecision::KeepExisting)
	) {
		merge_output
			.keep_existing_paths
			.insert(output_relative_path.clone());
	}

	if merge_output
		.keep_existing_paths
		.contains(&output_relative_path)
	{
		let prior_target = prior_out_dir.map(|prior| prior.join(&output_relative_path));
		if let Some(prior_target) = prior_target.as_ref().filter(|path| path.is_file()) {
			let prior_bytes = fs::read(prior_target)?;
			if prior_bytes
				.windows(PROVENANCE_KEY_PREFIX.len())
				.any(|window| window == PROVENANCE_KEY_PREFIX.as_bytes())
			{
				return Err(MergeError::Validation {
					path: Some(target_path.to_string()),
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
				path: target_path.to_string(),
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

	if let Some(external_resolution) = merge_output
		.external_file_resolutions
		.get(&output_relative_path)
	{
		let source_path = external_resolution.source_path();
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		match external_resolution {
			ExternalFileResolution::Frozen(_) => {
				let bytes = frozen_external_files.get(source_path).ok_or_else(|| {
					MergeError::Validation {
						path: Some(source_path.display().to_string()),
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
			path: target_path.to_string(),
			action: "external".to_string(),
			source: Some(source_path.display().to_string()),
			rationale: None,
		});
		return Ok(StructuralOutputMaterialization::ExternalWrite);
	}

	if merge_output.noop_vs_vanilla {
		// Shipping content equivalent to vanilla would only shadow the game file.
		report.handler_resolutions.push(HandlerResolutionRecord {
			path: target_path.to_string(),
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
	let target = out_dir.join(entry.output_path());
	if let Some(parent) = target.parent() {
		fs::create_dir_all(parent)?;
	}
	copy_file(source, &target).map_err(MergeError::from)
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
	let target = out_dir.join(entry.output_path());
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

pub(super) fn write_generated_descriptor(
	out_dir: &Path,
	playset_path: &Path,
	playset_name: &str,
	replace_prefixes: &BTreeSet<String>,
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
	let mut descriptor = format!(
		"# Source playset: {escaped_playset}\nname=\"{escaped_name}\"\npath=\"{escaped_path}\"\n"
	);
	for prefix in replace_prefixes {
		descriptor.push_str(&format!(
			"replace_path=\"{}\"\n",
			escape_descriptor_value(prefix)
		));
	}
	fs::write(descriptor_path, descriptor)?;
	Ok(())
}

fn winner_source_path<'a>(
	input: &'a ResolvedInput,
	entry: &MergePlanEntry,
) -> Result<&'a Path, MergeError> {
	let winner = entry
		.winner
		.as_ref()
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.output_path().to_string()),
			message: format!(
				"merge plan entry {} is missing a winner",
				entry.output_path()
			),
		})?;
	let contributors = input
		.file_inventory
		.get(entry.output_path())
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.output_path().to_string()),
			message: format!(
				"input is missing contributor inventory for {}",
				entry.output_path()
			),
		})?;
	find_contributor_path(contributors, winner)
		.map(|path| path.as_path())
		.ok_or_else(|| MergeError::Validation {
			path: Some(entry.output_path().to_string()),
			message: format!(
				"winner source {} is missing from input inventory for {}",
				winner.source_path,
				entry.output_path()
			),
		})
}

fn find_contributor_path<'a>(
	contributors: &'a [ResolvedInputContributor],
	winner: &MergePlanContributor,
) -> Option<&'a PathBuf> {
	contributors
		.iter()
		.find(|contributor| normalized_contributor_path(contributor) == winner.source_path)
		.map(|contributor| &contributor.absolute_path)
}

fn normalized_contributor_path(contributor: &ResolvedInputContributor) -> String {
	normalize_path_string(&contributor.absolute_path)
}

fn normalize_path_string(path: &Path) -> String {
	let raw = path.to_string_lossy();
	let stripped = strip_extended_length_prefix(&raw);
	stripped.replace('\\', "/")
}

/// Strip Windows `\\?\` / `\\?\UNC\` extended-length prefixes (and their
/// forward-slash twins) so the value embedded in a Paradox descriptor is
/// loadable by the launcher and the game.
fn strip_extended_length_prefix(path: &str) -> String {
	if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
		format!(r"\\{rest}")
	} else if let Some(rest) = path.strip_prefix(r"\\?\") {
		rest.to_string()
	} else if let Some(rest) = path.strip_prefix("//?/UNC/") {
		format!("//{rest}")
	} else if let Some(rest) = path.strip_prefix("//?/") {
		rest.to_string()
	} else {
		path.to_string()
	}
}

fn escape_descriptor_value(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn is_text_placeholder_path(path: &str) -> bool {
	let normalized = path.to_ascii_lowercase();
	let Some(ext) = normalized.rsplit('.').next() else {
		return false;
	};
	matches!(
		ext,
		"txt" | "lua" | "yml" | "yaml" | "csv" | "json" | "asset" | "gui" | "gfx" | "mod"
	)
}
