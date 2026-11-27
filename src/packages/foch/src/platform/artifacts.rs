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
	let cwd = std::env::current_dir().ok();
	resolve_shared_artifact_root(
		std::env::var_os(ARTIFACT_DIR_ENV).as_deref(),
		std::env::var_os("CARGO_TARGET_DIR").as_deref(),
		&repository_root(),
		dirs::data_local_dir(),
		std::env::temp_dir(),
		cwd.as_deref(),
	)
}

/// Resolve the shared artifact root without reading process environment.
///
/// An explicit artifact directory wins, including a path inside a checkout.
/// `CARGO_TARGET_DIR` is reused only when its absolute path is outside
/// `repo_root`, so Cargo's automatic per-worktree target directory does not
/// become another copy of probe data or merge output. A relative target
/// directory is resolved against `cwd` and lexically normalized before that
/// check; without a usable `cwd` it is not reused. The reused path is always
/// absolute. Otherwise the platform data directory (or the system temp
/// directory) is used.
pub fn resolve_shared_artifact_root(
	artifact_dir: Option<&OsStr>,
	cargo_target_dir: Option<&OsStr>,
	repo_root: &Path,
	data_local_dir: Option<PathBuf>,
	temp_dir: PathBuf,
	cwd: Option<&Path>,
) -> PathBuf {
	if let Some(dir) = artifact_dir.filter(|dir| !dir.is_empty()) {
		return PathBuf::from(dir);
	}
	let cargo_target = cargo_target_dir
		.filter(|dir| !dir.is_empty())
		.and_then(|dir| absolute_cargo_target(Path::new(dir), cwd))
		.filter(|path| !is_within(repo_root, path));
	if let Some(path) = cargo_target {
		return path.join("foch-artifacts");
	}
	data_local_dir
		.unwrap_or(temp_dir)
		.join("foch")
		.join("artifacts")
}

/// Absolutize a Cargo target directory before the in-checkout check.
///
/// Relative values resolve against `cwd` and are lexically normalized. A
/// relative value is rejected when `cwd` is missing or the join is not absolute,
/// so a relative path cannot later resolve inside a package directory.
fn absolute_cargo_target(path: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
	let joined = if path.is_absolute() {
		path.to_path_buf()
	} else {
		cwd?.join(path)
	};
	let normalized = normalize_lexically(&joined);
	normalized.is_absolute().then_some(normalized)
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
		let repo = abs(&["checkout"]);
		let resolved = resolve_shared_artifact_root(
			None,
			Some(repo.join("target").as_os_str()),
			&repo,
			Some(abs(&["data"])),
			abs(&["tmp"]),
			None,
		);
		assert_eq!(resolved, abs(&["data"]).join("foch").join("artifacts"));
		assert!(!is_within(&repo, &resolved));
		assert!(!is_within(&repo.join("target"), &resolved));
	}

	#[test]
	fn cargo_target_outside_the_checkout_is_reused() {
		let external = abs(&["shared-target"]);
		let repo = abs(&["checkout"]);
		let resolved = resolve_shared_artifact_root(
			None,
			Some(external.as_os_str()),
			&repo,
			Some(abs(&["data"])),
			abs(&["tmp"]),
			None,
		);
		assert_eq!(resolved, external.join("foch-artifacts"));
		assert!(resolved.is_absolute());
		assert!(!is_within(&repo, &resolved));
	}

	#[test]
	fn relative_cargo_target_inside_the_checkout_falls_back() {
		let repo = abs(&["work", "checkout"]);
		let cwd = abs(&["work", "checkout", "src", "packages"]);
		let fallback = abs(&["data"]).join("foch").join("artifacts");
		for cargo_target in ["target", "./target", "../../target"] {
			let resolved = resolve_shared_artifact_root(
				None,
				Some(OsStr::new(cargo_target)),
				&repo,
				Some(abs(&["data"])),
				abs(&["tmp"]),
				Some(&cwd),
			);
			assert_eq!(resolved, fallback, "{cargo_target}");
			assert!(resolved.is_absolute(), "{cargo_target}");
			assert!(!is_within(&repo, &resolved), "{cargo_target}");
		}
	}

	#[test]
	fn cargo_target_resolving_outside_the_checkout_is_absolute() {
		let repo = abs(&["work", "checkout"]);
		let cwd = repo.clone();
		let external = abs(&["shared", "target"]);
		let from_absolute = resolve_shared_artifact_root(
			None,
			Some(external.as_os_str()),
			&repo,
			Some(abs(&["data"])),
			abs(&["tmp"]),
			Some(&cwd),
		);
		assert_eq!(from_absolute, external.join("foch-artifacts"));
		assert!(from_absolute.is_absolute());
		assert!(!is_within(&repo, &from_absolute));

		let from_relative = resolve_shared_artifact_root(
			None,
			Some(OsStr::new("../external-shared-target")),
			&repo,
			Some(abs(&["data"])),
			abs(&["tmp"]),
			Some(&cwd),
		);
		let expected = abs(&["work", "external-shared-target"]).join("foch-artifacts");
		assert_eq!(from_relative, expected);
		assert!(from_relative.is_absolute());
		assert!(!is_within(&repo, &from_relative));
	}

	#[test]
	fn relative_cargo_target_without_a_cwd_is_not_reused() {
		let resolved = resolve_shared_artifact_root(
			None,
			Some(OsStr::new("target")),
			&abs(&["work", "checkout"]),
			Some(abs(&["data"])),
			abs(&["tmp"]),
			None,
		);
		assert_eq!(resolved, abs(&["data"]).join("foch").join("artifacts"));
		assert!(resolved.is_absolute());
	}

	#[test]
	fn artifact_dir_override_wins_even_inside_the_checkout() {
		let resolved = resolve_shared_artifact_root(
			Some(OsStr::new("checkout/custom")),
			Some(OsStr::new("shared-target")),
			Path::new("checkout"),
			None,
			PathBuf::from("tmp"),
			None,
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
			None,
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
			None,
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

	fn abs(parts: &[&str]) -> PathBuf {
		let mut path = if cfg!(windows) {
			PathBuf::from("C:\\")
		} else {
			PathBuf::from("/")
		};
		for part in parts {
			path.push(part);
		}
		path
	}
}
