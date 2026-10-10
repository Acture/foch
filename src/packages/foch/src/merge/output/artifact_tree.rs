use crate::merge::error::{MergeError, MergeErrorSubject};
use rayon::prelude::*;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use tempfile::TempDir;
use walkdir::WalkDir;

/// Exact output bytes produced by analysis and consumed by commit.
pub(crate) struct AnalyzedArtifactTree {
	root: TempDir,
	digest: blake3::Hash,
	file_count: usize,
}

impl AnalyzedArtifactTree {
	/// A root beside `final_dir` when its parent exists, so commit can move
	/// the frozen files into the staging directory created there instead of
	/// copying them.
	pub(crate) fn create_beside(final_dir: &Path) -> Result<TempDir, MergeError> {
		let mut builder = tempfile::Builder::new();
		builder.prefix(".foch-merge-analysis-");
		let parent = final_dir
			.parent()
			.filter(|parent| !parent.as_os_str().is_empty())
			.unwrap_or(Path::new("."));
		if parent.is_dir()
			&& let Ok(root) = builder.tempdir_in(parent)
		{
			return Ok(root);
		}
		builder.tempdir().map_err(MergeError::Io)
	}

	pub(crate) fn freeze(root: TempDir) -> Result<Self, MergeError> {
		let (digest, file_count) = digest_tree(root.path())?;
		Ok(Self {
			root,
			digest,
			file_count,
		})
	}

	pub(crate) fn file_count(&self) -> usize {
		self.file_count
	}

	/// Move the frozen files into the empty `destination`, copying an entry
	/// only when it cannot be renamed there, as across volumes. The tree must
	/// still hold the analyzed bytes, and so must `destination` afterwards.
	pub(crate) fn move_into(self, destination: &Path) -> Result<(), MergeError> {
		let (observed, _) = digest_tree(self.root.path())?;
		if observed != self.digest {
			return Err(MergeError::AnalyzedArtifactChanged);
		}
		let mut top_level = fs::read_dir(self.root.path())?.collect::<Result<Vec<_>, _>>()?;
		top_level.sort_by_key(fs::DirEntry::file_name);
		for entry in top_level {
			let target = destination.join(entry.file_name());
			if fs::rename(entry.path(), &target).is_err() {
				copy_tree_entry(self.root.path(), &entry.path(), destination)?;
			}
		}
		let (moved, _) = digest_tree(destination)?;
		if moved != self.digest {
			return Err(MergeError::AnalyzedArtifactChanged);
		}
		Ok(())
	}
}

/// Copy `path`, an entry of the tree at `root`, and everything below it to
/// the same relative place under `destination`.
fn copy_tree_entry(root: &Path, path: &Path, destination: &Path) -> Result<(), MergeError> {
	let mut entries = vec![];
	for entry in WalkDir::new(path).follow_links(false) {
		entries.push(entry.map_err(|error| MergeError::Io(io::Error::other(error.to_string())))?);
	}
	entries.sort_by(|left, right| left.path().cmp(right.path()));
	for entry in entries {
		let relative = safe_relative_path(root, entry.path())?;
		let target = destination.join(&relative);
		if entry.file_type().is_dir() {
			fs::create_dir_all(target)?;
		} else if entry.file_type().is_file() {
			if let Some(parent) = target.parent() {
				fs::create_dir_all(parent)?;
			}
			fs::copy(entry.path(), target)?;
		} else {
			return Err(unsupported_entry(entry.path()));
		}
	}
	Ok(())
}

/// One digest of every path and file in the tree. Files are hashed in
/// parallel and their hashes combined in path order; the digest is only ever
/// compared with another taken in this process.
fn digest_tree(root: &Path) -> Result<(blake3::Hash, usize), MergeError> {
	let parts = sorted_entries(root)?
		.par_iter()
		.map(|entry| {
			let relative = safe_relative_path(root, entry.path())?;
			let relative = relative
				.to_str()
				.ok_or_else(|| MergeError::Validation {
					subject: Some(MergeErrorSubject::Host(relative.to_path_buf())),
					message: "analyzed artifact path is not valid UTF-8".to_string(),
				})?
				.to_owned();
			if entry.file_type().is_dir() {
				Ok((relative, None))
			} else if entry.file_type().is_file() {
				Ok((relative, Some(blake3::hash(&fs::read(entry.path())?))))
			} else {
				Err(unsupported_entry(entry.path()))
			}
		})
		.collect::<Result<Vec<_>, MergeError>>()?;
	let mut hasher = blake3::Hasher::new();
	let mut file_count = 0usize;
	for (relative, file_hash) in parts {
		match file_hash {
			None => {
				hash_part(&mut hasher, b"directory");
				hash_part(&mut hasher, relative.as_bytes());
			}
			Some(file_hash) => {
				hash_part(&mut hasher, b"file");
				hash_part(&mut hasher, relative.as_bytes());
				hash_part(&mut hasher, file_hash.as_bytes());
				file_count = file_count.saturating_add(1);
			}
		}
	}
	Ok((hasher.finalize(), file_count))
}

fn sorted_entries(root: &Path) -> Result<Vec<walkdir::DirEntry>, MergeError> {
	let mut entries = WalkDir::new(root)
		.min_depth(1)
		.follow_links(false)
		.into_iter()
		.map(|entry| {
			entry.map_err(|error| {
				let message = error.to_string();
				match error.into_io_error() {
					Some(error) => MergeError::Io(io::Error::new(error.kind(), message)),
					None => MergeError::Validation {
						subject: Some(MergeErrorSubject::Host(root.to_path_buf())),
						message,
					},
				}
			})
		})
		.collect::<Result<Vec<_>, _>>()?;
	entries.sort_by(|left, right| left.path().cmp(right.path()));
	Ok(entries)
}

fn safe_relative_path(root: &Path, path: &Path) -> Result<PathBuf, MergeError> {
	let relative = path
		.strip_prefix(root)
		.map_err(|_| MergeError::Validation {
			subject: Some(MergeErrorSubject::Host(path.to_path_buf())),
			message: "analyzed artifact escaped its root".to_string(),
		})?;
	if relative.as_os_str().is_empty()
		|| relative
			.components()
			.any(|component| !matches!(component, Component::Normal(_)))
	{
		return Err(MergeError::Validation {
			subject: Some(MergeErrorSubject::Host(relative.to_path_buf())),
			message: "analyzed artifact path is not a safe relative path".to_string(),
		});
	}
	Ok(relative.to_path_buf())
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
	hasher.update(&(bytes.len() as u64).to_le_bytes());
	hasher.update(bytes);
}

fn unsupported_entry(path: &Path) -> MergeError {
	MergeError::Validation {
		subject: Some(MergeErrorSubject::Host(path.to_path_buf())),
		message: "analyzed artifact contains a symlink or special file".to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::AnalyzedArtifactTree;
	use std::fs;

	fn frozen_tree(final_dir: &std::path::Path) -> AnalyzedArtifactTree {
		let root = AnalyzedArtifactTree::create_beside(final_dir).expect("analysis root");
		fs::create_dir_all(root.path().join("nested/deeper")).expect("nested directory");
		fs::write(root.path().join("nested/value.txt"), b"frozen").expect("artifact");
		fs::write(root.path().join("nested/deeper/other.txt"), b"other").expect("artifact");
		fs::write(root.path().join("top.txt"), b"top").expect("artifact");
		AnalyzedArtifactTree::freeze(root).expect("freeze artifacts")
	}

	#[test]
	fn frozen_tree_moves_exact_files_beside_its_output() {
		let scratch = tempfile::TempDir::new().expect("scratch");
		let artifacts = frozen_tree(&scratch.path().join("out"));
		assert_eq!(artifacts.root.path().parent(), Some(scratch.path()));
		assert_eq!(artifacts.file_count(), 3);
		let destination = scratch.path().join("staging");
		fs::create_dir(&destination).expect("destination");

		artifacts.move_into(&destination).expect("move artifacts");

		assert_eq!(
			fs::read(destination.join("nested/value.txt")).expect("moved artifact"),
			b"frozen"
		);
		assert_eq!(
			fs::read(destination.join("nested/deeper/other.txt")).expect("moved artifact"),
			b"other"
		);
		assert_eq!(
			fs::read(destination.join("top.txt")).expect("moved artifact"),
			b"top"
		);
		let leftovers = fs::read_dir(scratch.path())
			.expect("scratch listing")
			.map(|entry| entry.expect("entry").file_name())
			.collect::<Vec<_>>();
		assert_eq!(leftovers, vec![std::ffi::OsString::from("staging")]);
	}

	#[test]
	fn an_entry_that_cannot_be_renamed_is_copied() {
		let scratch = tempfile::TempDir::new().expect("scratch");
		let artifacts = frozen_tree(&scratch.path().join("out"));
		let destination = scratch.path().join("staging");
		fs::create_dir_all(destination.join("nested")).expect("occupied target");
		let root = artifacts.root.path().to_path_buf();

		super::copy_tree_entry(&root, &root.join("nested"), &destination).expect("copy entry");

		assert_eq!(
			fs::read(destination.join("nested/deeper/other.txt")).expect("copied artifact"),
			b"other"
		);
	}

	#[test]
	fn a_changed_frozen_file_is_refused() {
		let scratch = tempfile::TempDir::new().expect("scratch");
		let artifacts = frozen_tree(&scratch.path().join("out"));
		fs::write(artifacts.root.path().join("top.txt"), b"changed").expect("tamper");
		let destination = scratch.path().join("staging");
		fs::create_dir(&destination).expect("destination");

		assert!(matches!(
			artifacts.move_into(&destination),
			Err(crate::merge::error::MergeError::AnalyzedArtifactChanged)
		));
	}
}
