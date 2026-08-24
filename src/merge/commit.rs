//! Commit of an already analyzed and frozen merge artifact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::game::eu4::base::snapshot::{
	InstalledBaseSnapshotCommitGuard, InstalledBaseSnapshotIdentity,
	lock_and_validate_installed_base_snapshot_identity,
};
use crate::input::request::InputRequest;
use crate::input::{InputInventory, resolve_product_input_manifest};
use crate::model::{MergeReport, ProductInputAttestation};
use walkdir::WalkDir;

use super::analyze::{AnalysisStatusView, AnalyzedMerge, MergeStatusView, commit_exit_code};
use super::error::MergeError;
use super::output::materialize::OutputTransaction;

#[derive(Clone, Debug)]
pub(super) struct BaseSnapshotCommitGuard {
	game_key: String,
	game_version: String,
	playlist_path: PathBuf,
	pub(super) identity: InstalledBaseSnapshotIdentity,
}

impl BaseSnapshotCommitGuard {
	pub(super) fn from_inventory(inventory: &InputInventory) -> Result<Option<Self>, MergeError> {
		let Some(identity) = inventory.base_snapshot_identity.as_ref() else {
			return Ok(None);
		};
		let Some(game_version) = inventory.mod_cache_game_version.as_ref() else {
			return Err(MergeError::InputResolve {
				path: inventory.playlist_path.clone(),
				message: "base snapshot identity is missing its game version".to_string(),
			});
		};
		Ok(Some(Self {
			game_key: inventory.playlist.game.key().to_string(),
			game_version: game_version.clone(),
			playlist_path: inventory.playlist_path.clone(),
			identity: identity.clone(),
		}))
	}

	fn validate(&self) -> Result<InstalledBaseSnapshotCommitGuard, MergeError> {
		lock_and_validate_installed_base_snapshot_identity(
			&self.game_key,
			&self.game_version,
			&self.identity,
		)
		.map_err(|message| MergeError::InputResolve {
			path: self.playlist_path.clone(),
			message,
		})
	}
}

#[derive(Clone, Debug)]
pub(super) struct ProductInputCommitGuard {
	request: InputRequest,
	retained_paths: Option<BTreeSet<String>>,
	pub(super) expected: ProductInputAttestation,
}

impl ProductInputCommitGuard {
	pub(super) fn from_inventory(
		request: InputRequest,
		retained_paths: Option<BTreeSet<String>>,
		inventory: &InputInventory,
	) -> Option<Self> {
		Some(Self {
			request,
			retained_paths,
			expected: inventory.product_input_manifest.as_ref()?.attestation(),
		})
	}

	fn validate(&self) -> Result<(), MergeError> {
		let observed = resolve_product_input_manifest(&self.request, self.retained_paths.as_ref())
			.map_err(|error| MergeError::InputResolve {
				path: error.path,
				message: format!(
					"failed to revalidate product inputs before commit: {}",
					error.message
				),
			})?
			.attestation();
		if observed != self.expected {
			return Err(MergeError::InputResolve {
				path: self.request.source_path().to_path_buf(),
				message: format!(
					"product inputs changed during merge: expected {}, observed {}",
					self.expected.digest, observed.digest
				),
			});
		}
		Ok(())
	}
}

#[derive(Clone, Debug)]
pub(super) struct PriorOutputGuard {
	root: PathBuf,
	files: BTreeMap<PathBuf, blake3::Hash>,
}

impl PriorOutputGuard {
	pub(super) fn from_report(
		root: &Path,
		report: &MergeReport,
	) -> Result<Option<Self>, MergeError> {
		let mut files: BTreeMap<PathBuf, blake3::Hash> = BTreeMap::new();
		for resolution in &report.handler_resolutions {
			if !resolution.action.eq_ignore_ascii_case("kept_existing") {
				continue;
			}
			let relative = safe_output_relative_path(Path::new(&resolution.path))?;
			let bytes = fs::read(root.join(&relative))?;
			files.insert(relative, blake3::hash(&bytes));
		}
		if files.is_empty() {
			Ok(None)
		} else {
			Ok(Some(Self {
				root: root.to_path_buf(),
				files,
			}))
		}
	}

	fn validate(&self) -> Result<(), MergeError> {
		for (relative, expected) in &self.files {
			let path = self.root.join(relative);
			let unchanged = fs::read(&path)
				.map(|bytes| blake3::hash(&bytes) == *expected)
				.unwrap_or(false);
			if !unchanged {
				return Err(MergeError::AnalyzedOutputChanged { path });
			}
		}
		Ok(())
	}
}

fn safe_output_relative_path(path: &Path) -> Result<PathBuf, MergeError> {
	if path.as_os_str().is_empty()
		|| path
			.components()
			.any(|component| !matches!(component, std::path::Component::Normal(_)))
	{
		return Err(MergeError::Validation {
			path: Some(path.display().to_string()),
			message: "output path is not a safe relative path".to_string(),
		});
	}
	Ok(path.to_path_buf())
}

fn fingerprint_replacement_target(root: &Path) -> Result<Option<ReplacementTarget>, MergeError> {
	match fs::symlink_metadata(root) {
		Ok(metadata) if metadata.file_type().is_dir() => {}
		Ok(_) => {
			return Err(MergeError::Validation {
				path: Some(root.display().to_string()),
				message: "merge output target is not a directory".to_string(),
			});
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(MergeError::Io(error)),
	}

	let mut entries = WalkDir::new(root)
		.min_depth(1)
		.follow_links(false)
		.into_iter()
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| {
			let message = error.to_string();
			match error.into_io_error() {
				Some(error) => MergeError::Io(io::Error::new(error.kind(), message)),
				None => MergeError::Validation {
					path: Some(root.display().to_string()),
					message,
				},
			}
		})?;
	entries.sort_by(|left, right| left.path().cmp(right.path()));
	if entries.is_empty() {
		return Ok(None);
	}

	let mut hasher = blake3::Hasher::new();
	let mut file_count: usize = 0;
	let mut total_bytes: u64 = 0;
	for entry in entries {
		let relative = entry
			.path()
			.strip_prefix(root)
			.map_err(|_| MergeError::Validation {
				path: Some(entry.path().display().to_string()),
				message: "replacement target entry escaped its root".to_string(),
			})?;
		let relative = safe_output_relative_path(relative)?;
		let relative = relative.to_str().ok_or_else(|| MergeError::Validation {
			path: Some(relative.display().to_string()),
			message: "replacement target path is not valid UTF-8".to_string(),
		})?;
		if entry.file_type().is_dir() {
			hash_replacement_part(&mut hasher, b"directory");
			hash_replacement_part(&mut hasher, relative.as_bytes());
		} else if entry.file_type().is_file() {
			let bytes = fs::read(entry.path())?;
			hash_replacement_part(&mut hasher, b"file");
			hash_replacement_part(&mut hasher, relative.as_bytes());
			hash_replacement_part(&mut hasher, &bytes);
			file_count = file_count.saturating_add(1);
			total_bytes = total_bytes.saturating_add(bytes.len() as u64);
		} else {
			return Err(MergeError::Validation {
				path: Some(entry.path().display().to_string()),
				message: "replacement target contains a symlink or special file".to_string(),
			});
		}
	}

	Ok(Some(ReplacementTarget {
		path: root.to_path_buf(),
		fingerprint: hasher.finalize(),
		file_count,
		total_bytes,
	}))
}

fn validate_replacement_target(
	root: &Path,
	expected: &ReplacementTarget,
) -> Result<(), MergeError> {
	let observed = fingerprint_replacement_target(root)?;
	if expected.path != root || observed.as_ref() != Some(expected) {
		return Err(MergeError::ReplacementTargetChanged {
			path: root.to_path_buf(),
		});
	}
	Ok(())
}

fn hash_replacement_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

fn validate_commit_guards(
	base_snapshot_guard: Option<&BaseSnapshotCommitGuard>,
	product_input_guard: Option<&ProductInputCommitGuard>,
) -> Result<Option<InstalledBaseSnapshotCommitGuard>, MergeError> {
	if let Some(product_input_guard) = product_input_guard {
		product_input_guard.validate()?;
	}
	base_snapshot_guard
		.map(BaseSnapshotCommitGuard::validate)
		.transpose()
}

#[cfg(test)]
pub(super) fn finalize_merge_output<Guard>(
	transaction: OutputTransaction,
	execution: CommitResult,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
) -> Result<CommitResult, MergeError> {
	finalize_merge_output_with_commit(
		transaction,
		execution,
		validate_base_snapshot,
		OutputTransaction::commit,
	)
}

#[cfg(test)]
pub(super) fn finalize_merge_output_with_commit<Guard>(
	transaction: OutputTransaction,
	execution: CommitResult,
	validate_base_snapshot: impl FnOnce(&Path) -> Result<Guard, MergeError>,
	commit: impl FnOnce(OutputTransaction) -> Result<(), MergeError>,
) -> Result<CommitResult, MergeError> {
	super::analyze::write_merge_report_artifact(transaction.staging_dir(), &execution.report)?;
	let _base_snapshot_commit_guard = validate_base_snapshot(transaction.staging_dir())?;
	commit(transaction)?;
	Ok(execution)
}

#[derive(Clone, Debug)]
pub struct CommitResult {
	pub report: MergeReport,
	pub merge_status: MergeStatusView,
	pub analysis_status: AnalysisStatusView,
	pub exit_code: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitAuthorization {
	EmptyTargetOnly,
	ReplaceExisting(ReplacementTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementTarget {
	pub(super) path: PathBuf,
	pub(super) fingerprint: blake3::Hash,
	pub(super) file_count: usize,
	pub(super) total_bytes: u64,
}

impl ReplacementTarget {
	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn file_count(&self) -> usize {
		self.file_count
	}

	pub fn total_bytes(&self) -> u64 {
		self.total_bytes
	}

	pub fn fingerprint(&self) -> String {
		self.fingerprint.to_hex().to_string()
	}
}

impl AnalyzedMerge {
	/// Capture the exact non-empty target the caller is being asked to replace.
	pub fn replacement_target(&self) -> Result<Option<ReplacementTarget>, MergeError> {
		fingerprint_replacement_target(&self.out_dir)
	}

	/// Atomically install the frozen artifacts without invoking analysis again.
	pub fn commit(self, authorization: CommitAuthorization) -> Result<CommitResult, MergeError> {
		let transaction = OutputTransaction::begin(&self.out_dir)?;
		let expected_replacement = match (transaction.prior_dir(), authorization) {
			(None, CommitAuthorization::EmptyTargetOnly) => None,
			(None, CommitAuthorization::ReplaceExisting(_)) => {
				return Err(MergeError::ReplacementTargetChanged { path: self.out_dir });
			}
			(Some(_), CommitAuthorization::EmptyTargetOnly) => {
				return Err(MergeError::ReplacementAuthorizationRequired { path: self.out_dir });
			}
			(Some(_), CommitAuthorization::ReplaceExisting(expected)) => {
				validate_replacement_target(&self.out_dir, &expected)?;
				Some(expected)
			}
		};
		if let Some(guard) = self.prior_output_guard.as_ref() {
			guard.validate()?;
		}
		self.artifacts.copy_into(transaction.staging_dir())?;
		let _base_snapshot_commit_guard = validate_commit_guards(
			self.base_snapshot_commit_guard.as_ref(),
			self.product_input_commit_guard.as_ref(),
		)?;
		if let Some(expected) = expected_replacement.as_ref() {
			validate_replacement_target(&self.out_dir, expected)?;
		}
		transaction.commit()?;
		let exit_code = commit_exit_code(&self.analysis);
		Ok(CommitResult {
			report: self.analysis.report,
			merge_status: self.analysis.merge_status,
			analysis_status: self.analysis.analysis_status,
			exit_code,
		})
	}
}

#[cfg(test)]
mod tests;
