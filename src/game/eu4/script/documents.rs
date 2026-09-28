use super::localisation::parse_localisation_file;
use super::{
	ParsedScriptFile, ParsedScriptWithInputIdentity, build_semantic_index,
	parse_script_file_with_input_identity, parse_script_file_without_cache_with_input_identity,
};
use crate::model::{
	CsvRow, DocumentFamily, DocumentRecord, FamilyParseStats, GamePath, GamePathBuf, JsonProperty,
	LocalisationDefinition, LocalisationDuplicate, ParseFamilyStats, ParseIssue, SemanticIndex,
};
use rayon::prelude::*;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DiscoveredTextDocument {
	pub absolute_path: PathBuf,
	pub relative_path: GamePathBuf,
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
	pub path: GamePathBuf,
	pub entries: Vec<LocalisationDefinition>,
	pub duplicates: Vec<LocalisationDuplicate>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub struct ParsedCsvDocument {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub rows: Vec<CsvRow>,
	pub parse_issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug)]
pub struct ParsedJsonDocument {
	pub mod_id: String,
	pub path: GamePathBuf,
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
	pub relative_path: GamePathBuf,
	pub size_bytes: u64,
	pub content_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsvSchema {
	Generic,
	Eu4Adjacencies,
	Eu4Definition,
}

/// Selects the text documents among `relative_paths`, the validated inventory
/// of `root` or a subset of it, that exist as files. Each physical path is the
/// game path resolved under `root`.
pub fn discover_text_documents_from_paths(
	root: &Path,
	relative_paths: &[GamePathBuf],
) -> Vec<DiscoveredTextDocument> {
	let mut docs = Vec::new();
	for relative_path in relative_paths {
		if is_excluded_text_path(relative_path) {
			continue;
		}
		let Some(family) = classify_document_family(relative_path) else {
			continue;
		};
		let absolute_path = relative_path.to_path(root);
		if !absolute_path.is_file() {
			continue;
		}
		docs.push(DiscoveredTextDocument {
			absolute_path,
			relative_path: relative_path.clone(),
			family,
		});
	}
	// Component order, as documents have always been parsed and indexed in:
	// scope ids and persisted record order follow it. It differs from byte
	// order when a name sorts below `/`, as `common/defines.lua` does against
	// `common/defines/`.
	docs.sort_by(|lhs, rhs| {
		lhs.relative_path
			.as_relative_path()
			.cmp(rhs.relative_path.as_relative_path())
	});
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
	parse_script: fn(&str, &Path, &GamePath) -> ParsedScriptWithInputIdentity,
) -> ParsedDocumentBatch {
	let parsed: Vec<(ParsedTextDocument, Option<ParsedDocumentInputIdentity>)> = documents
		.par_iter()
		.map(|doc| parse_text_document_with(mod_id, root, doc, parse_script))
		.collect();

	let mut batch = ParsedDocumentBatch::default();
	for (doc, input_identity) in parsed {
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
	// Component order, as in discovery.
	documents.sort_by(|lhs, rhs| {
		(lhs.path.as_relative_path(), lhs.mod_id.as_str())
			.cmp(&(rhs.path.as_relative_path(), rhs.mod_id.as_str()))
	});
	documents.dedup_by(|lhs, rhs| {
		lhs.path == rhs.path
			&& lhs.mod_id == rhs.mod_id
			&& lhs.family == rhs.family
			&& lhs.parse_ok == rhs.parse_ok
	});
}

/// Classify one game path into a supported text-document parser family. The
/// extension matches in any case.
pub fn classify_document_family(relative_path: &GamePath) -> Option<DocumentFamily> {
	let ext = relative_path.extension()?.to_ascii_lowercase();

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

/// `common/defines.lua` or a file inside `common/defines/`, ignoring ASCII
/// case in every component.
pub fn is_clausewitz_defines_path(relative_path: &GamePath) -> bool {
	relative_path
		.as_str()
		.eq_ignore_ascii_case("common/defines.lua")
		|| relative_path.is_inside(&["common", "defines"], str::eq_ignore_ascii_case)
}

fn parse_text_document_with(
	mod_id: &str,
	root: &Path,
	doc: &DiscoveredTextDocument,
	parse_script: fn(&str, &Path, &GamePath) -> ParsedScriptWithInputIdentity,
) -> (ParsedTextDocument, Option<ParsedDocumentInputIdentity>) {
	match doc.family {
		DocumentFamily::Clausewitz => {
			let parsed = parse_script(mod_id, root, &doc.relative_path);
			let input_identity =
				parsed
					.input_identity
					.map(|identity| ParsedDocumentInputIdentity {
						relative_path: doc.relative_path.clone(),
						size_bytes: identity.size_bytes,
						content_digest: identity.content_digest,
					});
			(ParsedTextDocument::Clausewitz(parsed.file), input_identity)
		}
		DocumentFamily::Localisation => (
			ParsedTextDocument::Localisation(parse_localisation_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		),
		DocumentFamily::Csv => (
			ParsedTextDocument::Csv(parse_csv_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		),
		DocumentFamily::Json => (
			ParsedTextDocument::Json(parse_json_document(
				mod_id,
				&doc.absolute_path,
				&doc.relative_path,
			)),
			None,
		),
	}
}

fn parse_localisation_document(
	mod_id: &str,
	absolute_path: &Path,
	relative_path: &GamePath,
) -> ParsedLocalisationDocument {
	let parsed = parse_localisation_file(mod_id, absolute_path, relative_path);
	ParsedLocalisationDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_owned(),
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
	relative_path: &GamePath,
) -> ParsedCsvDocument {
	let mut rows = Vec::new();
	let mut parse_issues = Vec::new();
	let raw = match fs::read(absolute_path) {
		Ok(raw) => raw,
		Err(err) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_owned(),
				line: 1,
				column: 1,
				message: format!("unable to read csv file: {err}"),
			});
			return ParsedCsvDocument {
				mod_id: mod_id.to_string(),
				path: relative_path.to_owned(),
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
				path: relative_path.to_owned(),
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
			path: relative_path.to_owned(),
			line: line_no,
			column: 1,
		});
	}

	ParsedCsvDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_owned(),
		rows,
		parse_issues,
	}
}

fn decode_csv_bytes(raw: &[u8]) -> String {
	crate::game::eu4::text::decode_paradox_bytes(raw).into_owned()
}

fn csv_schema_for(relative_path: &GamePath) -> CsvSchema {
	match relative_path.as_str() {
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
	relative_path: &GamePath,
) -> ParsedJsonDocument {
	let mut properties = Vec::new();
	let mut parse_issues = Vec::new();
	let content = match fs::read_to_string(absolute_path) {
		Ok(content) => content,
		Err(err) => {
			parse_issues.push(ParseIssue {
				mod_id: mod_id.to_string(),
				path: relative_path.to_owned(),
				line: 1,
				column: 1,
				message: format!("unable to read json file: {err}"),
			});
			return ParsedJsonDocument {
				mod_id: mod_id.to_string(),
				path: relative_path.to_owned(),
				properties,
				parse_issues,
			};
		}
	};

	match serde_json::from_str::<JsonValue>(&content) {
		Ok(json) => collect_json_properties(&json, "$", mod_id, relative_path, &mut properties),
		Err(err) => parse_issues.push(ParseIssue {
			mod_id: mod_id.to_string(),
			path: relative_path.to_owned(),
			line: err.line(),
			column: err.column(),
			message: err.to_string(),
		}),
	}

	ParsedJsonDocument {
		mod_id: mod_id.to_string(),
		path: relative_path.to_owned(),
		properties,
		parse_issues,
	}
}

fn collect_json_properties(
	value: &JsonValue,
	base_path: &str,
	mod_id: &str,
	relative_path: &GamePath,
	out: &mut Vec<JsonProperty>,
) {
	match value {
		JsonValue::Object(map) => {
			for (key, child) in map {
				let next = format!("{base_path}.{key}");
				out.push(JsonProperty {
					key_path: next.clone(),
					mod_id: mod_id.to_string(),
					path: relative_path.to_owned(),
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

/// Loadable files the game does not read as text documents: anything inside
/// `dlc_metadata/` or `hints/` (named exactly), and a few metadata files by
/// name (ignoring ASCII case). Inventories come from the input walker, which
/// has already left out every top-level directory that is not a loadable root
/// (`licenses/`, `patchnotes/` and the like) and every file directly under the
/// root.
fn is_excluded_text_path(relative_path: &GamePath) -> bool {
	for directory in ["dlc_metadata", "hints"] {
		if relative_path.is_inside(&[directory], str::eq) {
			return true;
		}
	}
	matches!(
		relative_path.file_name().to_ascii_lowercase().as_str(),
		"steam.txt"
			| "描述.txt"
			| "thirdpartylicenses.txt"
			| "checksum_manifest.txt"
			| "clausewitz_branch.txt"
			| "clausewitz_rev.txt"
			| "eu4_branch.txt"
			| "eu4_rev.txt"
			| "launcher-settings.json"
			| "settings-layout.json"
	)
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
		classify_document_family, discover_text_documents_from_paths, is_excluded_text_path,
		parse_csv_document, parse_discovered_text_documents, parse_localisation_document,
	};
	use crate::game::eu4::Eu4;
	use crate::input::{FileFilter, InventoryOwner, collect_relative_files};
	use crate::model::{DocumentFamily, GamePath, GamePathBuf};
	use std::fs;
	use tempfile::TempDir;

	fn game_path(text: &str) -> &GamePath {
		GamePath::new(text).expect("valid game path")
	}

	/// Writes each file under `root` and returns the root's inventory as the
	/// input walker produces it, which is what discovery is given.
	fn write_inventory(root: &std::path::Path, files: &[(&str, &[u8])]) -> Vec<GamePathBuf> {
		for (relative, contents) in files {
			let physical = GamePath::new(relative)
				.expect("valid game path")
				.to_path(root);
			fs::create_dir_all(physical.parent().expect("parent")).expect("create parent");
			fs::write(&physical, contents).expect("write file");
		}
		collect_relative_files(root, &FileFilter::for_game(Eu4), InventoryOwner::Mod("mod"))
			.expect("walk inventory")
	}

	fn discovered_paths(root: &std::path::Path, inventory: &[GamePathBuf]) -> Vec<String> {
		discover_text_documents_from_paths(root, inventory)
			.into_iter()
			.map(|doc| {
				assert_eq!(doc.absolute_path, doc.relative_path.to_path(root));
				doc.relative_path.into_string()
			})
			.collect()
	}

	#[test]
	fn classify_supported_text_families() {
		assert_eq!(
			classify_document_family(game_path("events/a.txt")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(game_path("interface/a.gui")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(game_path("localisation/test_l_english.yml")),
			Some(DocumentFamily::Localisation)
		);
		assert_eq!(
			classify_document_family(game_path("common/data.csv")),
			Some(DocumentFamily::Csv)
		);
		assert_eq!(
			classify_document_family(game_path("common/settings.json")),
			Some(DocumentFamily::Json)
		);
		assert_eq!(
			classify_document_family(game_path("common/defines/00_test.lua")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(game_path("common/defines.lua")),
			Some(DocumentFamily::Clausewitz)
		);
		assert_eq!(
			classify_document_family(game_path("script/shader.lua")),
			None
		);
	}

	#[test]
	fn defines_paths_match_in_any_case_but_only_under_common() {
		for text in [
			"common/defines.lua",
			"Common/Defines.LUA",
			"common/defines/00_defines.lua",
			"COMMON/DEFINES/nested/x.lua",
		] {
			assert_eq!(
				classify_document_family(game_path(text)),
				Some(DocumentFamily::Clausewitz),
				"{text}"
			);
		}
		for text in [
			"common/defines",
			"events/defines.lua",
			"common/definesx/a.lua",
		] {
			assert_eq!(classify_document_family(game_path(text)), None, "{text}");
		}
	}

	#[test]
	fn discovery_finds_ui_files_but_not_the_descriptor() {
		let tmp = TempDir::new().expect("temp dir");
		let inventory = write_inventory(
			tmp.path(),
			&[
				("descriptor.mod", b"name=\"a\""),
				("interface/main.gui", b"windowType = { }"),
			],
		);

		assert_eq!(
			discovered_paths(tmp.path(), &inventory),
			vec!["interface/main.gui"]
		);
	}

	#[test]
	fn discovery_skips_inventory_paths_that_are_not_files() {
		let tmp = TempDir::new().expect("temp dir");
		let mut inventory =
			write_inventory(tmp.path(), &[("events/real.txt", b"namespace = test")]);
		inventory.push(GamePathBuf::parse("events/missing.txt").expect("valid game path"));

		assert_eq!(
			discovered_paths(tmp.path(), &inventory),
			vec!["events/real.txt"]
		);
	}

	#[test]
	fn discovery_excludes_noise_directories() {
		let tmp = TempDir::new().expect("temp dir");
		let inventory = write_inventory(
			tmp.path(),
			&[
				// Not loadable roots: the walker never lists them.
				("licenses/LUA.txt", b"license"),
				("patchnotes/1.0.txt", b"patchnotes"),
				("ebook/a.txt", b"ebook"),
				("legal_notes/a.txt", b"legal"),
				("builtin_dlc/builtin_dlc.txt", b"dlc"),
				// Loadable roots that are not text documents.
				("dlc_metadata/metadata.txt", b"metadata"),
				("hints/tips.txt", b"hint"),
				("events/real.txt", b"namespace = test"),
			],
		);

		assert_eq!(
			discovered_paths(tmp.path(), &inventory),
			vec!["events/real.txt"]
		);
	}

	#[test]
	fn noise_directories_match_exactly_and_metadata_names_in_any_case() {
		for (text, excluded) in [
			("hints/tips.txt", true),
			("dlc_metadata/metadata.txt", true),
			("Hints/kept.txt", false),
			("common/hints/kept.txt", false),
			("common/steam.txt", true),
			("interface/ThirdPartyLicenses.txt", true),
			("common/描述.txt", true),
			("common/launcher-settings.json", true),
			("common/steam_events.txt", false),
		] {
			assert_eq!(is_excluded_text_path(game_path(text)), excluded, "{text}");
		}
	}

	#[test]
	fn discovery_excludes_known_description_text_files() {
		let tmp = TempDir::new().expect("temp dir");
		let inventory = write_inventory(
			tmp.path(),
			&[
				// Directly under the root: the walker never lists them.
				("steam.txt", b"steam bbcode"),
				("描述.txt", b"mod description"),
				("launcher-settings.json", b"{\"launcher\":true}"),
				// Inside a loadable root: excluded by name.
				("common/steam.txt", b"steam bbcode"),
				("common/描述.txt", b"mod description"),
				("common/ThirdPartyLicenses.txt", b"third party licenses"),
				("common/checksum_manifest.txt", b"checksums"),
				("common/clausewitz_branch.txt", b"branch"),
				("common/clausewitz_rev.txt", b"rev"),
				("common/eu4_branch.txt", b"branch"),
				("common/eu4_rev.txt", b"rev"),
				("common/launcher-settings.json", b"{\"launcher\":true}"),
				("common/settings-layout.json", b"{\"layout\":true}"),
				("events/real.txt", b"namespace = test"),
			],
		);

		assert_eq!(
			discovered_paths(tmp.path(), &inventory),
			vec!["events/real.txt"]
		);
	}

	#[test]
	fn discovery_orders_documents_by_component() {
		let tmp = TempDir::new().expect("temp dir");
		let inventory = write_inventory(
			tmp.path(),
			&[
				("common/defines.lua", b"NDefines = {}"),
				("common/defines/00_defines.lua", b"NDefines = {}"),
				("events/a-b.txt", b"namespace = a"),
				("events/a/b.txt", b"namespace = b"),
			],
		);

		// A directory sorts before a sibling file whose name extends its own
		// with a character below `/`: the order documents were always indexed in.
		assert_eq!(
			discovered_paths(tmp.path(), &inventory),
			vec![
				"common/defines/00_defines.lua",
				"common/defines.lua",
				"events/a/b.txt",
				"events/a-b.txt",
			]
		);
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
			parse_localisation_document("mod", &path, game_path("localisation/test_l_english.yml"));
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
			parse_localisation_document("mod", &path, game_path("localisation/bad_l_english.yml"));
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
			parse_localisation_document("mod", &path, game_path("localisation/languages.yml"));
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
			parse_localisation_document("mod", &path, game_path("localisation/empty_l_german.yml"));
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

		let parsed = parse_csv_document("mod", &path, game_path("map/adjacencies.csv"));
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		assert_eq!(parsed.rows.len(), 2);
	}

	#[test]
	fn csv_parser_decodes_windows_1252_input() {
		let tmp = TempDir::new().expect("temp dir");
		let path = tmp.path().join("common").join("names.csv");
		fs::create_dir_all(path.parent().expect("csv parent")).expect("create csv dir");
		fs::write(&path, b"Name;Value\nMalm\xf6;1\n").expect("write csv");

		let parsed = parse_csv_document("mod", &path, game_path("common/names.csv"));
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

		let parsed = parse_csv_document("mod", &path, game_path("map/definition.csv"));
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

		let parsed = parse_csv_document("mod", &path, game_path("map/definition.csv"));
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

		let inventory = [
			"common/data.csv",
			"common/settings.json",
			"events/test.txt",
			"localisation/test_l_english.yml",
		]
		.map(|text| GamePathBuf::parse(text).expect("valid game path"));
		let discovered = discover_text_documents_from_paths(tmp.path(), &inventory);
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
	/// Document records are listed in component order whatever order the
	/// documents arrive in: it is the order persisted snapshots record them
	/// in, and byte order would reverse both pairs here.
	#[test]
	fn index_documents_are_in_component_order() {
		let tmp = TempDir::new().expect("temp dir");
		let inventory = write_inventory(
			tmp.path(),
			&[
				("common/defines.lua", b"NDefines = {}"),
				("common/defines/00_defines.lua", b"NDefines = {}"),
				("events/a-b.txt", b"namespace = a"),
				("events/a/b.txt", b"namespace = b"),
			],
		);
		let discovered = discover_text_documents_from_paths(tmp.path(), &inventory);
		let mut batch = parse_discovered_text_documents("mod-a", tmp.path(), &discovered);
		batch.documents.reverse();
		let expected = vec![
			"common/defines/00_defines.lua",
			"common/defines.lua",
			"events/a/b.txt",
			"events/a-b.txt",
		];

		let borrowed = build_semantic_index_from_documents(&batch.documents);
		let owned = build_semantic_index_from_owned_documents(batch.documents);
		for index in [borrowed, owned] {
			let paths = index
				.documents
				.iter()
				.map(|document| document.path.as_str())
				.collect::<Vec<_>>();
			assert_eq!(paths, expected);
		}
	}
}
