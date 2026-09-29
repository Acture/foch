//! Codec for parsed Clausewitz documents embedded in base-data snapshots.

use crate::game::eu4::content::ScriptFileKind;
use crate::game::eu4::content::eu4;
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::parser::{AstFile, AstStatement};
use crate::model::{GamePathBuf, ParseIssue};
use std::path::Path;

/// The game paths a document is identified by are written as their canonical
/// text and validated when read, so a corrupt section fails to decode instead
/// of naming a file outside the game's namespace.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredParsedScriptFile {
	mod_id: String,
	/// Never read. Released snapshots record where the building machine read
	/// the file; it is now written empty, which keeps the wire layout without
	/// persisting a machine's file system. Decoding resolves `relative_path`
	/// under the reading machine's game root instead.
	path: String,
	relative_path: GamePathBuf,
	file_kind: ScriptFileKind,
	module_name: String,
	ast: StoredAstFile,
	source: String,
	parse_issues: Vec<StoredParseIssue>,
	parse_cache_hit: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredAstFile {
	/// Never read, so it is not validated: snapshots built before the AST
	/// carried its game path record the builder's absolute file here. It is
	/// written as the game path, and a decoded AST is identified by the
	/// validated `relative_path`.
	path: String,
	statements: Vec<AstStatement>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredParseIssue {
	mod_id: String,
	path: GamePathBuf,
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

/// Decodes the documents of a snapshot and locates each under `root`, the game
/// root of the machine reading it.
pub(crate) fn decode_parsed_documents(
	bytes: &[u8],
	root: &Path,
) -> Result<Vec<ParsedScriptFile>, String> {
	let stored = bincode::deserialize::<Vec<StoredParsedScriptFile>>(bytes)
		.map_err(|err| err.to_string())?;
	Ok(stored
		.into_iter()
		.map(|file| file.into_parsed_script_file(root))
		.collect())
}

impl StoredParsedScriptFile {
	fn from_parsed_script_file(file: &ParsedScriptFile) -> Self {
		Self {
			mod_id: file.mod_id.clone(),
			path: String::new(),
			relative_path: file.relative_path.clone(),
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

	fn into_parsed_script_file(self, root: &Path) -> ParsedScriptFile {
		let content_family = eu4().classify_content_family(&self.relative_path);
		// The AST is identified by the document's game path, so decoded base
		// documents normalize the same way as the merge inputs they are the
		// ancestor of, whatever the stored AST path says.
		let ast = AstFile {
			path: self.relative_path.clone(),
			statements: self.ast.statements,
		};
		ParsedScriptFile {
			mod_id: self.mod_id,
			path: Some(self.relative_path.to_path(root)),
			relative_path: self.relative_path,
			content_family,
			file_kind: self.file_kind,
			module_name: self.module_name,
			ast,
			source: self.source,
			parse_issues: self
				.parse_issues
				.into_iter()
				.map(StoredParseIssue::into_parse_issue)
				.collect(),
			parse_cache_hit: true,
		}
	}
}

impl StoredAstFile {
	fn from_ast_file(file: &AstFile) -> Self {
		Self {
			path: file.path.as_str().to_owned(),
			statements: file.statements.clone(),
		}
	}
}

impl StoredParseIssue {
	fn from_parse_issue(item: &ParseIssue) -> Self {
		Self {
			mod_id: item.mod_id.clone(),
			path: item.path.clone(),
			line: item.line,
			column: item.column,
			message: item.message.clone(),
		}
	}

	fn into_parse_issue(self) -> ParseIssue {
		ParseIssue {
			mod_id: self.mod_id,
			path: self.path,
			line: self.line,
			column: self.column,
			message: self.message,
		}
	}
}

/// The section's wire layout with every path spelled as text, the layout
/// released snapshots were written in. Tests use it to encode sections the
/// typed codec cannot produce: legacy ones and corrupt ones.
#[cfg(test)]
pub(super) mod text_layout {
	use crate::game::eu4::content::ScriptFileKind;
	use crate::game::eu4::script::ParsedScriptFile;
	use crate::game::eu4::script::parser::AstStatement;

	#[derive(Clone, Debug, serde::Serialize)]
	pub struct File {
		pub mod_id: String,
		pub path: String,
		pub relative_path: String,
		pub file_kind: ScriptFileKind,
		pub module_name: String,
		pub ast: Ast,
		pub source: String,
		pub parse_issues: Vec<Issue>,
		pub parse_cache_hit: bool,
	}

	#[derive(Clone, Debug, serde::Serialize)]
	pub struct Ast {
		pub path: String,
		pub statements: Vec<AstStatement>,
	}

	#[derive(Clone, Debug, serde::Serialize)]
	pub struct Issue {
		pub mod_id: String,
		pub path: String,
		pub line: usize,
		pub column: usize,
		pub message: String,
	}

	impl File {
		/// `file` with the values `encode_parsed_documents` writes for it.
		pub fn of(file: &ParsedScriptFile) -> Self {
			Self {
				mod_id: file.mod_id.clone(),
				path: String::new(),
				relative_path: file.relative_path.as_str().to_owned(),
				file_kind: file.file_kind.clone(),
				module_name: file.module_name.clone(),
				ast: Ast {
					path: file.ast.path.as_str().to_owned(),
					statements: file.ast.statements.clone(),
				},
				source: file.source.clone(),
				parse_issues: file
					.parse_issues
					.iter()
					.map(|issue| Issue {
						mod_id: issue.mod_id.clone(),
						path: issue.path.as_str().to_owned(),
						line: issue.line,
						column: issue.column,
						message: issue.message.clone(),
					})
					.collect(),
				parse_cache_hit: true,
			}
		}
	}

	pub fn encode(files: &[File]) -> Vec<u8> {
		bincode::serialize(files).expect("encode text-layout parsed scripts")
	}
}

#[cfg(test)]
mod tests {
	use super::text_layout;
	use super::*;
	use crate::game::eu4::script::parse_script_bytes_cached;
	use crate::model::GamePath;

	/// A parsed document whose source has a parse issue, so every path field
	/// of the section is present.
	fn parsed_with_issue(relative: &str) -> ParsedScriptFile {
		let relative = GamePath::new(relative).expect("valid game path");
		let file = parse_script_bytes_cached(
			"__game__eu4",
			Path::new("/build"),
			relative,
			b"= 1\na = 1\n",
		);
		assert!(
			!file.parse_issues.is_empty(),
			"the fixture must carry a parse issue"
		);
		file
	}

	#[test]
	fn decoded_documents_are_located_under_the_reading_root() {
		let root = Path::new("/reader/game");
		let encoded =
			encode_parsed_documents(&[parsed_with_issue("events/a.txt")]).expect("encode");
		let decoded = decode_parsed_documents(&encoded, root).expect("decode");
		let [document] = decoded.as_slice() else {
			panic!("expected one document");
		};
		assert_eq!(document.relative_path.as_str(), "events/a.txt");
		assert_eq!(document.ast.path, document.relative_path);
		assert_eq!(document.parse_issues[0].path, document.relative_path);
		assert_eq!(
			document.path.as_deref(),
			Some(root.join("events").join("a.txt").as_path())
		);
		assert!(document.parse_cache_hit);
	}

	/// Typing the paths did not change the section's wire layout, so released
	/// snapshots decode and no schema version bump is needed.
	#[test]
	fn the_section_keeps_the_text_layout() {
		let file = parsed_with_issue("events/a.txt");
		assert_eq!(
			encode_parsed_documents(std::slice::from_ref(&file)).expect("encode"),
			text_layout::encode(&[text_layout::File::of(&file)])
		);
	}

	/// Snapshots built before the AST carried its game path record the
	/// builder's absolute file in both unread path fields. Those fields are
	/// not validated, so such a snapshot decodes, and the AST takes the
	/// document's game path.
	#[test]
	fn a_snapshot_that_recorded_the_builders_files_still_decodes() {
		let builder_file = "/builder/machine/Europa Universalis IV/events/a.txt";
		let mut legacy = text_layout::File::of(&parsed_with_issue("events/a.txt"));
		legacy.path = builder_file.to_string();
		legacy.ast.path = builder_file.to_string();
		let root = Path::new("/reader/game");

		let decoded = decode_parsed_documents(&text_layout::encode(&[legacy]), root)
			.expect("a legacy section decodes");
		let [document] = decoded.as_slice() else {
			panic!("expected one document");
		};
		assert_eq!(document.ast.path.as_str(), "events/a.txt");
		assert_eq!(
			document.path.as_deref(),
			Some(root.join("events").join("a.txt").as_path())
		);
	}

	/// Each game path the section is read by is validated on its own.
	#[test]
	fn each_read_game_path_that_is_not_valid_fails_to_decode() {
		let invalid = r"events\a.txt";
		let valid = text_layout::File::of(&parsed_with_issue("events/a.txt"));
		let mut relative_path = valid.clone();
		relative_path.relative_path = invalid.to_string();
		let mut issue_path = valid;
		issue_path.parse_issues[0].path = invalid.to_string();
		for (field, file) in [
			("relative_path", relative_path),
			("parse issue path", issue_path),
		] {
			let error =
				decode_parsed_documents(&text_layout::encode(&[file]), Path::new("/reader/game"))
					.expect_err(field);
			assert!(
				error.contains(&format!("invalid game path `{invalid}`")),
				"{field}: {error}"
			);
		}
	}
}
