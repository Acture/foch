use crate::check::model::{
	CsvRow, DocumentFamily, DocumentRecord, JsonProperty, LocalisationDefinition,
	LocalisationDuplicate, ParseIssue, SemanticIndex,
};
use crate::check::semantic_index::{ParsedScriptFile, build_semantic_index, parse_script_file};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
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

pub(crate) fn parse_text_documents(mod_id: &str, root: &Path) -> Vec<ParsedTextDocument> {
	discover_text_documents(root)
		.into_iter()
		.filter_map(|doc| parse_text_document(mod_id, root, &doc))
		.collect()
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
		"txt" | "lua" | "gui" | "gfx" | "asset" => Some(DocumentFamily::Clausewitz),
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

		if !header_seen {
			header_seen = true;
			if !localisation_header_regex().is_match(trimmed) {
				parse_issues.push(ParseIssue {
					mod_id: mod_id.to_string(),
					path: relative_path.to_path_buf(),
					line: line_no,
					column: 1,
					message: "missing or invalid localisation header".to_string(),
				});
			}
			continue;
		}

		let Some(captures) = localisation_entry_regex().captures(trimmed) else {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_path_buf(),
				line: line_no,
				column: 1,
				message: "invalid localisation entry".to_string(),
			});
			continue;
		};
		let Some(key_match) = captures.get(1) else {
			continue;
		};
		let key = key_match.as_str().to_string();
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

	if !header_seen {
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
	let content = match fs::read_to_string(absolute_path) {
		Ok(content) => content,
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
		if line.trim().is_empty() {
			continue;
		}
		let cols = split_csv_line(line, delimiter);
		if let Some(expected) = expected_columns {
			if cols.len() != expected {
				parse_issues.push(ParseIssue {
					mod_id: mod_id.to_string(),
					path: relative_path.to_path_buf(),
					line: line_no,
					column: 1,
					message: format!(
						"inconsistent csv column count: expected {expected}, got {}",
						cols.len()
					),
				});
			}
		} else {
			expected_columns = Some(cols.len());
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
	out
}

fn localisation_header_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| {
		Regex::new(r"^l_[A-Za-z0-9_]+:\s*$").expect("valid localisation header regex")
	})
}

fn localisation_entry_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| {
		Regex::new(r#"^([A-Za-z0-9_.-]+)\s*:\s*[0-9]+\s+"(?:[^"\\]|\\.)*"\s*$"#)
			.expect("valid localisation entry regex")
	})
}

#[cfg(test)]
mod tests {
	use super::{classify_document_family, discover_text_documents};
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
}
