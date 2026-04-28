//! Key-level dedup merge for `localisation/**.yml` files.
//!
//! Each merged file contains the union of keys defined by all contributors.
//! On collision, the highest-precedence contributor wins; lower-precedence
//! contributors still get to add keys that nobody higher defined.
//!
//! The output preserves the UTF-8 BOM (required by the EU4 engine), the
//! single `l_<lang>:` header, and the original entry text (key, version,
//! quoted value) for each surviving line. Comments and blank lines from the
//! source files are not preserved; the output is a normalized listing of
//! merged entries.

use crate::workspace::ResolvedFileContributor;
use std::collections::BTreeMap;
use std::fs;

/// Result of merging a localisation file across contributors.
#[derive(Debug)]
pub(crate) enum LocalisationMergeOutcome {
	/// Successfully merged. Bytes are ready to be written to disk
	/// (BOM-prefixed UTF-8).
	Merged(Vec<u8>),
	/// Contributors disagreed on the language header. Caller should fall
	/// back to last-writer-overlay and surface the included warning.
	LanguageMismatch { warning: String },
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

pub(crate) fn merge_localisation_file(
	target_path: &str,
	contributors: &[ResolvedFileContributor],
) -> Result<LocalisationMergeOutcome, String> {
	// Highest precedence first so "first writer wins" gives us the winner.
	let mut sorted: Vec<&ResolvedFileContributor> = contributors.iter().collect();
	sorted.sort_by(|a, b| b.precedence.cmp(&a.precedence));

	let mut language: Option<(String, String)> = None; // (lang, mod_id)
	let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
	let mut order: Vec<String> = Vec::new();

	for contributor in &sorted {
		let raw = fs::read(&contributor.absolute_path).map_err(|err| {
			format!(
				"failed to read {} ({}): {err}",
				contributor.absolute_path.display(),
				contributor.mod_id
			)
		})?;
		let bytes = strip_utf8_bom(&raw);

		let mut header_seen_lang: Option<String> = None;
		for line in split_lines(bytes) {
			let trimmed = trim_ascii_start(line);
			if trimmed.is_empty() || trimmed.first() == Some(&b'#') {
				continue;
			}
			if let Some(lang) = parse_header(trimmed) {
				if header_seen_lang.is_none() {
					header_seen_lang = Some(lang.to_string());
				}
				continue;
			}
			if header_seen_lang.is_none() {
				continue;
			}
			let Some(key) = extract_key(trimmed) else {
				continue;
			};
			if !entries.contains_key(&key) {
				let mut entry_line: Vec<u8> = Vec::with_capacity(trimmed.len() + 1);
				entry_line.push(b' ');
				entry_line.extend_from_slice(trim_ascii_end(trimmed));
				entries.insert(key.clone(), entry_line);
				order.push(key);
			}
		}

		let Some(lang) = header_seen_lang else {
			continue;
		};
		match &language {
			None => language = Some((lang, contributor.mod_id.clone())),
			Some((existing, existing_mod)) if existing == &lang => {
				let _ = existing_mod;
			}
			Some((existing, existing_mod)) => {
				return Ok(LocalisationMergeOutcome::LanguageMismatch {
					warning: format!(
						"localisation merge fallback for {}: language mismatch ({} declares l_{}, {} declares l_{})",
						target_path, existing_mod, existing, contributor.mod_id, lang,
					),
				});
			}
		}
	}

	let lang = match language {
		Some((lang, _)) => lang,
		None => {
			return Err(format!(
				"no l_<lang> header found in any contributor for {target_path}",
			));
		}
	};

	let mut out: Vec<u8> = Vec::with_capacity(UTF8_BOM.len() + 32 + entries.len() * 64);
	out.extend_from_slice(UTF8_BOM);
	out.extend_from_slice(format!("l_{lang}:\n").as_bytes());
	for key in &order {
		if let Some(line) = entries.get(key) {
			out.extend_from_slice(line);
			out.push(b'\n');
		}
	}
	Ok(LocalisationMergeOutcome::Merged(out))
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
	bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
	let mut out = Vec::new();
	let mut start = 0usize;
	for (idx, byte) in bytes.iter().enumerate() {
		if *byte == b'\n' {
			let mut line = &bytes[start..idx];
			if line.ends_with(b"\r") {
				line = &line[..line.len() - 1];
			}
			out.push(line);
			start = idx + 1;
		}
	}
	if start < bytes.len() {
		let mut line = &bytes[start..];
		if line.ends_with(b"\r") {
			line = &line[..line.len() - 1];
		}
		out.push(line);
	}
	out
}

fn trim_ascii_start(bytes: &[u8]) -> &[u8] {
	let idx = bytes
		.iter()
		.position(|byte| !byte.is_ascii_whitespace())
		.unwrap_or(bytes.len());
	&bytes[idx..]
}

fn trim_ascii_end(bytes: &[u8]) -> &[u8] {
	let idx = bytes
		.iter()
		.rposition(|byte| !byte.is_ascii_whitespace())
		.map_or(0, |idx| idx + 1);
	&bytes[..idx]
}

fn parse_header(line: &[u8]) -> Option<&str> {
	let trimmed = trim_ascii_end(line);
	let body = if let Some(idx) = trimmed.iter().position(|byte| *byte == b'#') {
		trim_ascii_end(&trimmed[..idx])
	} else {
		trimmed
	};
	let body = std::str::from_utf8(body).ok()?;
	let lang = body.strip_prefix("l_")?.strip_suffix(':')?;
	if lang.is_empty()
		|| !lang
			.chars()
			.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
	{
		return None;
	}
	Some(lang)
}

fn extract_key(line: &[u8]) -> Option<String> {
	let trimmed = trim_ascii_start(line);
	let colon_idx = trimmed.iter().position(|byte| *byte == b':')?;
	let key_bytes = &trimmed[..colon_idx];
	if key_bytes.is_empty()
		|| !key_bytes
			.iter()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
	{
		return None;
	}
	std::str::from_utf8(key_bytes).ok().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::workspace::ResolvedFileContributor;
	use tempfile::TempDir;

	fn make_contributor(
		dir: &std::path::Path,
		mod_id: &str,
		precedence: usize,
		filename: &str,
		bytes: &[u8],
	) -> ResolvedFileContributor {
		let path = dir.join(filename);
		std::fs::write(&path, bytes).expect("write contributor");
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: dir.to_path_buf(),
			absolute_path: path,
			precedence,
			is_base_game: false,
			parse_ok_hint: None,
		}
	}

	#[test]
	fn merges_disjoint_keys_into_union() {
		let tmp = TempDir::new().expect("tempdir");
		let hre = make_contributor(
			tmp.path(),
			"hre",
			10,
			"hre_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n HRE_KEY:0 \"Holy\"\n SHARED:0 \"From HRE\"\n",
		);
		let ee = make_contributor(
			tmp.path(),
			"ee",
			5,
			"ee_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n EE_KEY:0 \"Eastern\"\n SHARED:0 \"From EE\"\n",
		);
		let outcome = merge_localisation_file("localisation/test_l_english.yml", &[hre, ee])
			.expect("merge ok");
		let LocalisationMergeOutcome::Merged(bytes) = outcome else {
			panic!("expected merged outcome");
		};
		assert!(bytes.starts_with(UTF8_BOM), "BOM preserved");
		let text = std::str::from_utf8(&bytes).expect("utf-8");
		assert!(text.contains("l_english:"), "header present: {text}");
		assert!(text.contains("HRE_KEY"), "HRE-only key survives: {text}");
		assert!(text.contains("EE_KEY"), "EE-only key survives: {text}");
		// Highest precedence (HRE) wins on collision.
		assert!(text.contains("\"From HRE\""));
		assert!(!text.contains("\"From EE\""));
	}

	#[test]
	fn language_mismatch_falls_back() {
		let tmp = TempDir::new().expect("tempdir");
		let a = make_contributor(
			tmp.path(),
			"mod_a",
			10,
			"a_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n KEY_A:0 \"A\"\n",
		);
		let b = make_contributor(
			tmp.path(),
			"mod_b",
			5,
			"b_l_french.yml",
			b"\xEF\xBB\xBFl_french:\n KEY_B:0 \"B\"\n",
		);
		let outcome =
			merge_localisation_file("localisation/test_l_english.yml", &[a, b]).expect("ok");
		match outcome {
			LocalisationMergeOutcome::LanguageMismatch { warning } => {
				assert!(warning.contains("language mismatch"), "{warning}");
			}
			LocalisationMergeOutcome::Merged(_) => panic!("expected language mismatch"),
		}
	}

	#[test]
	fn bom_preserved_on_output_even_when_inputs_lack_it() {
		let tmp = TempDir::new().expect("tempdir");
		let a = make_contributor(
			tmp.path(),
			"a",
			10,
			"a_l_english.yml",
			b"l_english:\n KEY_A:0 \"A\"\n",
		);
		let b = make_contributor(
			tmp.path(),
			"b",
			5,
			"b_l_english.yml",
			b"l_english:\n KEY_B:0 \"B\"\n",
		);
		let LocalisationMergeOutcome::Merged(bytes) =
			merge_localisation_file("localisation/x_l_english.yml", &[a, b]).expect("ok")
		else {
			panic!("expected merged");
		};
		assert!(bytes.starts_with(UTF8_BOM));
	}

	#[test]
	fn collision_winner_is_highest_precedence() {
		let tmp = TempDir::new().expect("tempdir");
		let low = make_contributor(
			tmp.path(),
			"low",
			1,
			"low_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n SHARED:0 \"low value\"\n",
		);
		let high = make_contributor(
			tmp.path(),
			"high",
			99,
			"high_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n SHARED:0 \"high value\"\n",
		);
		// Pass in arbitrary order; merger must sort by precedence.
		let LocalisationMergeOutcome::Merged(bytes) =
			merge_localisation_file("localisation/x_l_english.yml", &[low, high]).expect("ok")
		else {
			panic!("expected merged");
		};
		let text = std::str::from_utf8(&bytes).unwrap();
		assert!(text.contains("\"high value\""));
		assert!(!text.contains("\"low value\""));
	}

	#[test]
	fn comments_and_blank_lines_skipped() {
		let tmp = TempDir::new().expect("tempdir");
		let a = make_contributor(
			tmp.path(),
			"a",
			10,
			"a_l_english.yml",
			b"\xEF\xBB\xBF# leading comment\nl_english:\n\n KEY_A:0 \"A\"\n# trailing\n",
		);
		let b = make_contributor(
			tmp.path(),
			"b",
			5,
			"b_l_english.yml",
			b"\xEF\xBB\xBFl_english:\n KEY_B:0 \"B\"\n",
		);
		let LocalisationMergeOutcome::Merged(bytes) =
			merge_localisation_file("localisation/x_l_english.yml", &[a, b]).expect("ok")
		else {
			panic!("expected merged");
		};
		let text = std::str::from_utf8(&bytes).unwrap();
		assert!(text.contains("KEY_A"));
		assert!(text.contains("KEY_B"));
		assert!(!text.contains("leading comment"));
	}
}
