use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use foch_language::analyzer::parser::{AstFile, parse_clausewitz_file};

use crate::common_module::CommonModuleViewBuilder;
use crate::config::Eu4GameDiscovery;
use crate::dataset::{DatasetPaths, ObservationRecord, SnapshotRecord, read_jsonl};
use crate::object_store::ObjectStore;
use crate::score::definition_module_policy_for_path;

pub(crate) struct LoadedSnapshot {
	pub(crate) snapshot: SnapshotRecord,
	pub(crate) compatch: PathBuf,
	pub(crate) source_dirs: Vec<PathBuf>,
}

pub(crate) fn validate_snapshot_game(
	snapshot: &SnapshotRecord,
	game: &Eu4GameDiscovery,
) -> Result<(), Box<dyn std::error::Error>> {
	if snapshot.game.version != game.game_version {
		return Err(format!(
			"base-game version mismatch for {}: snapshot={} local={}",
			snapshot.case_id, snapshot.game.version, game.game_version
		)
		.into());
	}
	if snapshot.game.steam_build_id != game.steam_build_id {
		return Err(format!(
			"Steam build mismatch for {}: snapshot={:?} local={:?}",
			snapshot.case_id, snapshot.game.steam_build_id, game.steam_build_id
		)
		.into());
	}
	Ok(())
}

/// Opens an immutable snapshot after verifying every referenced CAS object once
/// for the lifetime of `store`.
pub(crate) fn open_snapshot(
	store: &ObjectStore,
	snapshot: SnapshotRecord,
) -> io::Result<LoadedSnapshot> {
	let compatch = store.verify_once(&snapshot.compatch.content_hash)?.tree;
	let source_dirs = snapshot
		.source_mods
		.iter()
		.map(|source| {
			store
				.verify_once(&source.content_hash)
				.map(|object| object.tree)
		})
		.collect::<io::Result<Vec<_>>>()?;
	Ok(LoadedSnapshot {
		snapshot,
		compatch,
		source_dirs,
	})
}

pub(crate) fn adjudication_ast_pair(
	relative_path: &str,
	loaded: &LoadedSnapshot,
	game_root: &Path,
	output_dir: &Path,
) -> Option<(AstFile, AstFile)> {
	if let Some(policy) = definition_module_policy_for_path(relative_path) {
		let mut builder = CommonModuleViewBuilder::default();
		let mut candidate_roots = Vec::with_capacity(loaded.source_dirs.len() + 2);
		candidate_roots.push(game_root);
		candidate_roots.extend(loaded.source_dirs.iter().map(PathBuf::as_path));
		candidate_roots.push(output_dir);
		let mut human_roots = Vec::with_capacity(loaded.source_dirs.len() + 2);
		human_roots.push(game_root);
		human_roots.extend(loaded.source_dirs.iter().map(PathBuf::as_path));
		human_roots.push(loaded.compatch.as_path());
		let candidate = builder
			.view(&candidate_roots, policy.namespace_prefix)
			.ok()?;
		let human = builder.view(&human_roots, policy.namespace_prefix).ok()?;
		return Some((candidate.as_ref().clone(), human.as_ref().clone()));
	}

	let candidate = parse_clausewitz_file(&output_dir.join(relative_path));
	let human = parse_clausewitz_file(&loaded.compatch.join(relative_path));
	(candidate.diagnostics.is_empty() && human.diagnostics.is_empty())
		.then_some((candidate.ast, human.ast))
}

pub(crate) fn latest_snapshots(paths: &DatasetPaths) -> io::Result<Vec<SnapshotRecord>> {
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)?;
	let observations = read_jsonl::<ObservationRecord>(&paths.observations)?;
	let mut observed_at = HashMap::new();
	for observation in &observations {
		observed_at
			.entry(observation.snapshot_id.as_str())
			.and_modify(|current: &mut &str| {
				if observation.observed_at.as_str() > *current {
					*current = observation.observed_at.as_str();
				}
			})
			.or_insert(observation.observed_at.as_str());
	}
	let mut latest: BTreeMap<String, (String, SnapshotRecord)> = BTreeMap::new();
	for snapshot in snapshots {
		let timestamp = observed_at
			.get(snapshot.snapshot_id.as_str())
			.copied()
			.unwrap_or("")
			.to_string();
		latest
			.entry(snapshot.case_id.clone())
			.and_modify(|current| {
				if (timestamp.as_str(), snapshot.snapshot_id.as_str())
					> (current.0.as_str(), current.1.snapshot_id.as_str())
				{
					*current = (timestamp.clone(), snapshot.clone());
				}
			})
			.or_insert((timestamp, snapshot));
	}
	Ok(latest.into_values().map(|(_, snapshot)| snapshot).collect())
}
