#![allow(dead_code)]

use std::borrow::Cow;
use std::path::PathBuf;

use foch_core::decode_paradox_bytes;
use foch_engine::merge::patch::{ClausewitzPatch, diff_ast};
use foch_language::analyzer::content_family::{MergeKeySource, MergePolicies, CwtType};
use foch_language::analyzer::parser::{ParseResult, parse_clausewitz_content};
use foch_language::analyzer::semantic_index::ParsedScriptFile;

const SCRIPT_SEPARATOR: &[u8] = b"\n---\n";
pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

const FIXED_BASE: &str = "country_event = {\n\tid = fuzz.1\n\ttitle = \"old\"\n\toption = {\n\t\tname = \"A\"\n\t\tadd_prestige = 1\n\t}\n}\nallowed_tags = { FRA ENG }\nflag = yes\n";
const FIXED_OVERLAY: &str = "country_event = {\n\tid = fuzz.1\n\ttitle = \"new\"\n\toption = {\n\t\tname = \"A\"\n\t\tadd_prestige = 2\n\t}\n\toption = {\n\t\tname = \"B\"\n\t\tadd_stability = 1\n\t}\n}\nallowed_tags = { FRA ENG CAS }\nflag = no\nextra = 42\n";

pub fn parse_clausewitz_file_from_bytes(path: &str, bytes: &[u8]) -> ParseResult {
	let content = decode_paradox_bytes(bytes);
	parse_clausewitz_content(PathBuf::from(path), &content)
}

pub fn parsed_script_from_bytes(
	mod_id: &str,
	path: &str,
	bytes: &[u8],
	require_clean_parse: bool,
) -> Option<ParsedScriptFile> {
	if bytes.len() > MAX_SCRIPT_BYTES {
		return None;
	}
	let content = decode_paradox_bytes(bytes);
	parsed_script_from_content(mod_id, path, content, require_clean_parse)
}

pub fn fixed_base() -> ParsedScriptFile {
	parsed_script_from_str("base", "common/fuzz_base.txt", FIXED_BASE)
}

pub fn fixed_patches() -> Vec<ClausewitzPatch> {
	let base = fixed_base();
	let overlay = parsed_script_from_str("overlay", "common/fuzz_overlay.txt", FIXED_OVERLAY);
	diff_ast(&base, &overlay, MergeKeySource::AssignmentKey)
}

pub fn default_policies() -> MergePolicies {
	MergePolicies::default()
}

pub fn split_pair(data: &[u8]) -> (&[u8], &[u8]) {
	let parts = split_by_separator(data);
	if parts.len() >= 2 {
		return (parts[0], parts[1]);
	}
	let mid = data.len() / 2;
	(&data[..mid], &data[mid..])
}

pub fn split_three(data: &[u8]) -> (&[u8], &[u8], &[u8]) {
	let parts = split_by_separator(data);
	if parts.len() >= 3 {
		return (parts[0], parts[1], parts[2]);
	}
	let first = data.len() / 3;
	let second = first.saturating_mul(2);
	(&data[..first], &data[first..second], &data[second..])
}

fn parsed_script_from_str(mod_id: &str, path: &str, content: &str) -> ParsedScriptFile {
	let parsed = parse_clausewitz_content(PathBuf::from(path), content);
	assert!(
		parsed.diagnostics.is_empty(),
		"fixed fuzz fixture must parse cleanly"
	);
	build_parsed_script(mod_id, path, parsed, content.to_string())
}

fn parsed_script_from_content(
	mod_id: &str,
	path: &str,
	content: Cow<'_, str>,
	require_clean_parse: bool,
) -> Option<ParsedScriptFile> {
	let parsed = parse_clausewitz_content(PathBuf::from(path), &content);
	if require_clean_parse && !parsed.diagnostics.is_empty() {
		return None;
	}
	Some(build_parsed_script(
		mod_id,
		path,
		parsed,
		content.into_owned(),
	))
}

fn build_parsed_script(
	mod_id: &str,
	path: &str,
	parsed: ParseResult,
	source: String,
) -> ParsedScriptFile {
	let path = PathBuf::from(path);
	ParsedScriptFile {
		mod_id: mod_id.to_string(),
		path: path.clone(),
		relative_path: path,
		content_family: None,
		file_kind: CwtType::new("other"),
		module_name: "fuzz".to_string(),
		ast: parsed.ast,
		source,
		parse_issues: Vec::new(),
		parse_cache_hit: false,
	}
}

fn split_by_separator(data: &[u8]) -> Vec<&[u8]> {
	let mut parts = Vec::new();
	let mut start = 0;
	while let Some(offset) = find_subslice(&data[start..], SCRIPT_SEPARATOR) {
		let end = start + offset;
		parts.push(&data[start..end]);
		start = end + SCRIPT_SEPARATOR.len();
	}
	if start > 0 {
		parts.push(&data[start..]);
	}
	parts
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
	if needle.is_empty() {
		return Some(0);
	}
	haystack
		.windows(needle.len())
		.position(|window| window == needle)
}
