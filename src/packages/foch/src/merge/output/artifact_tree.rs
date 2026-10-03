use crate::merge::error::MergeError;
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
	pub(crate) fn create() -> Result<TempDir, MergeError> {
		tempfile::Builder::new()
			.prefix("foch-merge-analysis-")
			.tempdir()
			.map_err(MergeError::Io)
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

	pub(crate) fn copy_into(&self, destination: &Path) -> Result<(), MergeError> {
		let (observed, _) = digest_tree(self.root.path())?;
		if observed != self.digest {
			return Err(MergeError::AnalyzedArtifactChanged);
		}

		for entry in sorted_entries(self.root.path())? {
			let relative = safe_relative_path(self.root.path(), entry.path())?;
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

		let (copied, _) = digest_tree(destination)?;
		if copied != self.digest {
			return Err(MergeError::AnalyzedArtifactChanged);
		}
		Ok(())
	}
}

fn digest_tree(root: &Path) -> Result<(blake3::Hash, usize), MergeError> {
	let mut hasher = blake3::Hasher::new();
	let mut file_count = 0usize;
	for entry in sorted_entries(root)? {
		let relative = safe_relative_path(root, entry.path())?;
		let relative = relative.to_str().ok_or_else(|| MergeError::Validation {
			path: Some(relative.display().to_string()),
			message: "analyzed artifact path is not valid UTF-8".to_string(),
		})?;
		if entry.file_type().is_dir() {
			hash_part(&mut hasher, b"directory");
			hash_part(&mut hasher, relative.as_bytes());
		} else if entry.file_type().is_file() {
			hash_part(&mut hasher, b"file");
			hash_part(&mut hasher, relative.as_bytes());
			hash_part(&mut hasher, &fs::read(entry.path())?);
			file_count = file_count.saturating_add(1);
		} else {
			return Err(unsupported_entry(entry.path()));
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
						path: Some(root.display().to_string()),
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
			path: Some(path.display().to_string()),
			message: "analyzed artifact escaped its root".to_string(),
		})?;
	if relative.as_os_str().is_empty()
		|| relative
			.components()
			.any(|component| !matches!(component, Component::Normal(_)))
	{
		return Err(MergeError::Validation {
			path: Some(relative.display().to_string()),
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
		path: Some(path.display().to_string()),
		message: "analyzed artifact contains a symlink or special file".to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::AnalyzedArtifactTree;
	use std::fs;

	#[test]
	fn frozen_tree_copies_exact_files() {
		let root = AnalyzedArtifactTree::create().expect("analysis root");
		fs::create_dir_all(root.path().join("nested")).expect("nested directory");
		fs::write(root.path().join("nested/value.txt"), b"frozen").expect("artifact");
		let artifacts = AnalyzedArtifactTree::freeze(root).expect("freeze artifacts");
		let destination = tempfile::TempDir::new().expect("destination");

		artifacts
			.copy_into(destination.path())
			.expect("copy artifacts");

		assert_eq!(artifacts.file_count(), 1);
		assert_eq!(
			fs::read(destination.path().join("nested/value.txt")).expect("copied artifact"),
			b"frozen"
		);
	}
}
