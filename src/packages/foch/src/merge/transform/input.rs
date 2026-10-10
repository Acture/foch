//! Source-bound edits and drift validation, independent of a content family.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::game::eu4::script::parse_cache::parse_clausewitz_for_path;
use crate::game::eu4::script::parser::{AstFile, repair_text_edits};
use crate::game::eu4::script::{ParsedScriptFile, parse_script_bytes_cached};
use crate::game::eu4::text::{decode_paradox_bytes, reencode_paradox_bytes};
use crate::input::ResolvedInput;
use crate::merge::error::{MergeError, MergeErrorSubject};
use crate::model::{GamePath, GamePathBuf};
use crate::project::SourceEdit;

#[derive(Clone, Debug, Default)]
pub(crate) struct SourceGuard(BTreeMap<PathBuf, String>);

impl SourceGuard {
	pub(crate) fn validate(&self) -> Result<(), MergeError> {
		for (path, expected) in &self.0 {
			let current = fs::read(path).map(|bytes| sha256(&bytes));
			if current.as_ref().ok() != Some(expected) {
				return Err(invalid(
					host(path),
					"reviewed source changed after analysis; analyze and review the updated input again",
				));
			}
		}
		Ok(())
	}

	/// Bind a file a transformation read, in any format, to the bytes analysis
	/// saw. Commit rejects the transformation if the file changes.
	pub(crate) fn bind(&mut self, path: PathBuf, bytes: &[u8]) -> Result<(), MergeError> {
		let digest = sha256(bytes);
		if self
			.0
			.get(&path)
			.is_some_and(|previous| previous != &digest)
		{
			return Err(invalid(
				host(&path),
				"transformations bind incompatible versions of the same source",
			));
		}
		self.0.insert(path, digest);
		Ok(())
	}

	pub(super) fn extend(&mut self, other: &Self) -> Result<(), MergeError> {
		for (path, digest) in &other.0 {
			if self.0.get(path).is_some_and(|previous| previous != digest) {
				return Err(invalid(
					host(path),
					"transformations bind incompatible versions of the same source",
				));
			}
		}
		self.0.extend(other.0.clone());
		Ok(())
	}
}

#[derive(Clone, Copy)]
pub(crate) struct SourceBinding<'a> {
	pub mod_id: &'a str,
	pub file: &'a str,
	pub sha256: &'a str,
}

pub(crate) struct SourceRepair<'a> {
	pub source: SourceBinding<'a>,
	pub edits: &'a [SourceEdit],
}

type SourceValidation = fn(&GamePath, &AstFile) -> Result<(), String>;

/// Borrowed policy for the original-source phase, before semantic adapters run.
#[derive(Default)]
pub(crate) struct ReviewedInputPolicy<'a> {
	pub label: &'static str,
	pub sources: Vec<SourceBinding<'a>>,
	pub repairs: Vec<SourceRepair<'a>>,
	pub validate: Option<SourceValidation>,
}

#[derive(Default)]
pub(crate) struct ReviewedInputs {
	pub source_guard: SourceGuard,
	pub evidence: Vec<String>,
}

pub(crate) struct ReviewedInputBatch {
	inputs: Vec<ReviewedInputs>,
	overlays: Vec<(ParsedScriptFile, Vec<u8>)>,
}

impl ReviewedInputBatch {
	/// Compose all original-bound edits before parsing or publishing overlays.
	/// Every participating adapter validates the resulting shared document.
	pub(crate) fn prepare(
		input: &ResolvedInput,
		policies: &[ReviewedInputPolicy<'_>],
	) -> Result<Self, MergeError> {
		let mut result = Self {
			inputs: policies.iter().map(|_| ReviewedInputs::default()).collect(),
			overlays: vec![],
		};
		let mut originals = BTreeMap::new();
		let mut edits: BTreeMap<(&str, &str), Vec<SourceEdit>> = BTreeMap::new();
		let mut owners: BTreeMap<(&str, &str), std::collections::BTreeSet<usize>> = BTreeMap::new();
		for (index, policy) in policies.iter().enumerate() {
			let label = policy.label;
			for binding in policy
				.sources
				.iter()
				.chain(policy.repairs.iter().map(|repair| &repair.source))
			{
				let SourceBinding {
					mod_id,
					file,
					sha256: expected,
				} = *binding;
				let key = (mod_id, file);
				if let std::collections::btree_map::Entry::Vacant(entry) = originals.entry(key) {
					let game_file = GamePathBuf::parse(file).map_err(|error| {
						invalid(
							MergeErrorSubject::Named(format!("{mod_id}:{file}")),
							format!("reviewed {label} source is not a game path: {error}"),
						)
					})?;
					let contributor = input
						.file_inventory
						.get(&game_file)
						.and_then(|contributors| {
							contributors.iter().find(|contributor| {
								contributor.mod_id == mod_id && !contributor.is_synthetic_base
							})
						})
						.ok_or_else(|| {
							invalid(
								game(&game_file),
								format!(
									"reviewed {label} source `{mod_id}:{file}` is not an enabled input"
								),
							)
						})?;
					let bytes = fs::read(contributor.absolute_path()).map_err(|error| {
						invalid(
							game(&game_file),
							format!("cannot read reviewed source: {error}"),
						)
					})?;
					let observed = sha256(&bytes);
					input
						.script_cache
						.load(contributor)
						.map_err(|error| invalid(game(&game_file), error))?;
					entry.insert((contributor, game_file, bytes, observed));
				}
				let (contributor, game_file, _, observed) = &originals[&key];
				if observed != expected {
					return Err(invalid(
						game(game_file),
						format!(
							"stale {label} decision for `{mod_id}:{file}`: expected SHA256 {expected}, observed {observed}"
						),
					));
				}
				result.inputs[index]
					.source_guard
					.0
					.insert(contributor.absolute_path(), observed.clone());
				owners.entry(key).or_default().insert(index);
			}
			for repair in &policy.repairs {
				let source = repair.source;
				let combined = edits.entry((source.mod_id, source.file)).or_default();
				for edit in repair.edits {
					// Two adapters can agree on one repair; apply it once.
					if !combined.contains(edit) {
						combined.push(edit.clone());
					}
				}
				result.inputs[index].evidence.push(format!(
					"accepted {label} repair: {}:{} SHA256 {} ({} edits)",
					source.mod_id,
					source.file,
					source.sha256,
					repair.edits.len()
				));
			}
		}
		for (key @ (mod_id, _), edits) in edits {
			let (contributor, file, bytes, _) = &originals[&key];
			let repaired = repair_text(&decode_paradox_bytes(bytes), &edits)
				.map_err(|error| invalid(game(file), error))?;
			let document = parse_script_bytes_cached(
				mod_id,
				&contributor.root_path,
				file,
				repaired.as_bytes(),
			);
			let reviewed_lines = reviewed_definition_lines(&document.ast.statements, &edits);
			for index in &owners[&key] {
				let policy = &policies[*index];
				// A reviewed repair is the exact text that was approved, so the
				// definitions it touches must parse on their own: an automatic
				// repair there would change what was reviewed. Elsewhere in the
				// file the usual automatic repairs still apply.
				let unreviewed = document.parse_issues.iter().any(|issue| {
					issue.repair.is_none()
						|| reviewed_lines
							.iter()
							.any(|lines| lines.contains(&issue.line))
				});
				if unreviewed {
					return Err(invalid(
						game(file),
						format!(
							"{} repair still has parse errors: {:?}",
							policy.label, document.parse_issues
						),
					));
				}
				if let Some(validate) = policy.validate {
					validate(file, &document.ast)
						.map_err(|message| invalid(game(file), message))?;
				}
			}
			let written = written_bytes(file, bytes, &repaired)
				.map_err(|message| invalid(game(file), message))?;
			result.overlays.push((document, written));
		}
		Ok(result)
	}

	pub(crate) fn apply(self, input: &mut ResolvedInput) -> Vec<ReviewedInputs> {
		for (document, bytes) in self.overlays {
			input.script_cache.insert_overlay(document, bytes);
		}
		self.inputs
	}
}

/// The line ranges of the top-level definitions that `edits`, already made,
/// changed in the text `statements` were parsed from.
pub(crate) fn reviewed_definition_lines(
	statements: &[crate::game::eu4::script::parser::AstStatement],
	edits: &[SourceEdit],
) -> Vec<std::ops::RangeInclusive<usize>> {
	use crate::game::eu4::script::parser::AstStatement;

	let mut sorted = edits.iter().collect::<Vec<_>>();
	sorted.sort_by_key(|edit| edit.start);
	let mut shift = 0isize;
	let mut ranges = Vec::new();
	for edit in sorted {
		let start = edit.start.saturating_add_signed(shift);
		let end = start + edit.replacement.len();
		shift += edit.replacement.len() as isize - (edit.end - edit.start) as isize;
		for statement in statements {
			let span = match statement {
				AstStatement::Assignment { span, .. } | AstStatement::Item { span, .. } => span,
				AstStatement::Comment { .. } => continue,
			};
			if span.start.offset <= end && start <= span.end.offset {
				ranges.push(span.start.line..=span.end.line);
			}
		}
	}
	ranges
}

pub(crate) fn repair_text(source: &str, edits: &[SourceEdit]) -> Result<String, String> {
	let mut sorted = edits.iter().collect::<Vec<_>>();
	sorted.sort_by_key(|edit| (edit.start, edit.end));
	for pair in sorted.windows(2) {
		if pair[0].end > pair[1].start || pair[0].start == pair[1].start {
			return Err("repair edits overlap in the original source".into());
		}
	}
	for edit in &sorted {
		if source.get(edit.start..edit.end) != Some(edit.expected.as_str()) {
			return Err(format!(
				"repair at UTF-8 range {}..{} does not match its expected original text",
				edit.start, edit.end
			));
		}
	}
	let mut result = source.to_owned();
	for edit in sorted.into_iter().rev() {
		result.replace_range(edit.start..edit.end, &edit.replacement);
	}
	Ok(result)
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
	format!("{:x}", Sha256::digest(bytes))
}

/// The bytes a reviewed file is copied out as: the reviewed text with the
/// automatic repairs the merge read it with elsewhere, in the encoding of
/// the `source` bytes, so the file written reads as the analysis did.
fn written_bytes(file: &GamePath, source: &[u8], reviewed: &str) -> Result<Vec<u8>, String> {
	let parsed = parse_clausewitz_for_path(file, reviewed);
	let mut edits = repair_text_edits(reviewed, &parsed.diagnostics)
		.ok_or("its automatic repairs cannot be placed in the reviewed text")?;
	edits.sort_by_key(|edit| std::cmp::Reverse(edit.offset));
	let mut text = reviewed.to_owned();
	for edit in edits {
		text.replace_range(edit.offset..edit.offset + edit.remove, edit.insert);
	}
	reencode_paradox_bytes(source, &text)
		.ok_or_else(|| "the repaired file cannot be written back in its own encoding".to_owned())
}

fn invalid(subject: MergeErrorSubject, message: impl Into<String>) -> MergeError {
	MergeError::Validation {
		subject: Some(subject),
		message: message.into(),
	}
}

fn host(path: &Path) -> MergeErrorSubject {
	MergeErrorSubject::Host(path.to_path_buf())
}

fn game(path: &GamePath) -> MergeErrorSubject {
	MergeErrorSubject::Game(path.to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_reviewed_file_is_written_with_its_automatic_repairs_in_its_encoding() {
		// Windows-1252, as EU4 scripts usually are: `é` is one byte.
		let source = b"a = { name = \"Caf\xe9\" }\n}\nb = { always = yes }\nc = 1\n";
		let reviewed = decode_paradox_bytes(source).replace("c = 1", "c = 2");
		let path = GamePath::new("common/scripted_triggers/x.txt").unwrap();
		let written = written_bytes(path, source, &reviewed).unwrap();
		assert_eq!(
			written,
			b"a = { name = \"Caf\xe9\" }\n\nb = { always = yes }\nc = 2\n",
			"{}",
			String::from_utf8_lossy(&written)
		);
	}

	#[test]
	fn adapters_compose_disjoint_source_repairs_and_reject_conflicting_edits() {
		use crate::game::eu4::Eu4;
		use crate::input::ResolvedInputContributor;
		use crate::playset::Playset;
		use std::collections::BTreeSet;
		let temp = tempfile::tempdir().unwrap();
		let relative = "history/provinces/1.txt";
		let path = temp.path().join(relative);
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		let text = "base_tax = 1\nbase_production = 2\n";
		fs::write(&path, text).unwrap();
		let contributor = ResolvedInputContributor {
			mod_id: "mod".into(),
			root_path: temp.path().into(),
			relative_path: crate::model::GamePathBuf::parse(relative).expect("valid game path"),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: None,
		};
		let mut input = ResolvedInput {
			playlist_path: temp.path().join("playlist.json"),
			playlist: Playset {
				game: Eu4,
				name: "repairs".into(),
				mods: vec![],
			},
			mods: vec![],
			installed_base_snapshot: None,
			cache_game_version: None,
			game_version: None,
			mod_snapshots: vec![],
			script_cache: Default::default(),
			file_inventory: BTreeMap::from([(
				contributor.relative_path.clone(),
				vec![contributor.clone()],
			)]),
			verified_absent_base_paths: BTreeSet::new(),
			requested_retained_paths: None,
			effective_retained_paths: None,
		};
		input.script_cache.insert_overlay(
			parse_script_bytes_cached(
				"mod",
				temp.path(),
				&contributor.relative_path,
				text.as_bytes(),
			),
			text.as_bytes().to_vec(),
		);
		let digest = sha256(text.as_bytes());
		let source = SourceBinding {
			mod_id: "mod",
			file: relative,
			sha256: &digest,
		};
		let left = [edit(text, "1", "3")];
		let right = [edit(text, "2", "4")];
		let policies = [
			ReviewedInputPolicy {
				label: "tax",
				repairs: vec![SourceRepair {
					source,
					edits: &left,
				}],
				validate: Some(|_, ast| {
					if ast.statements.len() == 2 {
						Ok(())
					} else {
						Err("both fields required".into())
					}
				}),
				..Default::default()
			},
			ReviewedInputPolicy {
				label: "production",
				repairs: vec![SourceRepair {
					source,
					edits: &right,
				}],
				..Default::default()
			},
		];
		let decisions = ReviewedInputBatch::prepare(&input, &policies)
			.unwrap()
			.apply(&mut input);
		assert_eq!(
			input
				.script_cache
				.overlay_bytes("mod", &contributor.relative_path)
				.unwrap(),
			b"base_tax = 3\nbase_production = 4\n"
		);
		assert_eq!(decisions.len(), 2);
		assert!(
			decisions
				.iter()
				.all(|decision| decision.source_guard.validate().is_ok())
		);
		assert_eq!(fs::read_to_string(&path).unwrap(), text);
		let conflicting = [edit(text, "1", "5")];
		let conflicts = [
			ReviewedInputPolicy {
				label: "one",
				repairs: vec![SourceRepair {
					source,
					edits: &left,
				}],
				..Default::default()
			},
			ReviewedInputPolicy {
				label: "two",
				repairs: vec![SourceRepair {
					source,
					edits: &conflicting,
				}],
				..Default::default()
			},
		];
		assert!(
			matches!(ReviewedInputBatch::prepare(&input, &conflicts), Err(error) if error.to_string().contains("overlap"))
		);
		assert_eq!(
			input
				.script_cache
				.overlay_bytes("mod", &contributor.relative_path)
				.unwrap(),
			b"base_tax = 3\nbase_production = 4\n"
		);
	}
	fn edit(source: &str, expected: &str, replacement: &str) -> SourceEdit {
		let start = source.find(expected).unwrap();
		SourceEdit {
			start,
			end: start + expected.len(),
			expected: expected.into(),
			replacement: replacement.into(),
		}
	}

	#[test]
	fn repair_ranges_reject_overlaps_stale_text_and_split_unicode() {
		let source = "名称 g = { }";
		let valid = edit(source, "g", "renamed");
		assert!(repair_text(source, &[valid.clone(), valid.clone()]).is_err());
		assert!(repair_text("changed", &[valid]).is_err());
		assert!(
			repair_text(
				source,
				&[SourceEdit {
					start: 1,
					end: 2,
					expected: "".into(),
					replacement: "".into()
				}]
			)
			.is_err()
		);
	}

	#[test]
	fn source_guard_rejects_changed_file_bytes() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("source.txt");
		fs::write(&path, "before").unwrap();
		let guard = SourceGuard(BTreeMap::from([(path.clone(), sha256(b"before"))]));
		assert!(guard.validate().is_ok());
		fs::write(&path, "after").unwrap();
		assert!(guard.validate().is_err());
	}

	#[test]
	fn composed_guards_validate_each_source_and_reject_conflicting_versions() {
		let temp = tempfile::tempdir().unwrap();
		let left = temp.path().join("left.txt");
		let right = temp.path().join("right.txt");
		fs::write(&left, "left").unwrap();
		fs::write(&right, "right").unwrap();
		let mut combined = SourceGuard(BTreeMap::from([(left.clone(), sha256(b"left"))]));
		combined
			.extend(&SourceGuard(BTreeMap::from([(
				right.clone(),
				sha256(b"right"),
			)])))
			.unwrap();
		assert!(combined.validate().is_ok());
		assert!(
			combined
				.extend(&SourceGuard(BTreeMap::from([(left, sha256(b"other"))])))
				.is_err()
		);
		assert!(combined.validate().is_ok());
		fs::write(right, "changed").unwrap();
		assert!(combined.validate().is_err());
	}
}
