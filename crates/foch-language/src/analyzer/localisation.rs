use encoding_rs::{GBK, WINDOWS_1252};
use foch_core::model::{
	DecodedLocalisationValue, LocalisationDefinition, LocalisationDuplicate,
	LocalisationValueEncoding, ParseIssue,
};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct ParsedLocalisationEntryData {
	pub definition: LocalisationDefinition,
	#[allow(dead_code)]
	pub decoded_value: DecodedLocalisationValue,
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

		entries.push(ParsedLocalisationEntryData {
			definition,
			decoded_value: decode_localisation_value(entry.raw_value),
		});
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

struct ParsedLocalisationEntry<'a> {
	key: String,
	raw_value: &'a [u8],
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

fn parse_localisation_entry_bytes(line: &[u8]) -> Option<ParsedLocalisationEntry<'_>> {
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
	let value_region = &remainder[value_start + 1..];
	let value_end = value_region.iter().rposition(|byte| *byte == b'"')?;
	let trailing = trim_ascii_start(&value_region[value_end + 1..]);
	if !trailing.is_empty() && trailing.first() != Some(&b'#') {
		return None;
	}
	Some(ParsedLocalisationEntry {
		key,
		raw_value: &value_region[..value_end],
	})
}

fn decode_localisation_value(raw: &[u8]) -> DecodedLocalisationValue {
	if let Ok(decoded) = std::str::from_utf8(raw) {
		return DecodedLocalisationValue {
			raw_bytes: raw.to_vec(),
			decoded_value: Some(decoded.to_string()),
			decode_kind: LocalisationValueEncoding::Utf8,
			decode_ok: true,
		};
	}

	if looks_like_eu4dll_escape(raw)
		&& let Some(decoded) = decode_eu4dll_escape(raw)
	{
		return DecodedLocalisationValue {
			raw_bytes: raw.to_vec(),
			decoded_value: Some(decoded),
			decode_kind: LocalisationValueEncoding::Eu4DllEscape,
			decode_ok: true,
		};
	}

	let (decoded, _, had_errors) = GBK.decode(raw);
	if !had_errors {
		return DecodedLocalisationValue {
			raw_bytes: raw.to_vec(),
			decoded_value: Some(decoded.into_owned()),
			decode_kind: LocalisationValueEncoding::Gb18030,
			decode_ok: true,
		};
	}

	let (decoded, _, had_errors) = WINDOWS_1252.decode(raw);
	if !had_errors {
		return DecodedLocalisationValue {
			raw_bytes: raw.to_vec(),
			decoded_value: Some(decoded.into_owned()),
			decode_kind: LocalisationValueEncoding::Windows1252,
			decode_ok: true,
		};
	}

	if let Some(decoded) = decode_eu4dll_escape(raw) {
		return DecodedLocalisationValue {
			raw_bytes: raw.to_vec(),
			decoded_value: Some(decoded),
			decode_kind: LocalisationValueEncoding::Eu4DllEscape,
			decode_ok: true,
		};
	}

	DecodedLocalisationValue {
		raw_bytes: raw.to_vec(),
		decoded_value: None,
		decode_kind: LocalisationValueEncoding::RawBytes,
		decode_ok: false,
	}
}

fn looks_like_eu4dll_escape(raw: &[u8]) -> bool {
	let mut idx = 0usize;
	let mut escapes = 0usize;
	while idx < raw.len() {
		match raw[idx] {
			0x10..=0x13 => {
				if idx + 2 >= raw.len() {
					return false;
				}
				escapes += 1;
				idx += 3;
			}
			_ => idx += 1,
		}
	}
	escapes > 0
}

fn decode_eu4dll_escape(raw: &[u8]) -> Option<String> {
	let mut out = String::new();
	let mut idx = 0usize;
	while idx < raw.len() {
		let byte = raw[idx];
		idx += 1;
		let mut code_point = match byte {
			0x10..=0x13 => {
				if idx + 1 >= raw.len() {
					return None;
				}
				let low = raw[idx] as u32;
				let high = raw[idx + 1] as u32;
				idx += 2;
				let mut code = (high << 8) + low;
				match byte {
					0x11 => code = code.saturating_sub(0xE),
					0x12 => code += 0x900,
					0x13 => code += 0x8F2,
					_ => {}
				}
				code
			}
			other => cp1252_to_ucs2(other),
		};
		if code_point > 0xFFFF || (code_point > 0x100 && code_point < 0x98F) {
			code_point = 0x2026;
		}
		out.push(char::from_u32(code_point).unwrap_or('\u{2026}'));
	}
	Some(out)
}

fn cp1252_to_ucs2(cp: u8) -> u32 {
	match cp {
		0x80 => 0x20AC,
		0x82 => 0x201A,
		0x83 => 0x0192,
		0x84 => 0x201E,
		0x85 => 0x2026,
		0x86 => 0x2020,
		0x87 => 0x2021,
		0x88 => 0x02C6,
		0x89 => 0x2030,
		0x8A => 0x0160,
		0x8B => 0x2039,
		0x8C => 0x0152,
		0x8E => 0x017D,
		0x91 => 0x2018,
		0x92 => 0x2019,
		0x93 => 0x201C,
		0x94 => 0x201D,
		0x95 => 0x2022,
		0x96 => 0x2013,
		0x97 => 0x2014,
		0x98 => 0x02DC,
		0x99 => 0x2122,
		0x9A => 0x0161,
		0x9B => 0x203A,
		0x9C => 0x0153,
		0x9E => 0x017E,
		0x9F => 0x0178,
		_ => cp as u32,
	}
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
	use super::{decode_eu4dll_escape, parse_localisation_file};
	use std::fs;
	use std::path::PathBuf;
	use tempfile::TempDir;

	#[test]
	fn eu4dll_escape_decode_handles_multibyte_escape_sequence() {
		let decoded = decode_eu4dll_escape(&[0x10, 0x2d, 0x4e]).expect("decode escape");
		assert_eq!(decoded, "中");
	}

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
}
