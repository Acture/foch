//! Read-only editor hover for merged EU4 scripts.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::position::{byte_offset, range_from_span};
use super::schema::{EditorPosition, EditorSchema, SchemaHover, SchemaWorkspace};
use crate::game::eu4::content::{MergeKeySource, eu4};
use crate::game::eu4::script::parser::{
	AstStatement, AstValue, ScriptSyntax, parse_clausewitz_statements,
};
use crate::game::eu4::text::decode_paradox_bytes;
use crate::model::{
	GamePath, GamePathBuf, MERGE_PROVENANCE_ARTIFACT_PATH, MergeProvenanceArtifact,
};

// Keep a damaged or unrelated sidecar from allocating unbounded hover memory.
const MAX_SIDECAR_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCRIPT_BYTES: u64 = 32 * 1024 * 1024;

type DefinitionSources = BTreeMap<GamePathBuf, BTreeMap<String, Vec<String>>>;

#[derive(Deserialize)]
#[serde(untagged)]
enum SourcesArtifact {
	Verified(MergeProvenanceArtifact),
	Legacy(DefinitionSources),
}

/// Combine recorded merge sources with the schema help available at the cursor.
///
/// Reads metadata afresh on each call; callers on an async runtime should run
/// this on a blocking worker. Versioned sidecars bind the record to the emitted
/// bytes. Legacy sidecars lack fingerprints and are explicitly historical.
pub fn document_hover(
	file_path: &Path,
	schema_path: Option<&GamePath>,
	text: &str,
	position: EditorPosition,
	schema: Option<&EditorSchema>,
	workspace: Option<&SchemaWorkspace>,
) -> Option<SchemaHover> {
	let location = output_location(file_path);
	let schema_help = schema
		.zip(schema_path)
		.and_then(|(schema, relative)| schema.hover(relative, text, position, workspace))
		.or_else(|| {
			schema
				.zip(location.as_ref())
				.and_then(|(schema, location)| {
					schema.hover(&location.relative, text, position, workspace)
				})
		});
	let sources = location
		.as_ref()
		.and_then(|location| provenance_hover(location, text, position));
	match (schema_help, sources) {
		(Some(mut help), Some(sources)) => {
			help.markdown.push_str("\n\n---\n\n");
			help.markdown.push_str(&sources.markdown);
			Some(help)
		}
		(help, sources) => help.or(sources),
	}
}

struct OutputLocation {
	root: PathBuf,
	file: PathBuf,
	relative: GamePathBuf,
}

fn output_location(file_path: &Path) -> Option<OutputLocation> {
	// Select the URI's boundary before resolving links. Otherwise a junction
	// can silently switch to another output's metadata or another file's key.
	for root in file_path.parent()?.ancestors() {
		// A missing/broken sidecar in an inner output must never fall through to
		// the sources of an enclosing output. A mod descriptor is also a boundary.
		if fs::symlink_metadata(root.join(".foch")).is_ok()
			|| fs::symlink_metadata(root.join("descriptor.mod")).is_ok()
		{
			let relative = file_path.strip_prefix(root).ok()?.to_path_buf();
			let root = fs::canonicalize(root).ok()?;
			let file = fs::canonicalize(file_path).ok()?;
			if file.strip_prefix(&root).ok()? != relative {
				return None;
			}
			let relative: GamePathBuf = GamePathBuf::from_physical(&root, &file).ok()?;
			return Some(OutputLocation {
				root,
				relative,
				file,
			});
		}
	}
	None
}

fn provenance_hover(
	location: &OutputLocation,
	text: &str,
	position: EditorPosition,
) -> Option<SchemaHover> {
	let descriptor = eu4().classify_content_family(&location.relative)?;
	if descriptor.merge_key_source != Some(MergeKeySource::AssignmentKey) {
		return None;
	}
	let offset = byte_offset(text, position)?;
	let parsed = parse_clausewitz_statements(ScriptSyntax::for_game_path(&location.relative), text);
	if !parsed.diagnostics.is_empty() || !complete_blocks(&parsed.statements, text) {
		return None;
	}
	let (key, span) = parsed
		.statements
		.iter()
		.find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value: AstValue::Block { .. },
				..
			} if (key_span.start.offset..key_span.end.offset).contains(&offset) => Some((key, key_span)),
			_ => None,
		})?;
	if parsed
		.statements
		.iter()
		.filter(
			|statement| matches!(statement, AstStatement::Assignment { key: other, .. } if other == key),
		)
		.count()
		!= 1
	{
		return None;
	}
	let sidecar_path = fs::canonicalize(location.root.join(MERGE_PROVENANCE_ARTIFACT_PATH)).ok()?;
	if !sidecar_path.starts_with(&location.root) {
		return None;
	}
	let sidecar = File::open(sidecar_path).ok()?;
	let sidecar_metadata = sidecar.metadata().ok()?;
	if !sidecar_metadata.is_file() || sidecar_metadata.len() > MAX_SIDECAR_BYTES {
		return None;
	}
	let script = File::open(&location.file).ok()?;
	let script_metadata = script.metadata().ok()?;
	if !script_metadata.is_file() || script_metadata.len() > MAX_SCRIPT_BYTES {
		return None;
	}
	let mut disk_bytes = Vec::new();
	script
		.take(MAX_SCRIPT_BYTES + 1)
		.read_to_end(&mut disk_bytes)
		.ok()?;
	if disk_bytes.len() as u64 > MAX_SCRIPT_BYTES || decode_paradox_bytes(&disk_bytes) != text {
		return None;
	}
	let mut bytes = Vec::new();
	sidecar
		.take(MAX_SIDECAR_BYTES + 1)
		.read_to_end(&mut bytes)
		.ok()?;
	if bytes.len() as u64 > MAX_SIDECAR_BYTES {
		return None;
	}
	let sources: SourcesArtifact = serde_json::from_slice(&bytes).ok()?;
	let (mods, names) = match &sources {
		SourcesArtifact::Verified(artifact) => {
			if artifact.version != 1 {
				return None;
			}
			let file = artifact.files.get(&location.relative)?;
			if file.content_hash != blake3::hash(&disk_bytes).to_hex().as_str() {
				return None;
			}
			(file.definitions.get(key)?, Some(&artifact.mod_names))
		}
		SourcesArtifact::Legacy(sources) => {
			if script_metadata.modified().ok()? > sidecar_metadata.modified().ok()? {
				return None;
			}
			(sources.get(&location.relative)?.get(key)?, None)
		}
	};
	if mods.is_empty() || mods.iter().any(|source| source.trim().is_empty()) {
		return None;
	}
	let markdown = if let Some(names) = names {
		let contributors = mods
			.iter()
			.map(|id| {
				let id_text = escape_markdown(id);
				match names
					.get(id)
					.filter(|name| !name.trim().is_empty() && *name != id)
				{
					Some(name) => format!("{} ({id_text})", escape_markdown(name)),
					None => id_text,
				}
			})
			.collect::<Vec<_>>()
			.join(", ");
		format!("**Merge sources**\n\nMerged from {contributors}")
	} else {
		format!(
			"**Recorded merge sources**\n\nMod IDs: {}\n\nFrom the last merge; the current file's content is not verified by this record.",
			mods.iter()
				.map(|source| escape_markdown(source))
				.collect::<Vec<_>>()
				.join(", ")
		)
	};
	Some(SchemaHover {
		markdown,
		range: range_from_span(text, span)?,
	})
}

// The tolerant parser can return an unterminated block without a diagnostic.
// Each block must own its closing brace, rather than borrowing its last child's.
fn complete_blocks(statements: &[AstStatement], text: &str) -> bool {
	statements.iter().all(|statement| {
		let value = match statement {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => value,
			AstStatement::Comment { .. } => return true,
		};
		let AstValue::Block { items, span } = value else {
			return true;
		};
		text.as_bytes().get(span.end.offset.saturating_sub(1)) == Some(&b'}')
			&& items.iter().all(|item| {
				let end = match item {
					AstStatement::Assignment { span, .. }
					| AstStatement::Item { span, .. }
					| AstStatement::Comment { span, .. } => span.end.offset,
				};
				end < span.end.offset
			}) && complete_blocks(items, text)
	})
}

fn escape_markdown(value: &str) -> String {
	let mut escaped = String::new();
	for character in value.chars() {
		if character.is_control() {
			escaped.push(' ');
		} else {
			if character.is_ascii_punctuation() {
				escaped.push('\\');
			}
			escaped.push(character);
		}
	}
	escaped
}

#[cfg(test)]
mod tests;
