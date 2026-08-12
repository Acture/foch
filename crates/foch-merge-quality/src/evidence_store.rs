//! Compact, content-addressed evidence bundles for live Workshop measurements.
//!
//! Unlike the legacy object store, this API has no recursive tree snapshot
//! operation. Callers must enumerate every report or scorer-relevant file and
//! give it an explicit bundle destination.

use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::dataset::{ArtifactHashAlgorithm, DatasetPaths, stable_id};

pub const EVIDENCE_BUNDLE_SCHEMA: &str = "2.0.0";
const MANIFEST_NAME: &str = "manifest.json";
const FILES_DIR: &str = "files";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceEntryKind {
	ProductInputManifest,
	MergeReport,
	ScorerConfig,
	ScorerEvidenceIndex,
	FileResult,
	BaseInput,
	SourceInput,
	CompatchInput,
	MergedOutput,
}

impl EvidenceEntryKind {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::ProductInputManifest => "product_input_manifest",
			Self::MergeReport => "merge_report",
			Self::ScorerConfig => "scorer_config",
			Self::ScorerEvidenceIndex => "scorer_evidence_index",
			Self::FileResult => "file_result",
			Self::BaseInput => "base_input",
			Self::SourceInput => "source_input",
			Self::CompatchInput => "compatch_input",
			Self::MergedOutput => "merged_output",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceContent {
	Bytes(Vec<u8>),
	SourcePath(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceEntryInput {
	pub kind: EvidenceEntryKind,
	pub relative_path: String,
	pub content: EvidenceContent,
}

impl EvidenceEntryInput {
	pub fn bytes(
		kind: EvidenceEntryKind,
		relative_path: impl Into<String>,
		content: impl Into<Vec<u8>>,
	) -> Self {
		Self {
			kind,
			relative_path: relative_path.into(),
			content: EvidenceContent::Bytes(content.into()),
		}
	}

	pub fn source_file(
		kind: EvidenceEntryKind,
		relative_path: impl Into<String>,
		source_path: impl Into<PathBuf>,
	) -> Self {
		Self {
			kind,
			relative_path: relative_path.into(),
			content: EvidenceContent::SourcePath(source_path.into()),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceBundleInput {
	pub measurement_id: String,
	pub input_version_id: String,
	pub entries: Vec<EvidenceEntryInput>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundleStats {
	pub files: u64,
	pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEntryRecord {
	pub kind: EvidenceEntryKind,
	pub relative_path: String,
	pub hash_algorithm: ArtifactHashAlgorithm,
	pub hash: String,
	pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundleManifest {
	pub schema: String,
	pub bundle_hash: String,
	pub measurement_id: String,
	pub input_version_id: String,
	pub stats: EvidenceBundleStats,
	pub entries: Vec<EvidenceEntryRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundleRef {
	pub hash_algorithm: ArtifactHashAlgorithm,
	pub hash: String,
	pub stats: EvidenceBundleStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredEvidenceBundle {
	pub hash: String,
	pub path: PathBuf,
	pub stats: EvidenceBundleStats,
	pub manifest: EvidenceBundleManifest,
	pub newly_stored: bool,
}

impl StoredEvidenceBundle {
	pub fn reference(&self) -> EvidenceBundleRef {
		EvidenceBundleRef {
			hash_algorithm: ArtifactHashAlgorithm::Blake3,
			hash: self.hash.clone(),
			stats: self.stats,
		}
	}

	/// Read one manifest-listed entry and verify its bytes at the point of use.
	pub fn read_entry(&self, relative_path: &str) -> io::Result<Vec<u8>> {
		validate_relative_path(relative_path)?;
		validate_hash(&self.hash)?;
		let mut matching_entries = self
			.manifest
			.entries
			.iter()
			.filter(|entry| entry.relative_path == relative_path);
		let entry = matching_entries.next().ok_or_else(|| {
			io::Error::new(
				ErrorKind::NotFound,
				format!("evidence entry is not listed in the manifest: {relative_path}"),
			)
		})?;
		if matching_entries.next().is_some() {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("evidence manifest lists {relative_path} more than once"),
			));
		}
		validate_manifest(&self.hash, &self.manifest)?;
		if self.stats != self.manifest.stats {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"evidence bundle {} has inconsistent stored stats",
					self.hash
				),
			));
		}

		let bundle = require_stored_bundle(&self.path, &self.hash)?;
		let content =
			read_regular_file_beneath(&bundle, &Path::new(FILES_DIR).join(relative_path))?;
		if content.len() as u64 != entry.bytes || blake3_hash(&content) != entry.hash {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"evidence entry {relative_path} is corrupt in bundle {}",
					self.hash
				),
			));
		}
		Ok(content)
	}
}

#[derive(Clone, Debug)]
pub struct EvidenceStore {
	root: PathBuf,
	work: PathBuf,
}

impl EvidenceStore {
	pub fn new(root: impl Into<PathBuf>, work: impl Into<PathBuf>) -> Self {
		Self {
			root: root.into(),
			work: work.into(),
		}
	}

	pub fn for_dataset(paths: &DatasetPaths) -> Self {
		Self::new(&paths.evidence_objects, &paths.evidence_work)
	}

	pub fn bundle_dir(&self, hash: &str) -> io::Result<PathBuf> {
		validate_hash(hash)?;
		let root = absolute_clean_path(&self.root)?;
		if fs::symlink_metadata(&root).is_ok() {
			let checked_root = require_directory(&root, "evidence store root")?;
			let shard = Path::new(&hash[..2]);
			if fs::symlink_metadata(root.join(shard)).is_ok() {
				require_directory_beneath(&checked_root, shard, "evidence shard")?;
			}
		}
		Ok(root.join(&hash[..2]).join(hash))
	}

	pub fn store(&self, input: EvidenceBundleInput) -> io::Result<StoredEvidenceBundle> {
		if input.measurement_id.is_empty() || input.input_version_id.is_empty() {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				"evidence bundle IDs must not be empty",
			));
		}
		let prepared = prepare_entries(input.entries)?;
		if prepared.is_empty() {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				"evidence bundle must contain at least one explicit entry",
			));
		}
		let stats = bundle_stats(&prepared)?;
		let records: Vec<EvidenceEntryRecord> =
			prepared.iter().map(|entry| entry.record.clone()).collect();
		let bundle_hash = bundle_hash(
			&input.measurement_id,
			&input.input_version_id,
			stats,
			&records,
		);
		let manifest = EvidenceBundleManifest {
			schema: EVIDENCE_BUNDLE_SCHEMA.to_string(),
			bundle_hash: bundle_hash.clone(),
			measurement_id: input.measurement_id,
			input_version_id: input.input_version_id,
			stats,
			entries: records,
		};
		let root = ensure_directory(&self.root, "evidence store root")?;
		let work = ensure_directory(&self.work, "evidence work root")?;
		validate_disjoint_roots(&root, &work)?;
		let shard =
			ensure_directory_beneath(&root, Path::new(&bundle_hash[..2]), "evidence shard")?;
		let destination = shard.path.join(&bundle_hash);
		if fs::symlink_metadata(&destination).is_ok() {
			require_directory_beneath(&shard, Path::new(&bundle_hash), "evidence bundle")?;
			let stored = self.open(&bundle_hash)?;
			if stored.manifest != manifest {
				return Err(io::Error::new(
					ErrorKind::AlreadyExists,
					format!("evidence bundle {bundle_hash} has conflicting metadata"),
				));
			}
			return Ok(stored);
		}

		let staging = tempfile::Builder::new()
			.prefix("evidence-")
			.tempdir_in(&work.path)?;
		let staging_name = staging.path().file_name().ok_or_else(|| {
			io::Error::new(ErrorKind::InvalidData, "evidence staging path has no name")
		})?;
		let checked_staging = require_directory_beneath(
			&work,
			Path::new(staging_name),
			"evidence staging directory",
		)?;
		let staging_files = ensure_directory_beneath(
			&checked_staging,
			Path::new(FILES_DIR),
			"evidence staging files directory",
		)?;
		for entry in &prepared {
			write_new_file_beneath(
				&staging_files,
				Path::new(&entry.record.relative_path),
				&entry.content,
			)?;
		}
		let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
		write_new_file_beneath(&checked_staging, Path::new(MANIFEST_NAME), &manifest_bytes)?;

		match fs::rename(&checked_staging.path, &destination) {
			Ok(()) => {
				let mut stored = self.open(&bundle_hash)?;
				stored.newly_stored = true;
				Ok(stored)
			}
			Err(_) if fs::symlink_metadata(&destination).is_ok() => {
				let stored = self.open(&bundle_hash)?;
				if stored.manifest != manifest {
					return Err(io::Error::new(
						ErrorKind::AlreadyExists,
						format!("evidence bundle {bundle_hash} raced with conflicting metadata"),
					));
				}
				Ok(stored)
			}
			Err(error) => Err(error),
		}
	}

	/// Open and fully verify a compact evidence bundle.
	pub fn open(&self, hash: &str) -> io::Result<StoredEvidenceBundle> {
		validate_hash(hash)?;
		let root = require_directory(&self.root, "evidence store root")?;
		let shard = require_directory_beneath(&root, Path::new(&hash[..2]), "evidence shard")?;
		let bundle = require_directory_beneath(&shard, Path::new(hash), "evidence bundle")?;
		let manifest_path = bundle.path.join(MANIFEST_NAME);
		let manifest: EvidenceBundleManifest = serde_json::from_slice(&read_regular_file_beneath(
			&bundle,
			Path::new(MANIFEST_NAME),
		)?)
		.map_err(|error| {
			io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"invalid evidence manifest {}: {error}",
					manifest_path.display()
				),
			)
		})?;
		validate_manifest(hash, &manifest)?;
		validate_bundle_layout(&bundle, &manifest)?;
		for entry in &manifest.entries {
			let content = read_regular_file_beneath(
				&bundle,
				&Path::new(FILES_DIR).join(&entry.relative_path),
			)?;
			if content.len() as u64 != entry.bytes || blake3_hash(&content) != entry.hash {
				return Err(io::Error::new(
					ErrorKind::InvalidData,
					format!(
						"evidence entry {} is corrupt in bundle {hash}",
						entry.relative_path
					),
				));
			}
		}
		Ok(StoredEvidenceBundle {
			hash: hash.to_string(),
			path: bundle.path,
			stats: manifest.stats,
			manifest,
			newly_stored: false,
		})
	}
}

#[derive(Clone, Debug)]
struct PreparedEntry {
	record: EvidenceEntryRecord,
	content: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CheckedDirectory {
	path: PathBuf,
	canonical: PathBuf,
}

fn absolute_clean_path(path: &Path) -> io::Result<PathBuf> {
	let absolute = if path.is_absolute() {
		path.to_path_buf()
	} else {
		std::env::current_dir()?.join(path)
	};
	if absolute
		.components()
		.any(|component| matches!(component, Component::ParentDir | Component::CurDir))
	{
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("path must not contain dot components: {}", path.display()),
		));
	}
	Ok(absolute)
}

fn require_directory(path: &Path, label: &str) -> io::Result<CheckedDirectory> {
	let path = absolute_clean_path(path)?;
	let metadata = fs::symlink_metadata(&path)?;
	if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"{label} must be a real directory, not a symlink: {}",
				path.display()
			),
		));
	}
	let canonical = fs::canonicalize(&path)?;
	let canonical_metadata = fs::symlink_metadata(&canonical)?;
	if canonical_metadata.file_type().is_symlink() || !canonical_metadata.file_type().is_dir() {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"{label} does not resolve to a regular directory: {}",
				path.display()
			),
		));
	}
	Ok(CheckedDirectory { path, canonical })
}

fn ensure_directory(path: &Path, label: &str) -> io::Result<CheckedDirectory> {
	let path = absolute_clean_path(path)?;
	match fs::symlink_metadata(&path) {
		Ok(_) => require_directory(&path, label),
		Err(error) if error.kind() == ErrorKind::NotFound => {
			let parent = path.parent().ok_or_else(|| {
				io::Error::new(
					ErrorKind::InvalidInput,
					format!("{label} has no parent: {}", path.display()),
				)
			})?;
			require_directory(parent, &format!("{label} parent"))?;
			match fs::create_dir(&path) {
				Ok(()) => {}
				Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
				Err(error) => return Err(error),
			}
			require_directory(&path, label)
		}
		Err(error) => Err(error),
	}
}

fn validate_disjoint_roots(root: &CheckedDirectory, work: &CheckedDirectory) -> io::Result<()> {
	if root.canonical == work.canonical
		|| root.canonical.starts_with(&work.canonical)
		|| work.canonical.starts_with(&root.canonical)
	{
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			"evidence store and work roots must be disjoint",
		));
	}
	Ok(())
}

fn require_stored_bundle(path: &Path, hash: &str) -> io::Result<CheckedDirectory> {
	let path = absolute_clean_path(path)?;
	let shard_path = path.parent().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle path has no shard: {}", path.display()),
		)
	})?;
	let root_path = shard_path.parent().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle path has no store root: {}", path.display()),
		)
	})?;
	if path.file_name() != Some(Path::new(hash).as_os_str())
		|| shard_path.file_name() != Some(Path::new(&hash[..2]).as_os_str())
	{
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"evidence bundle path does not match hash {hash}: {}",
				path.display()
			),
		));
	}
	let root = require_directory(root_path, "evidence store root")?;
	let shard = require_directory_beneath(&root, Path::new(&hash[..2]), "evidence shard")?;
	let bundle = require_directory_beneath(&shard, Path::new(hash), "evidence bundle")?;
	if bundle.path != path {
		return Err(io::Error::new(
			ErrorKind::PermissionDenied,
			format!("evidence bundle escapes its store root: {}", path.display()),
		));
	}
	Ok(bundle)
}

fn validate_relative_filesystem_path(path: &Path) -> io::Result<()> {
	if path.as_os_str().is_empty()
		|| path.is_absolute()
		|| path
			.components()
			.any(|component| !matches!(component, Component::Normal(_)))
	{
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("path must be a canonical relative path: {}", path.display()),
		));
	}
	Ok(())
}

fn require_directory_beneath(
	parent: &CheckedDirectory,
	relative: &Path,
	label: &str,
) -> io::Result<CheckedDirectory> {
	validate_relative_filesystem_path(relative)?;
	let mut current = parent.path.clone();
	let mut canonical = parent.canonical.clone();
	for component in relative.components() {
		let Component::Normal(component) = component else {
			unreachable!("relative path was validated")
		};
		current.push(component);
		let metadata = fs::symlink_metadata(&current)?;
		if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"{label} contains a non-directory or symlink: {}",
					current.display()
				),
			));
		}
		canonical = fs::canonicalize(&current)?;
		if !canonical.starts_with(&parent.canonical) {
			return Err(io::Error::new(
				ErrorKind::PermissionDenied,
				format!("{label} escapes its root: {}", current.display()),
			));
		}
	}
	Ok(CheckedDirectory {
		path: current,
		canonical,
	})
}

fn ensure_directory_beneath(
	parent: &CheckedDirectory,
	relative: &Path,
	label: &str,
) -> io::Result<CheckedDirectory> {
	validate_relative_filesystem_path(relative)?;
	let mut current = parent.clone();
	for component in relative.components() {
		let Component::Normal(component) = component else {
			unreachable!("relative path was validated")
		};
		let child_relative = Path::new(component);
		let child_path = current.path.join(child_relative);
		match fs::symlink_metadata(&child_path) {
			Ok(_) => {}
			Err(error) if error.kind() == ErrorKind::NotFound => {
				match fs::create_dir(&child_path) {
					Ok(()) => {}
					Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
					Err(error) => return Err(error),
				}
			}
			Err(error) => return Err(error),
		}
		current = require_directory_beneath(&current, child_relative, label)?;
		if !current.canonical.starts_with(&parent.canonical) {
			return Err(io::Error::new(
				ErrorKind::PermissionDenied,
				format!("{label} escapes its root: {}", current.path.display()),
			));
		}
	}
	Ok(current)
}

fn validate_regular_file_beneath(
	root: &CheckedDirectory,
	relative: &Path,
	label: &str,
) -> io::Result<PathBuf> {
	validate_relative_filesystem_path(relative)?;
	let file_name = relative.file_name().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("{label} path has no file name: {}", relative.display()),
		)
	})?;
	let parent = match relative.parent() {
		Some(parent) if !parent.as_os_str().is_empty() => {
			require_directory_beneath(root, parent, label)?
		}
		_ => root.clone(),
	};
	let path = parent.path.join(file_name);
	let metadata = fs::symlink_metadata(&path)?;
	if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!(
				"{label} must be one regular file, not a symlink: {}",
				path.display()
			),
		));
	}
	let canonical = fs::canonicalize(&path)?;
	if !canonical.starts_with(&root.canonical) {
		return Err(io::Error::new(
			ErrorKind::PermissionDenied,
			format!("{label} escapes its root: {}", path.display()),
		));
	}
	Ok(path)
}

fn open_regular_file_no_follow(path: &Path) -> io::Result<File> {
	let mut options = OpenOptions::new();
	options.read(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
	}
	options.open(path)
}

fn open_new_file_no_follow(path: &Path) -> io::Result<File> {
	let mut options = OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
		options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
	}
	options.open(path)
}

fn read_regular_file_beneath(root: &CheckedDirectory, relative: &Path) -> io::Result<Vec<u8>> {
	let path = validate_regular_file_beneath(root, relative, "evidence bundle file")?;
	let path_before = source_file_fingerprint(&fs::symlink_metadata(&path)?);
	let mut file = open_regular_file_no_follow(&path)?;
	let handle_before = source_file_fingerprint(&file.metadata()?);
	if path_before != handle_before {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"evidence bundle file changed before reading: {}",
				path.display()
			),
		));
	}
	let mut content = Vec::with_capacity(usize::try_from(handle_before.len).unwrap_or(0));
	file.read_to_end(&mut content)?;
	let handle_after = source_file_fingerprint(&file.metadata()?);
	let path_after = source_file_fingerprint(&fs::symlink_metadata(&path)?);
	if handle_before != handle_after
		|| handle_after != path_after
		|| content.len() as u64 != handle_after.len
	{
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"evidence bundle file changed while reading: {}",
				path.display()
			),
		));
	}
	validate_regular_file_beneath(root, relative, "evidence bundle file")?;
	Ok(content)
}

fn write_new_file_beneath(
	root: &CheckedDirectory,
	relative: &Path,
	content: &[u8],
) -> io::Result<()> {
	validate_relative_filesystem_path(relative)?;
	let file_name = relative.file_name().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!(
				"evidence destination has no file name: {}",
				relative.display()
			),
		)
	})?;
	let parent = match relative.parent() {
		Some(parent) if !parent.as_os_str().is_empty() => {
			ensure_directory_beneath(root, parent, "evidence destination parent")?
		}
		_ => root.clone(),
	};
	let path = parent.path.join(file_name);
	let mut file = open_new_file_no_follow(&path)?;
	file.write_all(content)?;
	file.flush()?;
	validate_regular_file_beneath(root, relative, "evidence destination")?;
	Ok(())
}

fn source_boundary(
	path: &Path,
	kind: EvidenceEntryKind,
	destination: &str,
) -> io::Result<(PathBuf, PathBuf)> {
	let destination = Path::new(destination);
	let prefix_components = match kind {
		EvidenceEntryKind::SourceInput => Some(2_usize),
		EvidenceEntryKind::CompatchInput
		| EvidenceEntryKind::MergedOutput
		| EvidenceEntryKind::BaseInput => Some(1_usize),
		_ => None,
	};
	if let Some(prefix_components) = prefix_components {
		let relative = destination
			.components()
			.skip(prefix_components)
			.collect::<PathBuf>();
		if relative.as_os_str().is_empty() || !path.ends_with(&relative) {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				format!(
					"evidence source {} does not match destination {}",
					path.display(),
					destination.display()
				),
			));
		}
		let mut root = path.to_path_buf();
		for _ in relative.components() {
			root.pop();
		}
		if root.as_os_str().is_empty() {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				format!(
					"evidence source has no containment root: {}",
					path.display()
				),
			));
		}
		return Ok((root, relative));
	}

	let root = path.parent().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("evidence source has no root: {}", path.display()),
		)
	})?;
	let relative = path.file_name().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("evidence source has no file name: {}", path.display()),
		)
	})?;
	Ok((root.to_path_buf(), PathBuf::from(relative)))
}

fn prepare_entries(entries: Vec<EvidenceEntryInput>) -> io::Result<Vec<PreparedEntry>> {
	let mut prepared = Vec::with_capacity(entries.len());
	for entry in entries {
		validate_relative_path(&entry.relative_path)?;
		let content = match entry.content {
			EvidenceContent::Bytes(content) => content,
			EvidenceContent::SourcePath(source) => {
				read_stable_evidence_source_file(&source, entry.kind, &entry.relative_path)?
			}
		};
		prepared.push(PreparedEntry {
			record: EvidenceEntryRecord {
				kind: entry.kind,
				relative_path: entry.relative_path,
				hash_algorithm: ArtifactHashAlgorithm::Blake3,
				hash: blake3_hash(&content),
				bytes: content.len() as u64,
			},
			content,
		});
	}
	prepared.sort_by(|left, right| {
		(left.record.kind, left.record.relative_path.as_str())
			.cmp(&(right.record.kind, right.record.relative_path.as_str()))
	});
	let mut paths = BTreeSet::new();
	for entry in &prepared {
		if !paths.insert(entry.record.relative_path.as_str()) {
			return Err(io::Error::new(
				ErrorKind::InvalidInput,
				format!(
					"duplicate evidence destination {}",
					entry.record.relative_path
				),
			));
		}
	}
	Ok(prepared)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFileFingerprint {
	len: u64,
	modified_seconds: Option<u64>,
	modified_nanoseconds: Option<u32>,
	device_id: Option<u64>,
	inode: Option<u64>,
	changed_seconds: Option<i64>,
	changed_nanoseconds: Option<i64>,
}

pub(crate) fn read_stable_source_file(root: &Path, relative: &Path) -> io::Result<Vec<u8>> {
	validate_relative_filesystem_path(relative)?;
	read_stable_file_under_root(&root.join(relative), root, relative)
}

fn read_stable_evidence_source_file(
	path: &Path,
	kind: EvidenceEntryKind,
	destination: &str,
) -> io::Result<Vec<u8>> {
	let (source_root, source_relative) = source_boundary(path, kind, destination)?;
	read_stable_file_under_root(path, &source_root, &source_relative)
}

fn read_stable_file_under_root(
	path: &Path,
	source_root: &Path,
	source_relative: &Path,
) -> io::Result<Vec<u8>> {
	let checked_root = require_directory(source_root, "evidence source root")?;
	let checked_path =
		validate_regular_file_beneath(&checked_root, source_relative, "evidence source")?;
	if absolute_clean_path(path)? != checked_path {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!(
				"evidence source is outside its declared root: {}",
				path.display()
			),
		));
	}
	let path_before = fs::symlink_metadata(path)?;
	if !path_before.file_type().is_file() {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!(
				"evidence source must be one explicit regular file: {}",
				path.display()
			),
		));
	}
	let path_before = source_file_fingerprint(&path_before);
	let mut file = open_regular_file_no_follow(path)?;
	let handle_before = source_file_fingerprint(&file.metadata()?);
	if path_before != handle_before {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence source changed before reading: {}", path.display()),
		));
	}
	let mut content = Vec::with_capacity(usize::try_from(handle_before.len).unwrap_or(0));
	file.read_to_end(&mut content)?;
	let handle_after = source_file_fingerprint(&file.metadata()?);
	let path_after_metadata = fs::symlink_metadata(path)?;
	if !path_after_metadata.file_type().is_file() {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"evidence source was replaced while reading: {}",
				path.display()
			),
		));
	}
	let path_after = source_file_fingerprint(&path_after_metadata);
	if handle_before != handle_after
		|| handle_after != path_after
		|| content.len() as u64 != handle_after.len
	{
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence source changed while reading: {}", path.display()),
		));
	}
	validate_regular_file_beneath(&checked_root, source_relative, "evidence source")?;
	Ok(content)
}

fn source_file_fingerprint(metadata: &Metadata) -> SourceFileFingerprint {
	let (modified_seconds, modified_nanoseconds) = metadata
		.modified()
		.ok()
		.and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
		.map_or((None, None), |duration| {
			(Some(duration.as_secs()), Some(duration.subsec_nanos()))
		});
	let mut fingerprint = SourceFileFingerprint {
		len: metadata.len(),
		modified_seconds,
		modified_nanoseconds,
		device_id: None,
		inode: None,
		changed_seconds: None,
		changed_nanoseconds: None,
	};
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		fingerprint.device_id = Some(metadata.dev());
		fingerprint.inode = Some(metadata.ino());
		fingerprint.changed_seconds = Some(metadata.ctime());
		fingerprint.changed_nanoseconds = Some(metadata.ctime_nsec());
	}
	fingerprint
}

fn bundle_stats(entries: &[PreparedEntry]) -> io::Result<EvidenceBundleStats> {
	let bytes = entries.iter().try_fold(0_u64, |total, entry| {
		total.checked_add(entry.record.bytes).ok_or_else(|| {
			io::Error::new(
				ErrorKind::InvalidInput,
				"evidence bundle byte count overflow",
			)
		})
	})?;
	Ok(EvidenceBundleStats {
		files: entries.len() as u64,
		bytes,
	})
}

fn bundle_hash(
	measurement_id: &str,
	input_version_id: &str,
	stats: EvidenceBundleStats,
	entries: &[EvidenceEntryRecord],
) -> String {
	let payload = serde_json::to_vec(&(
		EVIDENCE_BUNDLE_SCHEMA,
		measurement_id,
		input_version_id,
		stats,
		entries,
	))
	.expect("evidence bundle identity serializes");
	stable_id("evidence-bundle-v1", &[&payload])
}

fn validate_manifest(expected_hash: &str, manifest: &EvidenceBundleManifest) -> io::Result<()> {
	if manifest.schema != EVIDENCE_BUNDLE_SCHEMA || manifest.bundle_hash != expected_hash {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle {expected_hash} has invalid manifest identity"),
		));
	}
	if manifest.entries.is_empty() {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle {expected_hash} is empty"),
		));
	}
	let mut sorted = manifest.entries.clone();
	sorted.sort_by(|left, right| {
		(left.kind, left.relative_path.as_str()).cmp(&(right.kind, right.relative_path.as_str()))
	});
	if sorted != manifest.entries {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle {expected_hash} entries are not canonical"),
		));
	}
	let mut paths = BTreeSet::new();
	let mut bytes = 0_u64;
	for entry in &manifest.entries {
		validate_relative_path(&entry.relative_path).map_err(|error| {
			io::Error::new(
				ErrorKind::InvalidData,
				format!("invalid evidence manifest entry: {error}"),
			)
		})?;
		if entry.hash_algorithm != ArtifactHashAlgorithm::Blake3
			|| !is_blake3_hash(&entry.hash)
			|| !paths.insert(entry.relative_path.as_str())
		{
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!("evidence bundle {expected_hash} has invalid entry metadata"),
			));
		}
		bytes = bytes.checked_add(entry.bytes).ok_or_else(|| {
			io::Error::new(
				ErrorKind::InvalidData,
				"evidence bundle byte count overflow",
			)
		})?;
	}
	let expected_stats = EvidenceBundleStats {
		files: manifest.entries.len() as u64,
		bytes,
	};
	let actual_hash = bundle_hash(
		&manifest.measurement_id,
		&manifest.input_version_id,
		expected_stats,
		&manifest.entries,
	);
	if manifest.stats != expected_stats || actual_hash != expected_hash {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!("evidence bundle {expected_hash} manifest checksum is invalid"),
		));
	}
	Ok(())
}

fn validate_bundle_layout(
	bundle: &CheckedDirectory,
	manifest: &EvidenceBundleManifest,
) -> io::Result<()> {
	let path = &bundle.path;
	require_directory_beneath(bundle, Path::new(FILES_DIR), "evidence files directory")?;
	let mut expected = BTreeSet::from([PathBuf::from(MANIFEST_NAME)]);
	for entry in &manifest.entries {
		expected.insert(Path::new(FILES_DIR).join(&entry.relative_path));
	}
	let mut actual = BTreeSet::new();
	for entry in WalkDir::new(path).min_depth(1).follow_links(false) {
		let entry = entry.map_err(io::Error::other)?;
		if entry.file_type().is_dir() {
			continue;
		}
		if !entry.file_type().is_file() {
			return Err(io::Error::new(
				ErrorKind::InvalidData,
				format!(
					"evidence bundle contains a non-regular entry: {}",
					entry.path().display()
				),
			));
		}
		let relative = entry
			.path()
			.strip_prefix(path)
			.expect("walked evidence entry is below bundle root")
			.to_path_buf();
		validate_regular_file_beneath(bundle, &relative, "evidence bundle file")?;
		actual.insert(relative);
	}
	if actual != expected {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			"evidence bundle file set does not match its manifest",
		));
	}
	Ok(())
}

fn validate_relative_path(relative_path: &str) -> io::Result<()> {
	if relative_path.is_empty() || relative_path.contains('\\') {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("evidence destination is not canonical: {relative_path:?}"),
		));
	}
	let path = Path::new(relative_path);
	if path.is_absolute() {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("evidence destination must be relative: {relative_path:?}"),
		));
	}
	let components: Vec<&str> = path
		.components()
		.map(|component| match component {
			Component::Normal(value) => value.to_str().ok_or_else(|| {
				io::Error::new(
					ErrorKind::InvalidInput,
					"evidence destination must be UTF-8",
				)
			}),
			_ => Err(io::Error::new(
				ErrorKind::InvalidInput,
				format!("evidence destination is not canonical: {relative_path:?}"),
			)),
		})
		.collect::<io::Result<_>>()?;
	if components.join("/") != relative_path {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("evidence destination is not canonical: {relative_path:?}"),
		));
	}
	Ok(())
}

fn validate_hash(hash: &str) -> io::Result<()> {
	if !is_blake3_hash(hash) {
		return Err(io::Error::new(
			ErrorKind::InvalidInput,
			format!("invalid BLAKE3 hash: {hash:?}"),
		));
	}
	Ok(())
}

fn is_blake3_hash(value: &str) -> bool {
	value.len() == 64
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn blake3_hash(content: &[u8]) -> String {
	blake3::hash(content).to_hex().to_string()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn input(entries: Vec<EvidenceEntryInput>) -> EvidenceBundleInput {
		EvidenceBundleInput {
			measurement_id: "measurement".to_string(),
			input_version_id: "input-version".to_string(),
			entries,
		}
	}

	#[test]
	fn stores_only_explicit_files_with_deterministic_checksums() {
		let temp = tempfile::tempdir().unwrap();
		let source = temp.path().join("report.json");
		fs::write(&source, b"{\"status\":\"complete\"}").unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let entries = vec![
			EvidenceEntryInput::source_file(
				EvidenceEntryKind::MergeReport,
				"reports/merge.json",
				&source,
			),
			EvidenceEntryInput::bytes(
				EvidenceEntryKind::FileResult,
				"results/common/a.txt.json",
				b"score".to_vec(),
			),
		];
		let first = store.store(input(entries.clone())).unwrap();
		let second = store
			.store(input(entries.into_iter().rev().collect()))
			.unwrap();

		assert!(first.newly_stored);
		assert!(!second.newly_stored);
		assert_eq!(first.hash, second.hash);
		assert_eq!(first.manifest.schema, EVIDENCE_BUNDLE_SCHEMA);
		assert_eq!(
			first.stats,
			EvidenceBundleStats {
				files: 2,
				bytes: 26
			}
		);
		assert_eq!(first.reference().hash, first.hash);
		assert_eq!(
			first.read_entry("reports/merge.json").unwrap(),
			b"{\"status\":\"complete\"}"
		);
		let manifest_text = fs::read_to_string(first.path.join(MANIFEST_NAME)).unwrap();
		assert!(!manifest_text.contains(source.to_str().unwrap()));
		assert_eq!(store.open(&first.hash).unwrap().manifest, first.manifest);
	}

	#[test]
	fn content_and_binding_changes_change_bundle_hash() {
		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let entry = |content: &[u8]| {
			EvidenceEntryInput::bytes(
				EvidenceEntryKind::MergeReport,
				"report.json",
				content.to_vec(),
			)
		};
		let first = store.store(input(vec![entry(b"first")])).unwrap();
		let second = store.store(input(vec![entry(b"second")])).unwrap();
		let mut rebound = input(vec![entry(b"first")]);
		rebound.input_version_id = "other-input".to_string();
		let rebound = store.store(rebound).unwrap();

		assert_ne!(first.hash, second.hash);
		assert_ne!(first.hash, rebound.hash);
	}

	#[test]
	fn rejects_directories_symlinks_and_unsafe_destinations() {
		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let directory =
			EvidenceEntryInput::source_file(EvidenceEntryKind::MergedOutput, "output", temp.path());
		assert_eq!(
			store.store(input(vec![directory])).unwrap_err().kind(),
			ErrorKind::InvalidInput
		);

		for destination in ["../escape", "/absolute", "not//canonical", "back\\slash"] {
			let entry = EvidenceEntryInput::bytes(
				EvidenceEntryKind::MergeReport,
				destination,
				b"value".to_vec(),
			);
			assert_eq!(
				store.store(input(vec![entry])).unwrap_err().kind(),
				ErrorKind::InvalidInput,
				"{destination}"
			);
		}

		#[cfg(unix)]
		{
			use std::os::unix::fs::symlink;

			let source = temp.path().join("source");
			let source_root = temp.path().join("source-root");
			let link = source_root.join("source.txt");
			fs::write(&source, b"value").unwrap();
			fs::create_dir(&source_root).unwrap();
			symlink(&source, &link).unwrap();
			let entry = EvidenceEntryInput::source_file(
				EvidenceEntryKind::SourceInput,
				"sources/01-source/source.txt",
				link,
			);
			assert_eq!(
				store.store(input(vec![entry])).unwrap_err().kind(),
				ErrorKind::InvalidInput
			);
		}
	}

	#[test]
	fn detects_corrupt_stored_content() {
		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let stored = store
			.store(input(vec![EvidenceEntryInput::bytes(
				EvidenceEntryKind::MergeReport,
				"report.json",
				b"valid".to_vec(),
			)]))
			.unwrap();
		fs::write(stored.path.join("files/unlisted"), b"forged").unwrap();
		assert_eq!(
			store.open(&stored.hash).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
		fs::remove_file(stored.path.join("files/unlisted")).unwrap();
		fs::write(stored.path.join("files/report.json"), b"forged").unwrap();

		assert_eq!(
			store.open(&stored.hash).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
	}

	#[test]
	fn read_entry_rejects_post_open_tampering_and_ambiguous_metadata() {
		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let stored = store
			.store(input(vec![EvidenceEntryInput::bytes(
				EvidenceEntryKind::ScorerConfig,
				"metadata/scorer.json",
				b"valid".to_vec(),
			)]))
			.unwrap();
		assert_eq!(stored.read_entry("metadata/scorer.json").unwrap(), b"valid");
		assert_eq!(
			stored
				.read_entry("metadata/unlisted.json")
				.unwrap_err()
				.kind(),
			ErrorKind::NotFound
		);

		let entry_path = stored.path.join("files/metadata/scorer.json");
		fs::write(&entry_path, b"different-length").unwrap();
		assert_eq!(
			stored
				.read_entry("metadata/scorer.json")
				.unwrap_err()
				.kind(),
			ErrorKind::InvalidData
		);
		fs::write(&entry_path, b"forge").unwrap();
		assert_eq!(
			stored
				.read_entry("metadata/scorer.json")
				.unwrap_err()
				.kind(),
			ErrorKind::InvalidData
		);

		let mut ambiguous = stored.clone();
		let duplicate = ambiguous.manifest.entries[0].clone();
		ambiguous.manifest.entries.push(duplicate);
		assert_eq!(
			ambiguous
				.read_entry("metadata/scorer.json")
				.unwrap_err()
				.kind(),
			ErrorKind::InvalidData
		);
	}

	#[cfg(unix)]
	#[test]
	fn read_entry_rejects_intermediate_directory_symlink() {
		use std::os::unix::fs::symlink;

		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let stored = store
			.store(input(vec![EvidenceEntryInput::bytes(
				EvidenceEntryKind::ScorerEvidenceIndex,
				"metadata/evidence-index.json",
				b"{}".to_vec(),
			)]))
			.unwrap();
		assert_eq!(
			stored.read_entry("metadata/evidence-index.json").unwrap(),
			b"{}"
		);

		let metadata = stored.path.join("files/metadata");
		let outside = temp.path().join("outside-metadata");
		fs::rename(&metadata, &outside).unwrap();
		symlink(&outside, &metadata).unwrap();
		assert_eq!(
			stored
				.read_entry("metadata/evidence-index.json")
				.unwrap_err()
				.kind(),
			ErrorKind::InvalidData
		);
	}

	#[cfg(unix)]
	#[test]
	fn rejects_symlinked_store_work_shard_and_bundle_directories() {
		use std::os::unix::fs::symlink;

		let entry = || {
			input(vec![EvidenceEntryInput::bytes(
				EvidenceEntryKind::ScorerEvidenceIndex,
				"metadata/scorer-evidence-index.json",
				b"{}".to_vec(),
			)])
		};

		let temp = tempfile::tempdir().unwrap();
		let outside_store = temp.path().join("outside-store");
		let work = temp.path().join("work");
		fs::create_dir(&outside_store).unwrap();
		symlink(&outside_store, temp.path().join("store-link")).unwrap();
		let store = EvidenceStore::new(temp.path().join("store-link"), &work);
		assert_eq!(
			store.store(entry()).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
		assert!(fs::read_dir(&outside_store).unwrap().next().is_none());

		let store_root = temp.path().join("store");
		let outside_work = temp.path().join("outside-work");
		fs::create_dir(&store_root).unwrap();
		fs::create_dir(&outside_work).unwrap();
		symlink(&outside_work, temp.path().join("work-link")).unwrap();
		let store = EvidenceStore::new(&store_root, temp.path().join("work-link"));
		assert_eq!(
			store.store(entry()).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
		assert!(fs::read_dir(&outside_work).unwrap().next().is_none());

		let prepared = prepare_entries(entry().entries).unwrap();
		let stats = bundle_stats(&prepared).unwrap();
		let records = prepared
			.iter()
			.map(|entry| entry.record.clone())
			.collect::<Vec<_>>();
		let hash = bundle_hash("measurement", "input-version", stats, &records);
		let shard_target = temp.path().join("outside-shard");
		fs::create_dir(&shard_target).unwrap();
		symlink(&shard_target, store_root.join(&hash[..2])).unwrap();
		let store = EvidenceStore::new(&store_root, &work);
		assert_eq!(
			store.store(entry()).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
		assert!(fs::read_dir(&shard_target).unwrap().next().is_none());

		fs::remove_file(store_root.join(&hash[..2])).unwrap();
		let stored = store.store(entry()).unwrap();
		let displaced = temp.path().join("displaced-bundle");
		fs::rename(&stored.path, &displaced).unwrap();
		symlink(&displaced, &stored.path).unwrap();
		assert_eq!(
			store.open(&stored.hash).unwrap_err().kind(),
			ErrorKind::InvalidData
		);
	}

	#[cfg(unix)]
	#[test]
	fn rejects_source_root_and_intermediate_symlink_escape() {
		use std::os::unix::fs::symlink;

		let temp = tempfile::tempdir().unwrap();
		let outside = temp.path().join("outside");
		let outside_common = outside.join("common");
		fs::create_dir_all(&outside_common).unwrap();
		fs::write(outside_common.join("value.txt"), b"outside").unwrap();

		let source_link = temp.path().join("source-link");
		symlink(&outside, &source_link).unwrap();
		let linked_root = EvidenceEntryInput::source_file(
			EvidenceEntryKind::CompatchInput,
			"compatch/common/value.txt",
			source_link.join("common/value.txt"),
		);
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		assert_eq!(
			store.store(input(vec![linked_root])).unwrap_err().kind(),
			ErrorKind::InvalidData
		);

		let source_root = temp.path().join("source-root");
		fs::create_dir(&source_root).unwrap();
		symlink(&outside_common, source_root.join("common")).unwrap();
		let linked_intermediate = EvidenceEntryInput::source_file(
			EvidenceEntryKind::CompatchInput,
			"compatch/common/value.txt",
			source_root.join("common/value.txt"),
		);
		assert_eq!(
			store
				.store(input(vec![linked_intermediate]))
				.unwrap_err()
				.kind(),
			ErrorKind::InvalidData
		);
	}

	#[cfg(unix)]
	#[test]
	fn open_rejects_symlinked_manifest_before_reading_it() {
		use std::os::unix::fs::symlink;

		let temp = tempfile::tempdir().unwrap();
		let store = EvidenceStore::new(temp.path().join("objects"), temp.path().join("work"));
		let stored = store
			.store(input(vec![EvidenceEntryInput::bytes(
				EvidenceEntryKind::MergeReport,
				"report.json",
				b"valid".to_vec(),
			)]))
			.unwrap();
		let outside_manifest = temp.path().join("outside-manifest.json");
		fs::write(&outside_manifest, b"{}").unwrap();
		fs::remove_file(stored.path.join(MANIFEST_NAME)).unwrap();
		symlink(&outside_manifest, stored.path.join(MANIFEST_NAME)).unwrap();

		assert_eq!(
			store.open(&stored.hash).unwrap_err().kind(),
			ErrorKind::InvalidInput
		);
	}
}
