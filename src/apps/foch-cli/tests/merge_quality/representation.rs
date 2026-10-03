//! The one place that decides what "the same content" means for scoring.
//!
//! foch writes a schema-typed number in EU4's own representation — a `float`
//! field with three decimals, an `int` field as the integer `atoi` reads —
//! because the game's reader cannot tell `0.5` from `0.50`. A human
//! compatibility patch writes whatever its author typed. Comparing the two
//! without bringing both to that representation counts the tool's own output
//! as a divergence.
//!
//! Everything this harness compares goes through here. That is not tidiness:
//! the canonicalization was first added at each comparison site separately,
//! and two of the four were missed — the layered module view, which
//! `tiny_product_cli_to_pure_scorer_seam` caught by regressing to
//! `diverges_ast`, and the text similarity behind `matches_human`. A fifth
//! comparison added later would have been missed the same way.
//!
//! Two entry points, because there are two kinds of comparison and they need
//! different treatment:
//!
//! * [`parse`] and [`compose`] for content, which may reformat freely.
//! * [`read_text`] for similarity, which measures formatting and therefore
//!   must not.

use std::fs;
use std::path::{Path, PathBuf};

use foch::game::eu4::script::parser::{AstFile, parse_clausewitz_content};
use foch::merge::numeric::{
	canonicalize_numeric_text, canonicalize_numeric_values_with_active_schema,
};

/// Parse a file into the representation foch writes.
///
/// `rel` is a semantic input, not a label. The schema binds a root type by
/// matching a game-relative prefix such as `common/static_modifiers`, so an
/// absolute path on disk matches nothing, every number is left as written, and
/// the comparison silently goes back to comparing spellings. Callers hold
/// scratch paths and must pass the game-relative one.
pub fn parse(rel: &str, path: &Path) -> Option<AstFile> {
	let text = fs::read(path).ok()?;
	let text = foch::game::eu4::text::decode_paradox_bytes(&text).into_owned();
	parse_text(rel, &text)
}

/// Parse in-memory source into the representation foch writes.
pub fn parse_text(rel: &str, source: &str) -> Option<AstFile> {
	let parsed = parse_clausewitz_content(PathBuf::from(rel), source);
	parsed
		.diagnostics
		.is_empty()
		.then(|| compose(rel, &parsed.ast))
}

/// Bring an already-composed tree — a layered module view, say — into the same
/// representation.
///
/// Composing a module picks whole definitions by key and never compares a
/// value, so canonicalizing the result is the same answer as canonicalizing
/// every input file first, and it skips the definitions composition discards.
pub fn compose(rel: &str, ast: &AstFile) -> AstFile {
	canonicalize_numeric_values_with_active_schema(Path::new(rel), ast)
}

/// Read a file's text in the representation foch writes, leaving every other
/// byte where it was.
///
/// Re-emitting a parsed file would bring the numbers into line and reformat
/// the comments and whitespace along with them — which is most of what a
/// similarity score is measuring, so the reformatting would swamp the signal.
pub fn read_text(rel: &str, path: &Path) -> Option<String> {
	let bytes = fs::read(path).ok()?;
	let text = foch::game::eu4::text::decode_paradox_bytes(&bytes).into_owned();
	Some(canonicalize_text(rel, &text))
}

/// The text form of [`read_text`], for content already in memory.
pub fn canonicalize_text(rel: &str, source: &str) -> String {
	canonicalize_numeric_text(Path::new(rel), source)
}
