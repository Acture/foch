use super::localisation::parse_localisation_file;
use super::semantic_index::{
	ParsedScriptFile, ParsedScriptWithInputIdentity, build_semantic_index,
	parse_script_file_with_input_identity, parse_script_file_without_cache_with_input_identity,
};
use foch::model::{
	CsvRow, DocumentFamily, DocumentRecord, FamilyParseStats, JsonProperty, LocalisationDefinition,
	LocalisationDuplicate, ParseFamilyStats, ParseIssue, SemanticIndex,
};
use rayon::prelude::*;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct DiscoveredTextDocument {
	pub absolute_path: PathBuf,
	pub relative_path: PathBuf,
	pub family: DocumentFamily,
}

#[derive(Clone, Debug)]
pub enum ParsedTextDocument {
	Clausewitz(ParsedScriptFile),
	Localisation(ParsedLocalisationDocument),
	Csv(ParsedCsvDocument),
	Json(ParsedJsonDocument),
}

#[derive(Clone, Debug)]
pub struct ParsedLocalisationDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub entries: Vec<LocalisationDefinition>,
	pub duplicates: Vec<LocalisationDuplicate>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub struct ParsedCsvDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub rows: Vec<CsvRow>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub struct ParsedJsonDocument {
	pub mod_id: String,
	pub path: PathBuf,
	pub properties: Vec<JsonProperty>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedDocumentBatch {
	pub documents: Vec<ParsedTextDocument>,
	pub document_input_identities: Vec<ParsedDocumentInputIdentity>,
	pub clausewitz_cache_hits: usize,
	pub clausewitz_cache_misses: usize,
	pub parse_stats: ParseFamilyStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedDocumentInputIdentity {
	pub relative_path: PathBuf,
	pub size_bytes: u64,
	pub content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsvSchema {
	Generic,
	Eu4Adjacencies,
	Eu4Definition,
}

pub fn discover_text_documents(root: &Path) -> Vec<DiscoveredTextDocument> {
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

pub fn discover_text_documents_from_paths(
	root: &Path,
	relative_paths: &[PathBuf],
) -> Vec<DiscoveredTextDocument> {
	let mut docs = Vec::new();
	for relative_path in relative_paths {
		if is_excluded_text_path(relative_path) {
			continue;
		}
		let Some(family) = classify_document_family(relative_path) else {
			continue;
		};
		let absolute_path = root.join(relative_path);
		if !absolute_path.is_file() {
			continue;
		}
		docs.push(DiscoveredTextDocument {
			absolute_path,
			relative_path: relative_path.clone(),
			family,
		});
	}
	docs.sort_by(|lhs, rhs| lhs.relative_path.cmp(&rhs.relative_path));
	docs
}

pub fn parse_discovered_text_documents(
	mod_id: &str,
	root: &Path,
	documents: &[DiscoveredTextDocument],
) -> ParsedDocumentBatch {
	parse_discovered_text_documents_with(
		mod_id,
		root,
		documents,
		parse_script_file_with_input_identity,
	)
}

pub fn parse_discovered_text_documents_without_cache(
	mod_id: &str,
	root: &Path,
	documents: &[DiscoveredTextDocument],
) -> ParsedDocumentBatch {
	parse_discovered_text_documents_with(
		mod_id,
		root,
		documents,
		parse_script_file_without_cache_with_input_identity,
	)
}

fn parse_discovered_text_documents_with(
	mod_id: &str,
	root: &Path,
	documents: &[DiscoveredTextDocument],
	parse_script: fn(&str, &Path, &Path) -> Option<ParsedScriptWithInputIdentity>,
) -> ParsedDocumentBatch {
	let parsed: Vec<Option<(ParsedTextDocument, Option<ParsedDocumentInputIdentity>)>> = documents
		.par_iter()
		.map(|doc| parse_text_document_with(mod_id, root, doc, parse_script))
		.collect();

	let mut batch = ParsedDocumentBatch::default();
	for (doc, input_identity) in parsed.into_iter().flatten() {
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
		if let Some(input_identity) = input_identity {
			batch.document_input_identities.push(input_identity);
		}
		batch.documents.push(doc);
	}

	batch
}

pub fn build_semantic_index_from_documents(documents: &[ParsedTextDocument]) -> SemanticIndex {
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

	sort_and_dedup_document_records(&mut index.documents);

	index
}

/// Builds the same index as [`build_semantic_index_from_documents`] while
/// consuming parsed documents so Clausewitz ASTs and source buffers are not
/// cloned solely to pass them to the semantic indexer.
pub fn build_semantic_index_from_owned_documents(
	documents: Vec<ParsedTextDocument>,
) -> SemanticIndex {
	let mut clausewitz_docs: Vec<ParsedScriptFile> = Vec::new();
	let mut document_records: Vec<DocumentRecord> = Vec::with_capacity(documents.len());
	let mut localisation_definitions: Vec<LocalisationDefinition> = Vec::new();
	let mut localisation_duplicates: Vec<LocalisationDuplicate> = Vec::new();
	let mut csv_rows: Vec<CsvRow> = Vec::new();
	let mut json_properties: Vec<JsonProperty> = Vec::new();
	let mut parse_issues: Vec<ParseIssue> = Vec::new();

	for doc in documents {
		match doc {
			ParsedTextDocument::Clausewitz(file) => {
				document_records.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.relative_path.clone(),
					family: DocumentFamily::Clausewitz,
					parse_ok: file.parse_issues.is_empty(),
				});
				clausewitz_docs.push(file);
			}
			ParsedTextDocument::Localisation(file) => {
				document_records.push(DocumentRecord {
					mod_id: file.mod_id,
					path: file.path,
					family: DocumentFamily::Localisation,
					parse_ok: file.parse_issues.is_empty(),
				});
				localisation_definitions.extend(file.entries);
				localisation_duplicates.extend(file.duplicates);
				parse_issues.extend(file.parse_issues);
			}
			ParsedTextDocument::Csv(file) => {
				document_records.push(DocumentRecord {
					mod_id: file.mod_id,
					path: file.path,
					family: DocumentFamily::Csv,
					parse_ok: file.parse_issues.is_empty(),
				});
				csv_rows.extend(file.rows);
				parse_issues.extend(file.parse_issues);
			}
			ParsedTextDocument::Json(file) => {
				document_records.push(DocumentRecord {
					mod_id: file.mod_id,
					path: file.path,
					family: DocumentFamily::Json,
					parse_ok: file.parse_issues.is_empty(),
				});
				json_properties.extend(file.properties);
				parse_issues.extend(file.parse_issues);
			}
		}
	}

	let mut index = build_semantic_index(&clausewitz_docs);
	index.documents.extend(document_records);
	index
		.localisation_definitions
		.extend(localisation_definitions);
	index
		.localisation_duplicates
		.extend(localisation_duplicates);
	index.csv_rows.extend(csv_rows);
	index.json_properties.extend(json_properties);
	index.parse_issues.extend(parse_issues);
	sort_and_dedup_document_records(&mut index.documents);

	index
}

fn sort_and_dedup_document_records(documents: &mut Vec<DocumentRecord>) {
	documents.sort_by(|lhs, rhs| {
		(lhs.path.clone(), lhs.mod_id.clone()).cmp(&(rhs.path.clone(), rhs.mod_id.clone()))
	});
	documents.dedup_by(|lhs, rhs| {
		lhs.path == rhs.path
			&& lhs.mod_id == rhs.mod_id
			&& lhs.family == rhs.family
			&& lhs.parse_ok == rhs.parse_ok
	});
}

/// Classify one relative path into a supported text-document parser family.
pub fn classify_document_family(relative_path: &Path) -> Option<DocumentFamily> {
	let ext = relative_path
		.extension()
		.and_then(|value| value.to_str())
		.map(|value| value.to_ascii_lowercase())?;

	match ext.as_str() {
		"txt" | "gui" | "gfx" | "asset" => Some(DocumentFamily::Clausewitz),
		"mod" => Some(DocumentFamily::Clausewitz),
		"lua" if is_clausewitz_defines_path(relative_path) => Some(DocumentFamily::Clausewitz),
		"yml" | "yaml" => Some(DocumentFamily::Localisation),
		"csv" => Some(DocumentFamily::Csv),
		"json" => Some(DocumentFamily::Json),
		_ => None,
	}
}

pub fn is_clausewitz_defines_path(relative_path: &Path) -> bool {
	let normalized = relative_path
		.to_string_lossy()
		.replace('\\', "/")
		.to_ascii_lowercase();
	normalized == "common/defines.lua" || normalized.starts_with("common/defines/")
}

fn parse_text_document_with(
	mod_id: &str,
	root: &Path,
	doc: &DiscoveredTextDocument,
	parse_script: fn(&str, &Path, &Path) -> Option<ParsedScriptWithInputIdentity>,
) -> Option<(ParsedTextDocument, Option<ParsedDocumentInputIdentity>)> {
	match doc.family {
		DocumentFamily::Clausewitz => {
			parse_script(mod_id, root, &doc.absolute_path).map(|parsed| {
				let input_identity =
					parsed
						.input_identity
						.map(|identity| ParsedDocumentInputIdentity {
							relative_path: doc.relative_path.clone(),
							size_bytes: identity.size_bytes,
							content_digest: identity.content_digest,
						});
				(ParsedTextDocument::Clausewitz(parsed.file), input_identity)
			})
		}
		DocumentFamily::Localisation => Some((
			ParsedTextDocument::Localisation(parse_localisation_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		)),
		DocumentFamily::Csv => Some((
			ParsedTextDocument::Csv(parse_csv_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		)),
		DocumentFamily::Json => Some((
			ParsedTextDocument::Json(parse_json_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		)),
	}
}

fn parse_localisation_document(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &Path,
) -> ParsedLocalisationDocument {
	let parsed = parse_localisation_file(mod_id, absolute_path, relative_path);
	ParsedLocalisationDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_path_buf(),
		entries: parsed
			.entries
			.iter()
			.map(|item| item.definition.clone())
			.collect(),
		duplicates: parsed.duplicates,
		parse_issues: parsed.parse_issues,
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
	foch::game::eu4::text::decode_paradox_bytes(raw).into_owned()
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
	for prefix in [
		"licenses/",
		"patchnotes/",
		"ebook/",
		"legal_notes/",
		"builtin_dlc/",
		"dlc_metadata/",
		"hints/",
	] {
		if normalized.starts_with(prefix) {
			return true;
		}
	}
	let file_name = relative_path
		.file_name()
		.and_then(|value| value.to_str())
		.map(|value| value.to_ascii_lowercase());
	if file_name.as_deref().is_some_and(|value| {
		matches!(
			value,
			"steam.txt"
				| "描述.txt" | "thirdpartylicenses.txt"
				| "checksum_manifest.txt"
				| "clausewitz_branch.txt"
				| "clausewitz_rev.txt"
				| "eu4_branch.txt"
				| "eu4_rev.txt"
				| "launcher-settings.json"
				| "settings-layout.json"
		)
	}) {
		return true;
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

#[cfg(test)]
mod tests {
	use super::{
		build_semantic_index_from_documents, build_semantic_index_from_owned_documents,
		classify_document_family, discover_text_documents, parse_csv_document,
		parse_discovered_text_documents, parse_localisation_document,
	};
	use foch::model::DocumentFamily;
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
			classify_document_family(Path::new("common/defines/00_test.lua")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(Path::new("common/defines.lua")),
			Some(DocumentFamily::Clausewitz)
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
		fs::create_dir_all(tmp.path().join("builtin_dlc")).expect("create builtin dlc");
		fs::create_dir_all(tmp.path().join("dlc_metadata")).expect("create dlc metadata");
		fs::create_dir_all(tmp.path().join("hints")).expect("create hints");
		fs::create_dir_all(tmp.path().join("events")).expect("create events");
		fs::write(tmp.path().join("licenses").join("LUA.txt"), "license").expect("write license");
		fs::write(tmp.path().join("patchnotes").join("1.0.txt"), "patchnotes")
			.expect("write patchnotes");
		fs::write(
			tmp.path().join("builtin_dlc").join("builtin_dlc.txt"),
			"dlc",
		)
		.expect("write builtin dlc");
		fs::write(
			tmp.path().join("dlc_metadata").join("metadata.txt"),
			"metadata",
		)
		.expect("write dlc metadata");
		fs::write(tmp.path().join("hints").join("tips.txt"), "hint").expect("write hints");
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
	fn discovery_excludes_known_description_text_files() {
		let tmp = TempDir::new().expect("temp dir");
		fs::create_dir_all(tmp.path().join("events")).expect("create events");
		fs::write(tmp.path().join("steam.txt"), "steam bbcode").expect("write steam");
		fs::write(tmp.path().join("描述.txt"), "mod description").expect("write desc");
		fs::write(
			tmp.path().join("ThirdPartyLicenses.txt"),
			"third party licenses",
		)
		.expect("write third-party licenses");
		fs::write(tmp.path().join("checksum_manifest.txt"), "checksums")
			.expect("write checksum manifest");
		fs::write(tmp.path().join("clausewitz_branch.txt"), "branch")
			.expect("write clausewitz branch");
		fs::write(tmp.path().join("clausewitz_rev.txt"), "rev").expect("write clausewitz rev");
		fs::write(tmp.path().join("eu4_branch.txt"), "branch").expect("write eu4 branch");
		fs::write(tmp.path().join("eu4_rev.txt"), "rev").expect("write eu4 rev");
		fs::write(
			tmp.path().join("launcher-settings.json"),
			"{\"launcher\":true}",
		)
		.expect("write launcher settings");
		fs::write(tmp.path().join("settings-layout.json"), "{\"layout\":true}")
			.expect("write settings layout");
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

	#[test]
	fn borrowed_and_owned_semantic_index_builders_are_equivalent() {
		let tmp = TempDir::new().expect("temp dir");
		fs::create_dir_all(tmp.path().join("events")).expect("create events dir");
		fs::create_dir_all(tmp.path().join("localisation")).expect("create localisation dir");
		fs::create_dir_all(tmp.path().join("common")).expect("create common dir");
		fs::write(
			tmp.path().join("events").join("test.txt"),
			"namespace = test\ncountry_event = { id = test.1 }\n",
		)
		.expect("write script");
		fs::write(
			tmp.path().join("localisation").join("test_l_english.yml"),
			"l_english:\ntest.key:0 \"Test\"\n",
		)
		.expect("write localisation");
		fs::write(
			tmp.path().join("common").join("data.csv"),
			"key;value\nalpha;1\n",
		)
		.expect("write csv");
		fs::write(
			tmp.path().join("common").join("settings.json"),
			"{\"feature\":{\"enabled\":true}}\n",
		)
		.expect("write json");

		let discovered = discover_text_documents(tmp.path());
		let mut batch = parse_discovered_text_documents("mod-a", tmp.path(), &discovered);
		let duplicate_clausewitz = batch
			.documents
			.iter()
			.find(|document| matches!(document, super::ParsedTextDocument::Clausewitz(_)))
			.expect("clausewitz document")
			.clone();
		batch.documents.push(duplicate_clausewitz);

		let borrowed = build_semantic_index_from_documents(&batch.documents);
		let owned = build_semantic_index_from_owned_documents(batch.documents);
		let borrowed_json = serde_json::to_value(&borrowed).expect("serialize borrowed index");
		let owned_json = serde_json::to_value(&owned).expect("serialize owned index");

		assert_eq!(owned_json, borrowed_json);
		assert_eq!(
			owned.documents.len(),
			4,
			"duplicate record behavior changed"
		);
	}
}
