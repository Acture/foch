use crate::base_data::InstalledBaseSnapshot;
use foch_core::model::ModCandidate;
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::LoadedModSnapshot;

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceScriptCache {
	files: HashMap<(String, String), ParsedScriptFile>,
}

impl WorkspaceScriptCache {
	pub(crate) fn from_parts(
		mods: &[ModCandidate],
		mod_snapshots: &[Option<LoadedModSnapshot>],
		installed_base_snapshot: Option<&InstalledBaseSnapshot>,
		base_game_root: Option<&Path>,
	) -> Self {
		let mut cache = Self::default();
		for snapshot in mod_snapshots.iter().flatten() {
			for document in &snapshot.parsed_documents {
				cache.insert(document.clone());
			}
		}
		if let (Some(installed), Some(root)) = (installed_base_snapshot, base_game_root) {
			match installed.snapshot.parsed_script_files(root) {
				Ok(documents) => {
					for document in documents {
						cache.insert(document);
					}
				}
				Err(err) => {
					tracing::warn!(
						target: "foch::workspace::scripts",
						error = %err,
						"failed to decode base parsed script cache; falling back to on-demand parse"
					);
				}
			}
		}
		for (mod_item, snapshot) in mods.iter().zip(mod_snapshots.iter()) {
			if snapshot.is_none() {
				tracing::debug!(
					target: "foch::workspace::scripts",
					mod_id = %mod_item.mod_id,
					"no parsed script snapshot for mod"
				);
			}
		}
		cache
	}

	pub(crate) fn get(&self, mod_id: &str, relative_path: &Path) -> Option<&ParsedScriptFile> {
		self.files
			.get(&(mod_id.to_string(), normalize_path(relative_path)))
	}

	pub(crate) fn documents_for_mods(
		&self,
		enabled_mod_ids: &HashSet<String>,
		base_mod_id: Option<&str>,
	) -> Vec<ParsedScriptFile> {
		let mut documents = self
			.files
			.values()
			.filter(|document| {
				enabled_mod_ids.contains(&document.mod_id)
					|| base_mod_id.is_some_and(|base| document.mod_id == base)
			})
			.cloned()
			.collect::<Vec<_>>();
		documents.sort_by(|lhs, rhs| {
			(lhs.mod_id.as_str(), lhs.relative_path.as_os_str())
				.cmp(&(rhs.mod_id.as_str(), rhs.relative_path.as_os_str()))
		});
		documents
	}

	fn insert(&mut self, document: ParsedScriptFile) {
		self.files.insert(
			(
				document.mod_id.clone(),
				normalize_path(&document.relative_path),
			),
			document,
		);
	}
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}
