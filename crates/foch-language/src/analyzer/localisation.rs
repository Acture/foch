use foch_core::model::{LocalisationDefinition, LocalisationDuplicate, ParseIssue};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct ParsedLocalisationEntryData {
	pub definition: LocalisationDefinition,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedLocalisationFile {
	pub entries: Vec<ParsedLocalisationEntryData>,
	pub duplicates: Vec<LocalisationDuplicate>,
	pub parse_issues: Vec<ParseIssue>,
}

pub(crate) fn parse_localisation_file(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &Path,
) -> ParsedLocalisationFile {
	let mut entries = Vec::new();
	let mut duplicates = Vec::new();
	let mut parse_issues = Vec::new();
	let raw = match fs::read(absolute_path) {
		Ok(raw) => raw,
		Err(err) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: 1,
				column: 1,
				message: format!("unable to read localisation file: {err}"),
			});
			return ParsedLocalisationFile {
				entries,
				duplicates,
				parse_issues,
			};
		}
	};

	let normalized = match normalize_localisation_source(&raw) {
		Ok(bytes) => bytes,
		Err(message) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: 1,
				column: 1,
				message,
			});
			return ParsedLocalisationFile {
				entries,
				duplicates,
				parse_issues,
			};
		}
	};

	let mut header_seen = false;
	let mut saw_active_line = false;
	let mut header_issue_emitted = false;
	let mut seen_keys = HashMap::<String, usize>::new();

	for (line_no, line) in line_slices(&normalized) {
		let mut line = line;
		if line_no == 1 {
			line = trim_prefix(line, &[0xEF, 0xBB, 0xBF]);
		}
		let trimmed = trim_ascii_start(line);
		if trimmed.is_empty() || trimmed.first() == Some(&b'#') {
			continue;
		}
		saw_active_line = true;

		if parse_localisation_header_bytes(trimmed).is_some() {
			header_seen = true;
			// Localisation keys are scoped per language section. Resetting
			// the seen-key map on each header avoids flagging the same key
			// reused across e.g. l_english and l_german blocks (notably in
			// vanilla `localisation/languages.yml`).
			seen_keys.clear();
			continue;
		}

		if !header_seen {
			if !header_issue_emitted {
				parse_issues.push(ParseIssue {
					mod_id: mod_id.to_string(),
					path: relative_path.to_path_buf(),
					line: line_no,
					column: 1,
					message: "missing or invalid localisation header".to_string(),
				});
				header_issue_emitted = true;
			}
			continue;
		}

		let Some(entry) = parse_localisation_entry_bytes(trimmed) else {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: line_no,
				column: 1,
				message: "invalid localisation entry".to_string(),
			});
			continue;
		};
		let key = entry.key;
		let column = find_subslice(line, key.as_bytes()).map_or(1, |idx| idx + 1);
		let definition = LocalisationDefinition {
			key: key.clone(),
			mod_id: mod_id.to_string(),
			path: relative_path.to_path_buf(),
			line: line_no,
			column,
		};

		if let Some(first_line) = seen_keys.insert(key.clone(), line_no) {
			duplicates.push(LocalisationDuplicate {
				key,
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				first_line,
				duplicate_line: line_no,
			});
		}

		entries.push(ParsedLocalisationEntryData { definition });
	}

	if saw_active_line && !header_seen && !header_issue_emitted {
		parse_issues.push(ParseIssue {
			mod_id: mod_id.to_string(),
			path: relative_path.to_path_buf(),
			line: 1,
			column: 1,
			message: "missing localisation header".to_string(),
		});
	}

	ParsedLocalisationFile {
		entries,
		duplicates,
		parse_issues,
	}
}

pub(crate) fn collect_localisation_definitions_from_root(
	mod_id: &str,
	root: &Path,
) -> Vec<LocalisationDefinition> {
	let mut definitions = Vec::new();
	for entry in walkdir::WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
	{
		if !entry.file_type().is_file() {
			continue;
		}
		let absolute = entry.path();
		let Some(relative) = absolute.strip_prefix(root).ok() else {
			continue;
		};
		let normalized = relative.to_string_lossy().replace('\\', "/");
		if !(normalized.starts_with("localisation/")
			|| normalized.starts_with("common/localisation/"))
		{
			continue;
		}
		let Some(ext) = absolute.extension().and_then(|value| value.to_str()) else {
			continue;
		};
		if !matches!(ext.to_ascii_lowercase().as_str(), "yml" | "yaml") {
			continue;
		}
		let parsed = parse_localisation_file(mod_id, absolute, relative);
		definitions.extend(parsed.entries.into_iter().map(|item| item.definition));
	}
	definitions.sort_by(|lhs, rhs| {
		(
			lhs.path.clone(),
			lhs.line,
			lhs.column,
			lhs.key.clone(),
			lhs.mod_id.clone(),
		)
			.cmp(&(
				rhs.path.clone(),
				rhs.line,
				rhs.column,
				rhs.key.clone(),
				rhs.mod_id.clone(),
			))
	});
	definitions.dedup_by(|lhs, rhs| {
		lhs.path == rhs.path
			&& lhs.line == rhs.line
			&& lhs.column == rhs.column
			&& lhs.key == rhs.key
			&& lhs.mod_id == rhs.mod_id
	});
	definitions
}

struct ParsedLocalisationEntry {
	key: String,
}

fn normalize_localisation_source(raw: &[u8]) -> Result<Vec<u8>, String> {
	if raw.starts_with(&[0xFF, 0xFE]) {
		return decode_utf16_with_bom(&raw[2..], true);
	}
	if raw.starts_with(&[0xFE, 0xFF]) {
		return decode_utf16_with_bom(&raw[2..], false);
	}
	Ok(raw.to_vec())
}

fn decode_utf16_with_bom(raw: &[u8], little_endian: bool) -> Result<Vec<u8>, String> {
	if !raw.len().is_multiple_of(2) {
		return Err("invalid utf-16 localisation file".to_string());
	}
	let mut units = Vec::with_capacity(raw.len() / 2);
	for chunk in raw.chunks_exact(2) {
		let unit = if little_endian {
			u16::from_le_bytes([chunk[0], chunk[1]])
		} else {
			u16::from_be_bytes([chunk[0], chunk[1]])
		};
		units.push(unit);
	}
	String::from_utf16(&units)
		.map(|decoded| decoded.into_bytes())
		.map_err(|_| "invalid utf-16 localisation file".to_string())
}

fn line_slices(bytes: &[u8]) -> Vec<(usize, &[u8])> {
	let mut lines = Vec::new();
	let mut start = 0usize;
	let mut line_no = 1usize;
	for (idx, byte) in bytes.iter().enumerate() {
		if *byte == b'\n' {
			let mut line = &bytes[start..idx];
			if line.ends_with(b"\r") {
				line = &line[..line.len() - 1];
			}
			lines.push((line_no, line));
			line_no += 1;
			start = idx + 1;
		}
	}
	if start <= bytes.len() {
		let mut line = &bytes[start..];
		if line.ends_with(b"\r") {
			line = &line[..line.len() - 1];
		}
		if !line.is_empty() || bytes.is_empty() {
			lines.push((line_no, line));
		}
	}
	lines
}

fn trim_prefix<'a>(bytes: &'a [u8], prefix: &[u8]) -> &'a [u8] {
	bytes.strip_prefix(prefix).unwrap_or(bytes)
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

fn parse_localisation_header_bytes(line: &[u8]) -> Option<&str> {
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

fn parse_localisation_entry_bytes(line: &[u8]) -> Option<ParsedLocalisationEntry> {
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
	let key = std::str::from_utf8(key_bytes).ok()?.to_string();

	let mut remainder = trim_ascii_start(&trimmed[colon_idx + 1..]);
	if remainder.first().is_some_and(|byte| byte.is_ascii_digit()) {
		let version_end = remainder
			.iter()
			.position(|byte| !byte.is_ascii_digit())
			.unwrap_or(remainder.len());
		remainder = trim_ascii_start(&remainder[version_end..]);
	}
	let value_start = remainder.iter().position(|byte| *byte == b'"')?;
	// EU4's own loader is permissive: it accepts (a) entries whose value has no
	// closing quote on the same line (treats EOL as terminator) and (b) any
	// trailing text after the closing quote (stray words, mid-line comments
	// without `#`). Mods routinely ship both forms — see e.g. the unclosed
	// quote on `ME_Mann_Events.5.d` and the trailing-prose comment on
	// `FEE_Eranshahr_Events.4.OPT2`. Treat both as valid so the index records
	// the key even though the value is technically malformed.
	let _ = value_start;
	Some(ParsedLocalisationEntry { key })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
	if needle.is_empty() {
		return Some(0);
	}
	haystack
		.windows(needle.len())
		.position(|window| window == needle)
}

#[cfg(test)]
mod tests {
	use super::parse_localisation_file;
	use std::fs;
	use std::path::PathBuf;
	use tempfile::TempDir;

	#[test]
	fn parser_accepts_gbk_value_bytes_without_utf8_lossy() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("test_l_english.yml");
		fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
		let mut bytes = Vec::from(&b"l_english:\n TEST:0 \""[..]);
		bytes.extend([0xD6, 0xD0, 0xCE, 0xC4]);
		bytes.extend(&b"\"\n"[..]);
		fs::write(&path, bytes).expect("write file");

		let parsed = parse_localisation_file(
			"mod",
			&path,
			PathBuf::from("localisation/test_l_english.yml").as_path(),
		);
		assert_eq!(parsed.entries.len(), 1);
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
	}

	#[test]
	fn duplicate_detection_resets_per_language_section() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("languages.yml");
		fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
		let source = "l_english:\n l_english:0 \"English\"\n l_german:0 \"German\"\nl_german:\n l_english:0 \"Englisch\"\n l_german:0 \"Deutsch\"\n";
		fs::write(&path, source).expect("write file");

		let parsed = parse_localisation_file(
			"mod",
			&path,
			PathBuf::from("localisation/languages.yml").as_path(),
		);
		assert_eq!(parsed.entries.len(), 4);
		assert!(
			parsed.duplicates.is_empty(),
			"keys reused across sections must not be reported as duplicates: {:?}",
			parsed.duplicates,
		);
	}

	#[test]
	fn duplicate_detection_still_flags_within_section() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("dupes_l_english.yml");
		fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
		let source = "l_english:\n FOO:0 \"first\"\n FOO:0 \"second\"\n";
		fs::write(&path, source).expect("write file");

		let parsed = parse_localisation_file(
			"mod",
			&path,
			PathBuf::from("localisation/dupes_l_english.yml").as_path(),
		);
		assert_eq!(parsed.duplicates.len(), 1);
		assert_eq!(parsed.duplicates[0].key, "FOO");
	}

	#[test]
	fn parser_indexes_entry_with_unclosed_quote() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp
			.path()
			.join("localisation")
			.join("unclosed_l_english.yml");
		fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
		// EU4's loader treats EOL as the implicit string terminator, so mods
		// that ship lines like `KEY: "value` (no closing quote) still get the
		// key registered. Foch must match that behaviour or downstream
		// references will be falsely flagged as missing localisation.
		let source =
			"l_english:\n EVT_5_d: \"Some prose without a closing quote\n EVT_5_a: \"Reply!\"\n";
		fs::write(&path, source).expect("write file");

		let parsed = parse_localisation_file(
			"mod",
			&path,
			PathBuf::from("localisation/unclosed_l_english.yml").as_path(),
		);
		let keys: Vec<_> = parsed
			.entries
			.iter()
			.map(|entry| entry.definition.key.clone())
			.collect();
		assert_eq!(keys, vec!["EVT_5_d".to_string(), "EVT_5_a".to_string()]);
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
	}

	#[test]
	fn parser_indexes_entry_with_trailing_prose_after_closing_quote() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp
			.path()
			.join("localisation")
			.join("trailing_l_english.yml");
		fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
		// EU4 ignores anything after the closing quote on a localisation line,
		// so trailing prose comments without a leading `#` must not block
		// indexing of the key.
		let source = "l_english:\n OPT2: \"Short reply!\" was cut for making the option too long\n";
		fs::write(&path, source).expect("write file");

		let parsed = parse_localisation_file(
			"mod",
			&path,
			PathBuf::from("localisation/trailing_l_english.yml").as_path(),
		);
		assert_eq!(parsed.entries.len(), 1);
		assert_eq!(parsed.entries[0].definition.key, "OPT2");
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
	}
}
