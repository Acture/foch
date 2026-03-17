use crate::check::model::{
	CsvRow, DocumentFamily, DocumentRecord, FamilyParseStats, JsonProperty, LocalisationDefinition,
	LocalisationDuplicate, ParseFamilyStats, ParseIssue, SemanticIndex,
};
use crate::check::semantic_index::{ParsedScriptFile, build_semantic_index, parse_script_file};
use encoding_rs::WINDOWS_1252;
use rayon::prelude::*;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub(crate) struct DiscoveredTextDocument {
	pub absolute_path: PathBuf,
	pub relative_path: PathBuf,
	pub family: DocumentFamily,
}

#[derive(Clone, Debug)]
pub(crate) enum ParsedTextDocument {
	Clausewitz(ParsedScriptFile),
	Localisation(ParsedLocalisationDocument),
	Csv(ParsedCsvDocument),
	Json(ParsedJsonDocument),
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedLocalisationDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub entries: Vec<LocalisationDefinition>,
	pub duplicates: Vec<LocalisationDuplicate>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedCsvDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub rows: Vec<CsvRow>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedJsonDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub properties: Vec<JsonProperty>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ParsedDocumentBatch {
	pub documents: Vec<ParsedTextDocument>,
	pub clausewitz_cache_hits: usize,
	pub clausewitz_cache_misses: usize,
	pub parse_stats: ParseFamilyStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsvSchema {
	Generic,
	Eu4Adjacencies,
	Eu4Definition,
}

pub(crate) fn discover_text_documents(root: &Path) -> Vec<DiscoveredTextDocument> {
	let mut docs = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		let Some(relative_path) = path.strip_prefix(root).ok() else {
			continue;
		};
		if is_excluded_text_path(relative_path) {
			continue;
		}
		let Some(family) = classify_document_family(relative_path) else {
			continue;
		};

		docs.push(DiscoveredTextDocument {
			absolute_path: path.to_path_buf(),
			relative_path: relative_path.to_path_buf(),
			family,
		});
	}

	docs.sort_by(|lhs, rhs| lhs.relative_path.cmp(&rhs.relative_path));
	docs
}

pub(crate) fn parse_discovered_text_documents(
	mod_id: &str,
	root: &Path,
	documents: &[DiscoveredTextDocument],
) -> ParsedDocumentBatch {
	let parsed: Vec<Option<ParsedTextDocument>> = documents
		.par_iter()
		.map(|doc| parse_text_document(mod_id, root, doc))
		.collect();

	let mut batch = ParsedDocumentBatch::default();
	for doc in parsed.into_iter().flatten() {
		match document_parse_details(&doc) {
			DocumentParseDetails::Clausewitz {
				parse_issue_count,
				parse_ok,
				cache_hit,
			} => {
				let stats = &mut batch.parse_stats.clausewitz_mainline;
				stats.documents += 1;
				stats.parse_issue_count += parse_issue_count;
				if !parse_ok {
					stats.parse_failed_documents += 1;
				}
				if cache_hit {
					batch.clausewitz_cache_hits += 1;
				} else {
					batch.clausewitz_cache_misses += 1;
				}
			}
			DocumentParseDetails::Localisation {
				parse_issue_count,
				parse_ok,
			} => record_family_parse_details(
				&mut batch.parse_stats.localisation,
				parse_issue_count,
				parse_ok,
			),
			DocumentParseDetails::Csv {
				parse_issue_count,
				parse_ok,
			} => {
				record_family_parse_details(&mut batch.parse_stats.csv, parse_issue_count, parse_ok)
			}
			DocumentParseDetails::Json {
				parse_issue_count,
				parse_ok,
			} => record_family_parse_details(
				&mut batch.parse_stats.json,
				parse_issue_count,
				parse_ok,
			),
		}
		batch.documents.push(doc);
	}

	batch
}

pub(crate) fn build_semantic_index_from_documents(
	documents: &[ParsedTextDocument],
) -> SemanticIndex {
	let clausewitz_docs: Vec<ParsedScriptFile> = documents
		.iter()
		.filter_map(|doc| match doc {
			ParsedTextDocument::Clausewitz(file) => Some(file.clone()),
			_ => None,
		})
		.collect();

	let mut index = build_semantic_index(&clausewitz_docs);

	for doc in documents {
		match doc {
			ParsedTextDocument::Clausewitz(file) => {
				index.documents.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.relative_path.clone(),
					family: DocumentFamily::Clausewitz,
					parse_ok: file.parse_issues.is_empty(),
				});
			}
			ParsedTextDocument::Localisation(file) => {
				index.documents.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.path.clone(),
					family: DocumentFamily::Localisation,
					parse_ok: file.parse_issues.is_empty(),
				});
				index.localisation_definitions.extend(file.entries.clone());
				index
					.localisation_duplicates
					.extend(file.duplicates.clone());
				index.parse_issues.extend(file.parse_issues.clone());
			}
			ParsedTextDocument::Csv(file) => {
				index.documents.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.path.clone(),
					family: DocumentFamily::Csv,
					parse_ok: file.parse_issues.is_empty(),
				});
				index.csv_rows.extend(file.rows.clone());
				index.parse_issues.extend(file.parse_issues.clone());
			}
			ParsedTextDocument::Json(file) => {
				index.documents.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.path.clone(),
					family: DocumentFamily::Json,
					parse_ok: file.parse_issues.is_empty(),
				});
				index.json_properties.extend(file.properties.clone());
				index.parse_issues.extend(file.parse_issues.clone());
			}
		}
	}

	index.documents.sort_by(|lhs, rhs| {
		(lhs.path.clone(), lhs.mod_id.clone()).cmp(&(rhs.path.clone(), rhs.mod_id.clone()))
	});
	index.documents.dedup_by(|lhs, rhs| {
		lhs.path == rhs.path
			&& lhs.mod_id == rhs.mod_id
			&& lhs.family == rhs.family
			&& lhs.parse_ok == rhs.parse_ok
	});

	index
}

pub(crate) fn classify_document_family(relative_path: &Path) -> Option<DocumentFamily> {
	let ext = relative_path
		.extension()
		.and_then(|value| value.to_str())
		.map(|value| value.to_ascii_lowercase())?;

	match ext.as_str() {
		"txt" | "gui" | "gfx" | "asset" => Some(DocumentFamily::Clausewitz),
		"mod" => Some(DocumentFamily::Clausewitz),
		"yml" | "yaml" => Some(DocumentFamily::Localisation),
		"csv" => Some(DocumentFamily::Csv),
		"json" => Some(DocumentFamily::Json),
		_ => None,
	}
}

pub(crate) fn parse_text_document(
	mod_id: &str,
	root: &Path,
	doc: &DiscoveredTextDocument,
) -> Option<ParsedTextDocument> {
	match doc.family {
		DocumentFamily::Clausewitz => {
			parse_script_file(mod_id, root, &doc.absolute_path).map(ParsedTextDocument::Clausewitz)
		}
		DocumentFamily::Localisation => Some(ParsedTextDocument::Localisation(
			parse_localisation_document(mod_id, &doc.absolute_path, &doc.relative_path),
		)),
		DocumentFamily::Csv => Some(ParsedTextDocument::Csv(parse_csv_document(
			mod_id,
			&doc.absolute_path,
			&doc.relative_path,
		))),
		DocumentFamily::Json => Some(ParsedTextDocument::Json(parse_json_document(
			mod_id,
			&doc.absolute_path,
			&doc.relative_path,
		))),
	}
}

fn parse_localisation_document(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &Path,
) -> ParsedLocalisationDocument {
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
			return ParsedLocalisationDocument {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				entries,
				duplicates,
				parse_issues,
			};
		}
	};
	let content = String::from_utf8_lossy(&raw);
	let mut header_seen = false;
	let mut saw_active_line = false;
	let mut header_issue_emitted = false;
	let mut seen_keys = HashMap::<String, usize>::new();

	for (line_idx, line) in content.lines().enumerate() {
		let line_no = line_idx + 1;
		let line = if line_idx == 0 {
			line.trim_start_matches('\u{feff}')
		} else {
			line
		};
		let trimmed = line.trim_start();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		saw_active_line = true;

		if parse_localisation_header(trimmed).is_some() {
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

		let Some(entry) = parse_localisation_entry(trimmed) else {
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
		let column = line.find(&key).map_or(1, |idx| idx + 1);
		let entry = LocalisationDefinition {
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

		entries.push(entry);
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

	ParsedLocalisationDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_path_buf(),
		entries,
		duplicates,
		parse_issues,
	}
}

fn parse_csv_document(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &Path,
) -> ParsedCsvDocument {
	let mut rows = Vec::new();
	let mut parse_issues = Vec::new();
	let raw = match fs::read(absolute_path) {
		Ok(raw) => raw,
		Err(err) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: 1,
				column: 1,
				message: format!("unable to read csv file: {err}"),
			});
			return ParsedCsvDocument {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				rows,
				parse_issues,
			};
		}
	};
	let content = decode_csv_bytes(&raw);
	let schema = csv_schema_for(relative_path);

	let mut delimiter = ',';
	if content
		.lines()
		.next()
		.is_some_and(|line| line.matches(';').count() > line.matches(',').count())
	{
		delimiter = ';';
	}

	let mut expected_columns = None;
	for (line_idx, line) in content.lines().enumerate() {
		let line_no = line_idx + 1;
		let line = if line_idx == 0 {
			line.trim_start_matches('\u{feff}')
		} else {
			line
		};
		if line.trim().is_empty() {
			continue;
		}
		let mut cols = split_csv_line(line, delimiter);
		if let Some((expected, actual)) =
			validate_csv_columns(schema, line_no, &mut cols, &mut expected_columns)
		{
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: line_no,
				column: 1,
				message: format!(
					"inconsistent csv column count: expected {expected}, got {actual}"
				),
			});
		}

		let identity = cols
			.iter()
			.find(|value| !value.trim().is_empty())
			.cloned()
			.unwrap_or_else(|| format!("row_{line_no}"));
		rows.push(CsvRow {
			identity,
			mod_id: mod_id.to_string(),
			path: relative_path.to_path_buf(),
			line: line_no,
			column: 1,
		});
	}

	ParsedCsvDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_path_buf(),
		rows,
		parse_issues,
	}
}

fn decode_csv_bytes(raw: &[u8]) -> String {
	match std::str::from_utf8(raw) {
		Ok(content) => content.to_string(),
		Err(_) => {
			let (decoded, _, _) = WINDOWS_1252.decode(raw);
			decoded.into_owned()
		}
	}
}

fn csv_schema_for(relative_path: &Path) -> CsvSchema {
	let normalized = relative_path.to_string_lossy().replace('\\', "/");
	match normalized.as_str() {
		"map/adjacencies.csv" => CsvSchema::Eu4Adjacencies,
		"map/definition.csv" => CsvSchema::Eu4Definition,
		_ => CsvSchema::Generic,
	}
}

fn validate_csv_columns(
	schema: CsvSchema,
	line_no: usize,
	cols: &mut Vec<String>,
	expected_columns: &mut Option<usize>,
) -> Option<(usize, usize)> {
	match schema {
		CsvSchema::Generic => match expected_columns {
			Some(expected) if cols.len() != *expected => Some((*expected, cols.len())),
			Some(_) => None,
			None => {
				*expected_columns = Some(cols.len());
				None
			}
		},
		CsvSchema::Eu4Adjacencies => {
			let expected = 9;
			*expected_columns = Some(expected);
			(cols.len() != expected).then_some((expected, cols.len()))
		}
		CsvSchema::Eu4Definition => {
			let expected = 6;
			*expected_columns = Some(expected);
			if line_no == 1 {
				return (cols.len() != expected).then_some((expected, cols.len()));
			}
			match cols.len() {
				5 => {
					cols.push(String::new());
					None
				}
				6 => None,
				_ => Some((expected, cols.len())),
			}
		}
	}
}

fn parse_json_document(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &Path,
) -> ParsedJsonDocument {
	let mut properties = Vec::new();
	let mut parse_issues = Vec::new();
	let content = match fs::read_to_string(absolute_path) {
		Ok(content) => content,
		Err(err) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: 1,
				column: 1,
				message: format!("unable to read json file: {err}"),
			});
			return ParsedJsonDocument {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				properties,
				parse_issues,
			};
		}
	};

	match serde_json::from_str::<JsonValue>(&content) {
		Ok(json) => collect_json_properties(&json, "$", mod_id, relative_path, &mut properties),
		Err(err) => parse_issues.push(ParseIssue {
			mod_id: mod_id.to_string(),
			path: relative_path.to_path_buf(),
			line: err.line(),
			column: err.column(),
			message: err.to_string(),
		}),
	}

	ParsedJsonDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_path_buf(),
		properties,
		parse_issues,
	}
}

fn collect_json_properties(
	value: &JsonValue,
	base_path: &str,
	mod_id: &str,
	relative_path: &Path,
	out: &mut Vec<JsonProperty>,
) {
	match value {
		JsonValue::Object(map) => {
			for (key, child) in map {
				let next = format!("{base_path}.{key}");
				out.push(JsonProperty {
					key_path: next.clone(),
					mod_id: mod_id.to_string(),
					path: relative_path.to_path_buf(),
					line: 1,
					column: 1,
				});
				collect_json_properties(child, &next, mod_id, relative_path, out);
			}
		}
		JsonValue::Array(items) => {
			for (idx, child) in items.iter().enumerate() {
				let next = format!("{base_path}[{idx}]");
				collect_json_properties(child, &next, mod_id, relative_path, out);
			}
		}
		_ => {}
	}
}

fn split_csv_line(line: &str, delimiter: char) -> Vec<String> {
	let mut out = Vec::new();
	let mut current = String::new();
	let mut in_quotes = false;
	let mut chars = line.chars().peekable();

	while let Some(ch) = chars.next() {
		match ch {
			'"' => {
				if in_quotes && chars.peek() == Some(&'"') {
					current.push('"');
					chars.next();
				} else {
					in_quotes = !in_quotes;
				}
			}
			value if value == delimiter && !in_quotes => {
				out.push(current.trim().to_string());
				current.clear();
			}
			_ => current.push(ch),
		}
	}

	out.push(current.trim().to_string());
	if line.trim_end().ends_with(delimiter) && out.last().is_some_and(|value| value.is_empty()) {
		out.pop();
	}
	out
}

fn is_excluded_text_path(relative_path: &Path) -> bool {
	let normalized = relative_path.to_string_lossy().replace('\\', "/");
	for prefix in ["licenses/", "patchnotes/", "ebook/", "legal_notes/"] {
		if normalized.starts_with(prefix) {
			return true;
		}
	}
	false
}

fn record_family_parse_details(
	stats: &mut FamilyParseStats,
	parse_issue_count: usize,
	parse_ok: bool,
) {
	stats.documents += 1;
	stats.parse_issue_count += parse_issue_count;
	if !parse_ok {
		stats.parse_failed_documents += 1;
	}
}

enum DocumentParseDetails {
	Clausewitz {
		parse_issue_count: usize,
		parse_ok: bool,
		cache_hit: bool,
	},
	Localisation {
		parse_issue_count: usize,
		parse_ok: bool,
	},
	Csv {
		parse_issue_count: usize,
		parse_ok: bool,
	},
	Json {
		parse_issue_count: usize,
		parse_ok: bool,
	},
}

fn document_parse_details(doc: &ParsedTextDocument) -> DocumentParseDetails {
	match doc {
		ParsedTextDocument::Clausewitz(file) => DocumentParseDetails::Clausewitz {
			parse_issue_count: file.parse_issues.len(),
			parse_ok: file.parse_issues.is_empty(),
			cache_hit: file.parse_cache_hit,
		},
		ParsedTextDocument::Localisation(file) => DocumentParseDetails::Localisation {
			parse_issue_count: file.parse_issues.len(),
			parse_ok: file.parse_issues.is_empty(),
		},
		ParsedTextDocument::Csv(file) => DocumentParseDetails::Csv {
			parse_issue_count: file.parse_issues.len(),
			parse_ok: file.parse_issues.is_empty(),
		},
		ParsedTextDocument::Json(file) => DocumentParseDetails::Json {
			parse_issue_count: file.parse_issues.len(),
			parse_ok: file.parse_issues.is_empty(),
		},
	}
}

struct ParsedLocalisationEntry<'a> {
	key: String,
	#[allow(dead_code)]
	version: &'a str,
	#[allow(dead_code)]
	raw_value: &'a str,
	#[allow(dead_code)]
	trailing_comment: Option<&'a str>,
}

fn parse_localisation_header(line: &str) -> Option<&str> {
	let trimmed = line.trim();
	let body = if let Some((before, _comment)) = trimmed.split_once('#') {
		before.trim_end()
	} else {
		trimmed
	};
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

fn parse_localisation_entry(line: &str) -> Option<ParsedLocalisationEntry<'_>> {
	let trimmed = line.trim_start();
	let (key, remainder) = trimmed.split_once(':')?;
	if key.is_empty()
		|| !key
			.chars()
			.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
	{
		return None;
	}

	let remainder = remainder.trim_start();
	let version_end = remainder
		.find(|ch: char| !ch.is_ascii_digit())
		.unwrap_or(remainder.len());
	if version_end == 0 {
		return None;
	}
	let version = &remainder[..version_end];
	let remainder = remainder[version_end..].trim_start();
	let value_start = remainder.find('"')?;
	let value_region = &remainder[value_start + 1..];
	let value_end = value_region.rfind('"')?;
	let raw_value = &value_region[..value_end];
	let trailing = value_region[value_end + 1..].trim_start();
	if !trailing.is_empty() && !trailing.starts_with('#') {
		return None;
	}
	Some(ParsedLocalisationEntry {
		key: key.to_string(),
		version,
		raw_value,
		trailing_comment: (!trailing.is_empty()).then_some(trailing),
	})
}

#[cfg(test)]
mod tests {
	use super::{
		classify_document_family, discover_text_documents, parse_csv_document,
		parse_localisation_document,
	};
	use crate::check::model::DocumentFamily;
	use std::fs;
	use std::path::Path;
	use tempfile::TempDir;

	#[test]
	fn classify_supported_text_families() {
		assert_eq!(
			classify_document_family(Path::new("events/a.txt")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(Path::new("interface/a.gui")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(Path::new("localisation/test_l_english.yml")),
			Some(DocumentFamily::Localisation)
		);
		assert_eq!(
			classify_document_family(Path::new("common/data.csv")),
			Some(DocumentFamily::Csv)
		);
		assert_eq!(
			classify_document_family(Path::new("common/settings.json")),
			Some(DocumentFamily::Json)
		);
		assert_eq!(
			classify_document_family(Path::new("script/shader.lua")),
			None
		);
	}

	#[test]
	fn discovery_finds_descriptor_and_ui_files() {
		let tmp = TempDir::new().expect("temp dir");
		fs::create_dir_all(tmp.path().join("interface")).expect("create interface");
		fs::write(tmp.path().join("descriptor.mod"), "name=\"a\"").expect("write descriptor");
		fs::write(
			tmp.path().join("interface").join("main.gui"),
			"windowType = { }",
		)
		.expect("write ui");

		let docs = discover_text_documents(tmp.path());
		assert!(
			docs.iter()
				.any(|doc| doc.relative_path == Path::new("descriptor.mod"))
		);
		assert!(
			docs.iter()
				.any(|doc| doc.relative_path == Path::new("interface/main.gui"))
		);
	}

	#[test]
	fn discovery_excludes_noise_prefixes() {
		let tmp = TempDir::new().expect("temp dir");
		fs::create_dir_all(tmp.path().join("licenses")).expect("create licenses");
		fs::create_dir_all(tmp.path().join("patchnotes")).expect("create patchnotes");
		fs::create_dir_all(tmp.path().join("events")).expect("create events");
		fs::write(tmp.path().join("licenses").join("LUA.txt"), "license").expect("write license");
		fs::write(tmp.path().join("patchnotes").join("1.0.txt"), "patchnotes")
			.expect("write patchnotes");
		fs::write(
			tmp.path().join("events").join("real.txt"),
			"namespace = test",
		)
		.expect("write event");

		let docs = discover_text_documents(tmp.path());
		assert_eq!(docs.len(), 1);
		assert_eq!(docs[0].relative_path, Path::new("events/real.txt"));
	}

	#[test]
	fn localisation_parser_accepts_internal_quotes_and_trailing_comments() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("test_l_english.yml");
		fs::create_dir_all(path.parent().expect("loc parent")).expect("create loc dir");
		fs::write(
			&path,
			"l_english:\nexample.key:0 \"The term \"Great Power\" is used here.\" # comment\n",
		)
		.expect("write loc");

		let parsed =
			parse_localisation_document("mod", &path, Path::new("localisation/test_l_english.yml"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.entries.len(), 1);
		assert_eq!(parsed.entries[0].key, "example.key");
	}

	#[test]
	fn localisation_parser_reports_malformed_entry() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("bad_l_english.yml");
		fs::create_dir_all(path.parent().expect("loc parent")).expect("create loc dir");
		fs::write(&path, "l_english:\nexample.key:0 Tooltip without quotes\n").expect("write loc");

		let parsed =
			parse_localisation_document("mod", &path, Path::new("localisation/bad_l_english.yml"));
		assert_eq!(parsed.entries.len(), 0);
		assert_eq!(parsed.parse_issues.len(), 1);
	}

	#[test]
	fn localisation_parser_accepts_multiple_language_headers() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("languages.yml");
		fs::create_dir_all(path.parent().expect("loc parent")).expect("create loc dir");
		fs::write(
			&path,
			"l_english:\n foo:0 \"English\"\nl_german:\n foo:0 \"Deutsch\"\n",
		)
		.expect("write loc");

		let parsed =
			parse_localisation_document("mod", &path, Path::new("localisation/languages.yml"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.entries.len(), 2);
	}

	#[test]
	fn localisation_parser_ignores_comment_only_files() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("localisation").join("empty_l_german.yml");
		fs::create_dir_all(path.parent().expect("loc parent")).expect("create loc dir");
		fs::write(&path, "# comment only\n# l_german:\n").expect("write loc");

		let parsed =
			parse_localisation_document("mod", &path, Path::new("localisation/empty_l_german.yml"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert!(parsed.entries.is_empty());
	}

	#[test]
	fn csv_parser_accepts_trailing_delimiter_row() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("map").join("adjacencies.csv");
		fs::create_dir_all(path.parent().expect("csv parent")).expect("create csv dir");
		fs::write(
			&path,
			"From;To;Type;x;y;z;w;u;v\n-1;-1;;-1;-1;-1;-1;-1;-1;\n",
		)
		.expect("write csv");

		let parsed = parse_csv_document("mod", &path, Path::new("map/adjacencies.csv"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.rows.len(), 2);
	}

	#[test]
	fn csv_parser_decodes_windows_1252_input() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("common").join("names.csv");
		fs::create_dir_all(path.parent().expect("csv parent")).expect("create csv dir");
		fs::write(&path, b"Name;Value\nMalm\xf6;1\n").expect("write csv");

		let parsed = parse_csv_document("mod", &path, Path::new("common/names.csv"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.rows[1].identity, "Malmö");
	}

	#[test]
	fn csv_parser_accepts_definition_standard_and_variant_rows() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("map").join("definition.csv");
		fs::create_dir_all(path.parent().expect("csv parent")).expect("create csv dir");
		fs::write(
			&path,
			"province;red;green;blue;x;x\n1;128;34;64;Stockholm;x\n3004;189;110;220;Unused1;\n",
		)
		.expect("write csv");

		let parsed = parse_csv_document("mod", &path, Path::new("map/definition.csv"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.rows.len(), 3);
	}

	#[test]
	fn csv_parser_rejects_invalid_definition_column_counts() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("map").join("definition.csv");
		fs::create_dir_all(path.parent().expect("csv parent")).expect("create csv dir");
		fs::write(
			&path,
			"province;red;green;blue;x;x\n1;128;34;64\n2;0;36;128;Östergötland;x;extra\n",
		)
		.expect("write csv");

		let parsed = parse_csv_document("mod", &path, Path::new("map/definition.csv"));
		assert_eq!(parsed.parse_issues.len(), 2, "{:?}", parsed.parse_issues);
	}
}
