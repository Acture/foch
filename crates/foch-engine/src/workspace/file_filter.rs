use foch::game::eu4::Eu4;
use globset::{GlobSet, GlobSetBuilder};
use std::path::Path;

/// Filter applied while walking mod roots and the base game install. Combines
/// the game's authoritative content-root list (`Eu4::is_loadable_content_path`)
/// with user-configured extra ignore globs from [`crate::Config`].
///
/// Globs are matched (case-insensitive) against the slash-normalized relative
/// path of each discovered file. The compiled [`GlobSet`] is built once and
/// reused for every walk to avoid repeated regex compilation.
#[derive(Clone, Debug)]
pub struct FileFilter {
	game: Eu4,
	extra_ignore: GlobSet,
	extra_ignore_pattern_count: usize,
}

impl FileFilter {
	/// Build a filter for `game` with `extra_patterns` glob strings.
	///
	/// Returns `Err` containing the offending pattern and `globset` message if
	/// any pattern fails to compile.
	pub fn new(game: Eu4, extra_patterns: &[String]) -> Result<Self, String> {
		let mut builder = GlobSetBuilder::new();
		for pattern in extra_patterns {
			let glob = globset::GlobBuilder::new(pattern)
				.case_insensitive(true)
				.literal_separator(false)
				.build()
				.map_err(|err| {
					format!("failed to parse extra_ignore_patterns pattern \"{pattern}\": {err}")
				})?;
			builder.add(glob);
		}
		let extra_ignore = builder
			.build()
			.map_err(|err| format!("failed to build extra_ignore_patterns GlobSet: {err}"))?;
		Ok(Self {
			game,
			extra_ignore,
			extra_ignore_pattern_count: extra_patterns.len(),
		})
	}

	/// Filter that retains every path the game would load and applies no extra
	/// ignore patterns. Useful in tests and contexts that don't have a
	/// [`crate::Config`] handy.
	pub fn for_game(game: Eu4) -> Self {
		Self {
			game,
			extra_ignore: GlobSet::empty(),
			extra_ignore_pattern_count: 0,
		}
	}

	pub fn game(&self) -> &Eu4 {
		&self.game
	}

	/// Returns `true` when the file at `relative` should be retained.
	pub fn accepts(&self, relative: &Path) -> bool {
		if !self.game.is_loadable_content_path(relative) {
			return false;
		}
		if self.extra_ignore_pattern_count == 0 {
			return true;
		}
		let normalized = normalize_for_match(relative);
		!self.extra_ignore.is_match(&normalized)
	}
}

fn normalize_for_match(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	fn pf(p: &str) -> PathBuf {
		PathBuf::from(p)
	}

	#[test]
	fn extra_pattern_bak_matches_top_and_nested() {
		let filter = FileFilter::new(Eu4, &["*.bak".to_string()]).unwrap();
		assert!(!filter.accepts(&pf("common/foo.bak")));
		assert!(!filter.accepts(&pf("common/dir/foo.bak")));
		assert!(filter.accepts(&pf("common/foo.txt")));
	}

	#[test]
	fn extra_pattern_dsstore_matches_top_and_nested() {
		let filter = FileFilter::new(Eu4, &["**/.DS_Store".to_string()]).unwrap();
		assert!(!filter.accepts(&pf("common/.DS_Store")));
		assert!(!filter.accepts(&pf("common/nested/.DS_Store")));
	}

	#[test]
	fn extra_pattern_match_is_case_insensitive() {
		let filter = FileFilter::new(Eu4, &["*.BAK".to_string()]).unwrap();
		assert!(!filter.accepts(&pf("common/Foo.bak")));
		assert!(!filter.accepts(&pf("common/foo.BAK")));
	}

	#[test]
	fn rejects_invalid_glob() {
		let err = FileFilter::new(Eu4, &["[".to_string()]).unwrap_err();
		assert!(err.contains("extra_ignore_patterns"));
	}

	#[test]
	fn defers_to_game_root_filter_when_no_extra_patterns() {
		let filter = FileFilter::for_game(Eu4);
		assert!(filter.accepts(&pf("common/countries/X.txt")));
		assert!(!filter.accepts(&pf("README.md")));
		assert!(!filter.accepts(&pf(".git/HEAD")));
	}

	#[test]
	fn collect_relative_files_drops_filtered_paths() {
		use crate::workspace::resolve::collect_relative_files;
		use std::fs;
		let dir = tempfile::tempdir().expect("tempdir");
		let root = dir.path();
		fs::create_dir_all(root.join("common/countries")).unwrap();
		fs::create_dir_all(root.join(".git")).unwrap();
		fs::create_dir_all(root.join("nested")).unwrap();
		fs::write(root.join("common/countries/X.txt"), "x").unwrap();
		fs::write(root.join("common/countries/X.bak"), "x").unwrap();
		fs::write(root.join("README.md"), "r").unwrap();
		fs::write(root.join(".git/HEAD"), "g").unwrap();
		fs::write(root.join("nested/.DS_Store"), "d").unwrap();
		fs::write(root.join("descriptor.mod"), "name=\"x\"").unwrap();

		let filter =
			FileFilter::new(Eu4, &["*.bak".to_string(), "**/.DS_Store".to_string()]).unwrap();
		let files = collect_relative_files(root, &filter).expect("collect relative files");
		let strs: Vec<String> = files
			.iter()
			.map(|p| p.to_string_lossy().replace('\\', "/"))
			.collect();
		assert_eq!(strs, vec!["common/countries/X.txt".to_string()]);
	}

	#[cfg(unix)]
	#[test]
	fn collect_relative_files_ignores_file_and_directory_symlinks() {
		use crate::workspace::resolve::collect_relative_files;
		use std::fs;
		use std::os::unix::fs::symlink;

		let dir = tempfile::tempdir().expect("tempdir");
		let root = dir.path().join("mod");
		let outside = dir.path().join("outside");
		fs::create_dir_all(root.join("common/countries")).expect("create mod files");
		fs::create_dir_all(outside.join("events")).expect("create outside files");
		let regular = root.join("common/countries/regular.txt");
		fs::write(&regular, "regular = yes\n").expect("write regular file");
		fs::write(outside.join("events/leak.txt"), "leak = yes\n").expect("write outside file");
		symlink(&regular, root.join("common/countries/linked.txt")).expect("create file symlink");
		symlink(outside.join("events"), root.join("events")).expect("create directory symlink");

		let files = collect_relative_files(&root, &FileFilter::for_game(Eu4))
			.expect("collect relative files");
		assert_eq!(files, vec![PathBuf::from("common/countries/regular.txt")]);
	}
}
