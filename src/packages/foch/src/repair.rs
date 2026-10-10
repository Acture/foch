//! Repairs of mod script files outside a merge.
//!
//! A merge only repairs Foch's own copy of a source file. `foch repair`
//! writes the same repairs out for a player: as a patch mod that holds only
//! the repaired files and loads after the mods it repairs, or, on explicit
//! request, into the source files themselves, each backed up first so the
//! change can be undone. The repairs are those a merge would make: automatic
//! one-token repairs with one trustworthy reading, and the reviewed
//! `[[repairs]]` of `foch.toml`.

use crate::game::eu4::base::snapshot::data_root;
use crate::game::eu4::script::parse_cache::parse_clausewitz_for_path;
use crate::game::eu4::script::parser::repair_text_edits;
use crate::game::eu4::text::{TextEdit, decode_paradox_bytes, reencode_paradox_bytes};
use crate::input::request::InputRequest;
use crate::merge::transform::input::{repair_text, reviewed_definition_lines};
use crate::model::{GamePath, GamePathBuf};
use crate::project::{Project, render_repairs_toml};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Every file of the input's mods that has something to repair.
#[derive(Clone, Debug, Default)]
pub struct RepairPlan {
	/// Files whose repaired bytes are known, in mod and path order.
	pub files: Vec<FileRepair>,
	/// Files with errors no repair covers, and why.
	pub unrepaired: Vec<UnrepairedFile>,
}

#[derive(Clone, Debug)]
pub struct FileRepair {
	pub mod_id: String,
	pub path: GamePathBuf,
	/// The source file the repair is for.
	pub source: PathBuf,
	/// What changes, one line each.
	pub changes: Vec<String>,
	pub sha256_before: String,
	pub sha256_after: String,
	pub repaired: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct UnrepairedFile {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub reasons: Vec<String>,
}

#[derive(Debug)]
pub struct RepairError(String);

impl fmt::Display for RepairError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for RepairError {}

impl From<std::io::Error> for RepairError {
	fn from(error: std::io::Error) -> Self {
		Self(error.to_string())
	}
}

fn sha256(bytes: &[u8]) -> String {
	format!("{:x}", Sha256::digest(bytes))
}

/// Finds what can be repaired in the mods `request` resolves to, with the
/// reviewed repairs of `project`. Nothing is written.
pub fn plan_repairs(request: &InputRequest, project: &Project) -> Result<RepairPlan, RepairError> {
	let input = crate::input::resolve_input(request, false)
		.map_err(|error| RepairError(error.to_string()))?;
	let mut targets = BTreeMap::<(String, GamePathBuf), PathBuf>::new();
	for (candidate, snapshot) in input.mods.iter().zip(&input.mod_snapshots) {
		let (Some(root), Some(snapshot)) = (&candidate.root_path, snapshot) else {
			continue;
		};
		for issue in &snapshot.semantic_index.parse_issues {
			targets.insert(
				(candidate.mod_id.clone(), issue.path.clone()),
				issue.path.to_path(root),
			);
		}
		for entry in project
			.repairs
			.iter()
			.filter(|entry| entry.mod_id == candidate.mod_id)
		{
			if let Ok(path) = GamePathBuf::parse(&entry.file) {
				let source = path.to_path(root);
				targets.insert((candidate.mod_id.clone(), path), source);
			}
		}
	}
	let mut plan = RepairPlan::default();
	for ((mod_id, path), source) in targets {
		match plan_file(project, &mod_id, &path, &source)? {
			Planned::Repair(repair) => plan.files.push(repair),
			Planned::Unrepaired(reasons) => plan.unrepaired.push(UnrepairedFile {
				mod_id,
				path,
				reasons,
			}),
			Planned::Nothing => {}
		}
	}
	Ok(plan)
}

enum Planned {
	Repair(FileRepair),
	Unrepaired(Vec<String>),
	Nothing,
}

fn plan_file(
	project: &Project,
	mod_id: &str,
	path: &GamePath,
	source: &Path,
) -> Result<Planned, RepairError> {
	let bytes = fs::read(source)?;
	let sha256_before = sha256(&bytes);
	let mut changes = Vec::new();
	let mut text = decode_paradox_bytes(&bytes).into_owned();
	let reviewed = project
		.repairs
		.iter()
		.find(|entry| entry.mod_id == mod_id && entry.file == path.as_str());
	if let Some(entry) = reviewed {
		if entry.sha256 != sha256_before {
			return Ok(Planned::Unrepaired(vec![format!(
				"the reviewed repair in foch.toml is stale: it was reviewed for SHA256 {}, the file now has {sha256_before}",
				entry.sha256
			)]));
		}
		text = repair_text(&text, &entry.edits).map_err(RepairError)?;
		changes.push(format!(
			"reviewed repair from foch.toml ({} edit(s))",
			entry.edits.len()
		));
	}
	let parsed = parse_clausewitz_for_path(path, &text);
	if let Some(entry) = reviewed {
		let touched = reviewed_definition_lines(&parsed.ast.statements, &entry.edits);
		if parsed.diagnostics.iter().any(|diagnostic| {
			touched
				.iter()
				.any(|lines| lines.contains(&diagnostic.span.start.line))
		}) {
			return Ok(Planned::Unrepaired(vec![
				"the reviewed repair in foch.toml leaves an error in the definitions it changes"
					.into(),
			]));
		}
	}
	let mut reasons = Vec::new();
	for diagnostic in &parsed.diagnostics {
		if let Some(isolation) = &diagnostic.isolation {
			let proposals = isolation
				.proposals
				.iter()
				.map(|proposal| proposal.description())
				.collect::<Vec<_>>();
			let entry = isolation.proposals.first().and_then(|proposal| {
				crate::project::proposed_repair_entry(mod_id, path, &bytes, *proposal)
			});
			reasons.push(format!(
				"definition `{}` at {}-{} has no trustworthy repair{}{}",
				isolation.definition,
				diagnostic.span.start.line,
				isolation.end_line,
				if proposals.is_empty() {
					String::new()
				} else {
					format!("; repairs that could be meant: {}", proposals.join(", "))
				},
				entry.map_or_else(String::new, |entry| format!(
					"; to apply the first after review, add to foch.toml:\n{}",
					render_repairs_toml(&[entry])
				))
			));
		} else if diagnostic.repair.is_none() {
			reasons.push(format!(
				"{}:{}: {}",
				diagnostic.span.start.line, diagnostic.span.start.column, diagnostic.message
			));
		}
	}
	if !reasons.is_empty() {
		return Ok(Planned::Unrepaired(reasons));
	}
	let edits: Vec<TextEdit> = repair_text_edits(&text, &parsed.diagnostics)
		.ok_or_else(|| RepairError(format!("{path}: a repair is not where the text has it")))?;
	for diagnostic in &parsed.diagnostics {
		if let Some(repair) = diagnostic.repair {
			changes.push(format!(
				"{}:{}: {}",
				diagnostic.span.start.line,
				diagnostic.span.start.column,
				repair.description()
			));
		}
	}
	if changes.is_empty() {
		return Ok(Planned::Nothing);
	}
	let mut ordered = edits;
	ordered.sort_by_key(|edit| std::cmp::Reverse(edit.offset));
	for edit in ordered {
		text.replace_range(edit.offset..edit.offset + edit.remove, edit.insert);
	}
	let Some(repaired) = reencode_paradox_bytes(&bytes, &text) else {
		return Ok(Planned::Unrepaired(vec![
			"its repairs cannot be written back in its encoding".into(),
		]));
	};
	Ok(Planned::Repair(FileRepair {
		mod_id: mod_id.to_owned(),
		path: path.to_owned(),
		source: source.to_owned(),
		changes,
		sha256_before,
		sha256_after: sha256(&repaired),
		repaired,
	}))
}

/// Checks that no source changed since `plan` read it.
fn verify_unchanged(plan: &RepairPlan) -> Result<(), RepairError> {
	for file in &plan.files {
		if sha256(&fs::read(&file.source)?) != file.sha256_before {
			return Err(RepairError(format!(
				"{} changed since the repairs were planned; plan them again",
				file.source.display()
			)));
		}
	}
	Ok(())
}

/// Writes a patch mod into `out_dir`, which must not exist or be empty: a
/// `descriptor.mod` named `name` and every repaired file at its game path.
/// Loaded after the mods it repairs, it replaces their broken files.
pub fn write_patch_mod(plan: &RepairPlan, out_dir: &Path, name: &str) -> Result<(), RepairError> {
	if out_dir.exists() && fs::read_dir(out_dir)?.next().is_some() {
		return Err(RepairError(format!(
			"{} is not empty; write the patch mod into a new or empty directory",
			out_dir.display()
		)));
	}
	verify_unchanged(plan)?;
	let mut paths = BTreeMap::new();
	for file in &plan.files {
		if let Some(other) = paths.insert(&file.path, &file.mod_id) {
			return Err(RepairError(format!(
				"{} is repaired in both {other} and {}; one patch mod can hold only one",
				file.path, file.mod_id
			)));
		}
	}
	fs::create_dir_all(out_dir)?;
	for file in &plan.files {
		let target = file.path.to_path(out_dir);
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(target, &file.repaired)?;
	}
	fs::write(
		out_dir.join("descriptor.mod"),
		format!(
			"name=\"{}\"\ntags={{\n\t\"Fixes\"\n}}\n",
			name.replace('"', "'")
		),
	)?;
	Ok(())
}

/// One file `repair_in_place` changed, as its backup manifest records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackedUpFile {
	pub mod_id: String,
	pub path: GamePathBuf,
	pub source: PathBuf,
	/// The original bytes, relative to the backup directory.
	pub backup: PathBuf,
	pub sha256_before: String,
	pub sha256_after: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BackupManifest {
	pub files: Vec<BackedUpFile>,
}

const BACKUP_MANIFEST: &str = "manifest.json";

/// The directory backups of in-place repairs are kept under.
pub fn backup_root() -> PathBuf {
	data_root().join("repair-backups")
}

/// Writes the repairs into the source files themselves. Each original is
/// copied first into a new directory under [`backup_root`], whose manifest
/// [`restore_backup`] reads; that directory is returned.
pub fn repair_in_place(plan: &RepairPlan) -> Result<PathBuf, RepairError> {
	verify_unchanged(plan)?;
	let stamp = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |elapsed| elapsed.as_millis());
	let backup_dir = backup_root().join(stamp.to_string());
	if backup_dir.exists() {
		return Err(RepairError(format!(
			"{} already exists; try again",
			backup_dir.display()
		)));
	}
	let mut manifest = BackupManifest::default();
	for file in &plan.files {
		let backup = Path::new(&file.mod_id).join(file.path.to_path(Path::new("")));
		let target = backup_dir.join(&backup);
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::copy(&file.source, &target)?;
		manifest.files.push(BackedUpFile {
			mod_id: file.mod_id.clone(),
			path: file.path.clone(),
			source: file.source.clone(),
			backup,
			sha256_before: file.sha256_before.clone(),
			sha256_after: file.sha256_after.clone(),
		});
	}
	fs::create_dir_all(&backup_dir)?;
	fs::write(
		backup_dir.join(BACKUP_MANIFEST),
		serde_json::to_vec_pretty(&manifest).map_err(|error| RepairError(error.to_string()))?,
	)?;
	for file in &plan.files {
		fs::write(&file.source, &file.repaired)?;
	}
	Ok(backup_dir)
}

/// What `restore_backup` did with each file of a backup.
#[derive(Clone, Debug, Default)]
pub struct RestoreSummary {
	pub restored: Vec<PathBuf>,
	/// Files that no longer hold the repaired bytes, left as they are: an
	/// update or another edit replaced them after the repair.
	pub skipped: Vec<PathBuf>,
}

/// Puts back the originals an in-place repair backed up into `backup_dir`,
/// for each file that still holds the bytes the repair wrote.
pub fn restore_backup(backup_dir: &Path) -> Result<RestoreSummary, RepairError> {
	let manifest: BackupManifest =
		serde_json::from_slice(&fs::read(backup_dir.join(BACKUP_MANIFEST))?)
			.map_err(|error| RepairError(format!("{}: {error}", backup_dir.display())))?;
	let mut summary = RestoreSummary::default();
	for file in manifest.files {
		let current = fs::read(&file.source).ok().map(|bytes| sha256(&bytes));
		if current.as_deref() != Some(file.sha256_after.as_str()) {
			summary.skipped.push(file.source);
			continue;
		}
		let original = fs::read(backup_dir.join(&file.backup))?;
		if sha256(&original) != file.sha256_before {
			return Err(RepairError(format!(
				"the backup of {} is damaged",
				file.source.display()
			)));
		}
		fs::write(&file.source, original)?;
		summary.restored.push(file.source);
	}
	Ok(summary)
}
