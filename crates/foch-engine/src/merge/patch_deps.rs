#![allow(dead_code)]

use std::path::PathBuf;

use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::semantic_index::parse_script_file;

use super::patch::{ClausewitzPatch, diff_ast};
use crate::workspace::ResolvedFileContributor;

/// Describes how to compute a single mod's patch for one file: which earlier
/// version of the file serves as the diff base.
#[derive(Clone, Debug)]
pub struct DiffPair {
	pub mod_id: String,
	pub mod_path: PathBuf,
	pub precedence: usize,
	/// Path to the file that serves as diff base for this mod.
	/// `None` means the file is new (no prior version exists).
	pub diff_base_path: Option<PathBuf>,
	/// Which mod (or base game) provides the diff base.
	pub diff_base_id: String,
}

/// Determine the diff base for each mod's contribution to a file.
///
/// The diff base is the "previous state" that this mod is editing:
/// - If no earlier mod touches this file → diff base = base game
/// - If an earlier mod (lower position) also touches this file → diff base =
///   that mod's version
///
/// This produces a chain: base → mod1 → mod2 → mod3 (each diffs against the
/// previous contributor).
///
/// `contributors` must be sorted by precedence (playlist position).
/// The base-game contributor (if any) is the chain anchor and is not itself
/// included in the output.
pub(crate) fn resolve_diff_chain(
	_file_path: &str,
	contributors: &[ResolvedFileContributor],
) -> Vec<DiffPair> {
	let mut pairs = Vec::new();

	// Track the most-recent contributor so far (starts as None — no prior
	// version at all).  When a base-game entry exists it becomes the first
	// "previous" without producing a DiffPair of its own.
	let mut prev: Option<&ResolvedFileContributor> = None;

	for c in contributors {
		if c.is_base_game {
			// Base game is the anchor; it does not produce a patch itself.
			prev = Some(c);
			continue;
		}

		let (diff_base_path, diff_base_id) = match prev {
			Some(p) => (Some(p.absolute_path.clone()), p.mod_id.clone()),
			None => (None, String::new()),
		};

		pairs.push(DiffPair {
			mod_id: c.mod_id.clone(),
			mod_path: c.absolute_path.clone(),
			precedence: c.precedence,
			diff_base_path,
			diff_base_id,
		});

		prev = Some(c);
	}

	pairs
}

/// Compute all patches for a single file across all contributing mods,
/// respecting the dependency chain.
///
/// Returns `Vec<(mod_id, precedence, patches)>` ready for
/// `merge_patch_sets()`.
pub(crate) fn compute_chained_patches(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	merge_key_source: MergeKeySource,
) -> Result<Vec<(String, usize, Vec<ClausewitzPatch>)>, String> {
	let chain = resolve_diff_chain(file_path, contributors);
	let mut result = Vec::with_capacity(chain.len());

	for pair in &chain {
		let mod_parsed = parse_script_file(
			&pair.mod_id,
			// root = parent of the absolute path so that the relative path is
			// just the filename — mirrors how callers elsewhere use
			// `parse_script_file`.
			pair.mod_path
				.parent()
				.ok_or_else(|| format!("invalid mod path: {}", pair.mod_path.display()))?,
			&pair.mod_path,
		)
		.ok_or_else(|| {
			format!(
				"failed to parse mod file {} for {}",
				pair.mod_path.display(),
				pair.mod_id,
			)
		})?;

		let patches = match &pair.diff_base_path {
			Some(base_path) => {
				let base_parsed = parse_script_file(
					&pair.diff_base_id,
					base_path
						.parent()
						.ok_or_else(|| format!("invalid base path: {}", base_path.display()))?,
					base_path,
				)
				.ok_or_else(|| {
					format!(
						"failed to parse base file {} for diff base {}",
						base_path.display(),
						pair.diff_base_id,
					)
				})?;
				diff_ast(&base_parsed, &mod_parsed, merge_key_source)
			}
			// No base exists — everything in this file is new content.
			None => diff_ast_as_inserts(&mod_parsed, merge_key_source),
		};

		result.push((pair.mod_id.clone(), pair.precedence, patches));
	}

	Ok(result)
}

/// When a file has no prior version, treat every top-level assignment as an
/// `InsertNode` patch.
fn diff_ast_as_inserts(
	parsed: &foch_language::analyzer::semantic_index::ParsedScriptFile,
	_merge_key_source: MergeKeySource,
) -> Vec<ClausewitzPatch> {
	use foch_language::analyzer::parser::AstStatement;

	parsed
		.ast
		.statements
		.iter()
		.filter_map(|stmt| match stmt {
			AstStatement::Assignment { key, .. } => Some(ClausewitzPatch::InsertNode {
				path: vec![],
				key: key.clone(),
				statement: stmt.clone(),
			}),
			_ => None,
		})
		.collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	/// Helper: build a `ResolvedFileContributor`.
	fn contributor(
		mod_id: &str,
		path: &str,
		precedence: usize,
		is_base_game: bool,
	) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(path)
				.parent()
				.unwrap_or(&PathBuf::from("/"))
				.to_path_buf(),
			absolute_path: PathBuf::from(path),
			precedence,
			is_base_game,
			is_synthetic_base: false,
			parse_ok_hint: None,
		}
	}

	// -----------------------------------------------------------------------
	// resolve_diff_chain tests
	// -----------------------------------------------------------------------

	#[test]
	fn single_mod_no_base() {
		let contribs = vec![contributor("mod_a", "/mods/a/file.txt", 1, false)];
		let chain = resolve_diff_chain("common/file.txt", &contribs);

		assert_eq!(chain.len(), 1);
		assert_eq!(chain[0].mod_id, "mod_a");
		assert!(chain[0].diff_base_path.is_none(), "no base → None");
		assert_eq!(chain[0].diff_base_id, "");
	}

	#[test]
	fn base_plus_one_mod() {
		let contribs = vec![
			contributor("__game__eu4", "/game/file.txt", 0, true),
			contributor("mod_a", "/mods/a/file.txt", 1, false),
		];
		let chain = resolve_diff_chain("common/file.txt", &contribs);

		assert_eq!(chain.len(), 1);
		assert_eq!(chain[0].mod_id, "mod_a");
		assert_eq!(
			chain[0].diff_base_path.as_deref(),
			Some(PathBuf::from("/game/file.txt").as_path()),
		);
		assert_eq!(chain[0].diff_base_id, "__game__eu4");
	}

	#[test]
	fn base_plus_two_mods() {
		let contribs = vec![
			contributor("__game__eu4", "/game/file.txt", 0, true),
			contributor("mod_a", "/mods/a/file.txt", 1, false),
			contributor("mod_b", "/mods/b/file.txt", 2, false),
		];
		let chain = resolve_diff_chain("common/file.txt", &contribs);

		assert_eq!(chain.len(), 2);

		// mod_a diffs against base game
		assert_eq!(chain[0].mod_id, "mod_a");
		assert_eq!(
			chain[0].diff_base_path.as_deref(),
			Some(PathBuf::from("/game/file.txt").as_path()),
		);
		assert_eq!(chain[0].diff_base_id, "__game__eu4");

		// mod_b diffs against mod_a (not base game!)
		assert_eq!(chain[1].mod_id, "mod_b");
		assert_eq!(
			chain[1].diff_base_path.as_deref(),
			Some(PathBuf::from("/mods/a/file.txt").as_path()),
		);
		assert_eq!(chain[1].diff_base_id, "mod_a");
	}

	#[test]
	fn two_mods_no_base() {
		let contribs = vec![
			contributor("mod_a", "/mods/a/file.txt", 1, false),
			contributor("mod_b", "/mods/b/file.txt", 2, false),
		];
		let chain = resolve_diff_chain("common/file.txt", &contribs);

		assert_eq!(chain.len(), 2);

		// mod_a: no prior contributor → diff_base_path = None
		assert_eq!(chain[0].mod_id, "mod_a");
		assert!(chain[0].diff_base_path.is_none());

		// mod_b: diffs against mod_a
		assert_eq!(chain[1].mod_id, "mod_b");
		assert_eq!(
			chain[1].diff_base_path.as_deref(),
			Some(PathBuf::from("/mods/a/file.txt").as_path()),
		);
		assert_eq!(chain[1].diff_base_id, "mod_a");
	}

	#[test]
	fn base_plus_three_mods_with_gap() {
		// mod_b does NOT touch this file.  The contributors list only
		// contains mods that *do* touch the file, so mod_b is absent.
		let contribs = vec![
			contributor("__game__eu4", "/game/file.txt", 0, true),
			contributor("mod_a", "/mods/a/file.txt", 1, false),
			// mod_b (precedence 2) is absent — it doesn't touch this file
			contributor("mod_c", "/mods/c/file.txt", 3, false),
		];
		let chain = resolve_diff_chain("common/file.txt", &contribs);

		assert_eq!(chain.len(), 2);

		// mod_a diffs against base game
		assert_eq!(chain[0].mod_id, "mod_a");
		assert_eq!(chain[0].diff_base_id, "__game__eu4");

		// mod_c diffs against mod_a (skipping the absent mod_b)
		assert_eq!(chain[1].mod_id, "mod_c");
		assert_eq!(
			chain[1].diff_base_path.as_deref(),
			Some(PathBuf::from("/mods/a/file.txt").as_path()),
		);
		assert_eq!(chain[1].diff_base_id, "mod_a");
	}
}
