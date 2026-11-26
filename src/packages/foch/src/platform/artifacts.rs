//! Shared root for large probe and manual-merge artifacts.
//!
//! Worktree copies of `target/manual-merges` and `target/probe` were tens of
//! GiB each. Defaults now land in one reusable directory outside the checkout.
//! `FOCH_ARTIFACT_DIR` still overrides that root. Existing per-job variables
//! (`FOCH_WORKSHOP_PROBE_DIR`, `FOCH_COMBO_PROBE_DIR`, `FOCH_DATA_DIR`,
//! `FOCH_CACHE_ROOT`, `FOCH_CONFIG_DIR`) still override their own paths.
//! `FOCH_KEEP_ARTIFACTS=1` retains one-shot diagnostic directories that
//! otherwise disappear with their [`tempfile::TempDir`].

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

pub const ARTIFACT_DIR_ENV: &str = "FOCH_ARTIFACT_DIR";
pub const KEEP_ARTIFACTS_ENV: &str = "FOCH_KEEP_ARTIFACTS";
pub const MANUAL_MERGES_DIR: &str = "manual-merges";
pub const PROBE_DATA_DIR: &str = "probe";

pub fn shared_artifact_root() -> PathBuf {
	resolve_shared_artifact_root(
		std::env::var_os(ARTIFACT_DIR_ENV).as_deref(),
		std::env::var_os("CARGO_TARGET_DIR").as_deref(),
		&repository_root(),
		dirs::data_local_dir(),
		std::env::temp_dir(),
	)
}

/// Resolve the shared artifact root without reading process environment.
///
/// An explicit artifact directory wins, including a path inside a checkout.
/// `CARGO_TARGET_DIR` is reused only when it is outside `repo_root`, so Cargo's
/// automatic per-worktree target directory does not become another copy of
/// probe data or merge output. Otherwise the platform data directory (or the
/// system temp directory) is used.
pub fn resolve_shared_artifact_root(
	artifact_dir: Option<&OsStr>,
	cargo_target_dir: Option<&OsStr>,
	repo_root: &Path,
	data_local_dir: Option<PathBuf>,
	temp_dir: PathBuf,
) -> PathBuf {
	if let Some(dir) = artifact_dir.filter(|dir| !dir.is_empty()) {
		return PathBuf::from(dir);
	}
	if let Some(dir) = cargo_target_dir.filter(|dir| !dir.is_empty()) {
		let path = PathBuf::from(dir);
		if !is_within(repo_root, &path) {
			return path.join("foch-artifacts");
		}
	}
	data_local_dir
		.unwrap_or(temp_dir)
		.join("foch")
		.join("artifacts")
}

pub fn manual_merges_root() -> PathBuf {
	shared_artifact_root().join(MANUAL_MERGES_DIR)
}

pub fn probe_data_root() -> PathBuf {
	shared_artifact_root().join(PROBE_DATA_DIR)
}

pub fn keep_artifacts_requested() -> bool {
	keep_artifacts_requested_from(std::env::var(KEEP_ARTIFACTS_ENV).ok().as_deref())
}

pub fn keep_artifacts_requested_from(value: Option<&str>) -> bool {
	let normalized = value.map(|text| text.trim().to_ascii_lowercase());
	matches!(
		normalized.as_deref(),
		Some("1") | Some("true") | Some("yes")
	)
}

pub fn repository_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

pub fn normalize_lexically(path: &Path) -> PathBuf {
	let mut out = PathBuf::new();
	for component in path.components() {
		match component {
			Component::CurDir => {}
			Component::ParentDir => {
				out.pop();
			}
			other => out.push(other.as_os_str()),
		}
	}
	out
}

pub fn is_within(root: &Path, path: &Path) -> bool {
	let root = normalize_lexically(root);
	let path = normalize_lexically(path);
	path.starts_with(&root)
}

#[cfg(test)]
mod tests {
	use std::ffi::OsStr;
	use std::path::{Path, PathBuf};

	use super::{
		is_within, keep_artifacts_requested_from, normalize_lexically, resolve_shared_artifact_root,
	};

	#[test]
	fn an_in_checkout_cargo_target_is_not_reused() {
		let repo = Path::new("checkout");
		let resolved = resolve_shared_artifact_root(
			None,
			Some(OsStr::new("checkout/target")),
			repo,
			Some(PathBuf::from("data")),
			PathBuf::from("tmp"),
		);
		assert_eq!(
			resolved,
			PathBuf::from("data").join("foch").join("artifacts")
		);
		assert!(!is_within(repo, &resolved));
		assert!(!is_within(Path::new("checkout/target"), &resolved));
	}

	#[test]
	fn cargo_target_outside_the_checkout_is_reused() {
		let resolved = resolve_shared_artifact_root(
			None,
			Some(OsStr::new("shared-target")),
			Path::new("checkout"),
			Some(PathBuf::from("data")),
			PathBuf::from("tmp"),
		);
		assert_eq!(
			resolved,
			PathBuf::from("shared-target").join("foch-artifacts")
		);
	}

	#[test]
	fn artifact_dir_override_wins_even_inside_the_checkout() {
		let resolved = resolve_shared_artifact_root(
			Some(OsStr::new("checkout/custom")),
			Some(OsStr::new("shared-target")),
			Path::new("checkout"),
			None,
			PathBuf::from("tmp"),
		);
		assert_eq!(resolved, PathBuf::from("checkout/custom"));
	}

	#[test]
	fn empty_overrides_fall_through_to_the_data_or_temp_dir() {
		let from_data = resolve_shared_artifact_root(
			Some(OsStr::new("")),
			Some(OsStr::new("")),
			Path::new("checkout"),
			Some(PathBuf::from("data")),
			PathBuf::from("tmp"),
		);
		assert_eq!(
			from_data,
			PathBuf::from("data").join("foch").join("artifacts")
		);
		let from_temp = resolve_shared_artifact_root(
			None,
			Some(OsStr::new("checkout/target")),
			Path::new("checkout"),
			None,
			PathBuf::from("tmp"),
		);
		assert_eq!(
			from_temp,
			PathBuf::from("tmp").join("foch").join("artifacts")
		);
	}

	#[test]
	fn parent_components_still_count_as_inside_the_checkout() {
		let repo = Path::new("checkout/src/packages/foch/../../..");
		let target = Path::new("checkout/target/manual-merges");
		assert!(is_within(repo, target));
		assert_eq!(normalize_lexically(repo), PathBuf::from("checkout"));
	}

	#[test]
	fn keep_switch_accepts_only_explicit_truthy_values() {
		assert!(keep_artifacts_requested_from(Some("1")));
		assert!(keep_artifacts_requested_from(Some(" true ")));
		assert!(keep_artifacts_requested_from(Some("YES")));
		assert!(!keep_artifacts_requested_from(None));
		assert!(!keep_artifacts_requested_from(Some("")));
		assert!(!keep_artifacts_requested_from(Some("0")));
		assert!(!keep_artifacts_requested_from(Some("false")));
	}
}
