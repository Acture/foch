use foch_core::model::ParseIssue;
use foch_language::analyzer::content_family::{CwtType, GameProfile};
use foch_language::analyzer::eu4_profile::eu4_profile;
use foch_language::analyzer::parser::{AstFile, AstStatement};
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredParsedScriptFile {
	mod_id: String,
	path: String,
	relative_path: String,
	file_kind: CwtType,
	module_name: String,
	ast: StoredAstFile,
	source: String,
	parse_issues: Vec<StoredParseIssue>,
	parse_cache_hit: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredAstFile {
	path: String,
	statements: Vec<AstStatement>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredParseIssue {
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	message: String,
}

pub(crate) fn encode_parsed_documents(documents: &[ParsedScriptFile]) -> Result<Vec<u8>, String> {
	let stored = documents
		.iter()
		.map(StoredParsedScriptFile::from_parsed_script_file)
		.collect::<Vec<_>>();
	bincode::serialize(&stored).map_err(|err| err.to_string())
}

pub(crate) fn decode_parsed_documents(bytes: &[u8]) -> Result<Vec<ParsedScriptFile>, String> {
	let stored = bincode::deserialize::<Vec<StoredParsedScriptFile>>(bytes)
		.map_err(|err| err.to_string())?;
	Ok(stored
		.into_iter()
		.map(StoredParsedScriptFile::into_parsed_script_file)
		.collect())
}

pub(crate) fn rebase_parsed_documents(root: &Path, documents: &mut [ParsedScriptFile]) {
	for document in documents {
		document.path = root.join(&document.relative_path);
		document.parse_cache_hit = true;
	}
}

impl StoredParsedScriptFile {
	fn from_parsed_script_file(file: &ParsedScriptFile) -> Self {
		Self {
			mod_id: file.mod_id.clone(),
			path: path_to_string(&file.path),
			relative_path: path_to_string(&file.relative_path),
			file_kind: file.file_kind.clone(),
			module_name: file.module_name.clone(),
			ast: StoredAstFile::from_ast_file(&file.ast),
			source: file.source.clone(),
			parse_issues: file
				.parse_issues
				.iter()
				.map(StoredParseIssue::from_parse_issue)
				.collect(),
			parse_cache_hit: true,
		}
	}

	fn into_parsed_script_file(self) -> ParsedScriptFile {
		let relative_path = PathBuf::from(self.relative_path);
		let content_family = eu4_profile().classify_content_family(&relative_path);
		ParsedScriptFile {
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			relative_path,
			content_family,
			file_kind: self.file_kind,
			module_name: self.module_name,
			ast: self.ast.into_ast_file(),
			source: self.source,
			parse_issues: self
				.parse_issues
				.into_iter()
				.map(StoredParseIssue::into_parse_issue)
				.collect(),
			parse_cache_hit: self.parse_cache_hit,
		}
	}
}

impl StoredAstFile {
	fn from_ast_file(file: &AstFile) -> Self {
		Self {
			path: path_to_string(&file.path),
			statements: file.statements.clone(),
		}
	}

	fn into_ast_file(self) -> AstFile {
		AstFile {
			path: PathBuf::from(self.path),
			statements: self.statements,
		}
	}
}

impl StoredParseIssue {
	fn from_parse_issue(item: &ParseIssue) -> Self {
		Self {
			mod_id: item.mod_id.clone(),
			path: path_to_string(&item.path),
			line: item.line,
			column: item.column,
			message: item.message.clone(),
		}
	}

	fn into_parse_issue(self) -> ParseIssue {
		ParseIssue {
			mod_id: self.mod_id,
			path: PathBuf::from(self.path),
			line: self.line,
			column: self.column,
			message: self.message,
		}
	}
}

fn path_to_string(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}
