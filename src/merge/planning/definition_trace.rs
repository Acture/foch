//! Kernel-neutral projection of direct definition changes into trace participants.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::MergeTraceContributor;

use super::dag::{FileDag, ModId, topo_levels};

pub(crate) fn compute_definition_participants(
	direct_definition_keys: &HashMap<ModId, BTreeSet<String>>,
	file_dag: &FileDag,
) -> BTreeMap<String, Vec<MergeTraceContributor>> {
	let participant_set = file_dag
		.contributors()
		.iter()
		.cloned()
		.collect::<BTreeSet<_>>();
	let levels = topo_levels(&participant_set, file_dag);
	let mut dag_level_by_mod = BTreeMap::new();
	for (level_idx, level) in levels.iter().enumerate() {
		for mod_id in level {
			dag_level_by_mod.insert(mod_id.clone(), level_idx);
		}
	}

	let mut participants: BTreeMap<String, Vec<MergeTraceContributor>> = BTreeMap::new();
	let mut ordered = file_dag.contributors().to_vec();
	ordered.sort_by_key(|mod_id| {
		(
			dag_level_by_mod.get(mod_id).copied().unwrap_or(usize::MAX),
			file_dag.precedence_of(mod_id),
			mod_id.0.clone(),
		)
	});
	for mod_id in ordered {
		let Some(keys) = direct_definition_keys.get(&mod_id) else {
			continue;
		};
		for key in keys {
			participants
				.entry(key.clone())
				.or_default()
				.push(MergeTraceContributor {
					mod_id: mod_id.0.clone(),
					precedence: file_dag.precedence_of(&mod_id),
					dag_level: dag_level_by_mod.get(&mod_id).copied().unwrap_or(usize::MAX),
				});
		}
	}
	participants
}
