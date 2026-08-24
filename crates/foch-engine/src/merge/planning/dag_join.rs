use std::collections::{BTreeSet, HashMap, HashSet};

use super::dag::{FileDag, ModId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DagJoinScope {
	Intermediate,
	Final,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DagJoinPlan {
	scope: DagJoinScope,
	ordered_branches: Vec<ModId>,
	shared_frontier: Vec<ModId>,
}

impl DagJoinPlan {
	pub(crate) fn scope(&self) -> DagJoinScope {
		self.scope
	}

	pub(crate) fn ordered_branches(&self) -> &[ModId] {
		&self.ordered_branches
	}

	pub(crate) fn shared_frontier(&self) -> &[ModId] {
		&self.shared_frontier
	}
}

pub(crate) fn plan_dag_join(
	branch_ids: &[ModId],
	file_dag: &FileDag,
	scope: DagJoinScope,
) -> Result<DagJoinPlan, String> {
	let mut ordered_branches = branch_ids.to_vec();
	ordered_branches.sort_by(|left, right| {
		file_dag
			.precedence_of(left)
			.cmp(&file_dag.precedence_of(right))
			.then_with(|| left.cmp(right))
	});
	ordered_branches.dedup();
	let shared_frontier = common_frontier(&ordered_branches, file_dag)?;
	Ok(DagJoinPlan {
		scope,
		ordered_branches,
		shared_frontier,
	})
}

pub(crate) fn sink_mods(file_dag: &FileDag) -> Vec<ModId> {
	let non_sinks = file_dag
		.contributors()
		.iter()
		.flat_map(|mod_id| file_dag.parents_of(mod_id).iter().cloned())
		.collect::<HashSet<_>>();
	file_dag
		.contributors()
		.iter()
		.filter(|mod_id| !non_sinks.contains(*mod_id))
		.cloned()
		.collect()
}

/// Maximal nodes present in every branch ancestry. The union ancestry is walked
/// once, then compact branch-coverage bitsets flow from tips to parents. For
/// `k` branches this costs `O(V + E * ceil(k / 64))` time and
/// `O(V * ceil(k / 64))` transient storage.
fn common_frontier(branch_ids: &[ModId], file_dag: &FileDag) -> Result<Vec<ModId>, String> {
	if branch_ids.is_empty() {
		return Ok(Vec::new());
	}

	let mut ancestor_nodes = BTreeSet::new();
	let mut stack = branch_ids.to_vec();
	while let Some(candidate) = stack.pop() {
		if !ancestor_nodes.insert(candidate.clone()) {
			continue;
		}
		for parent in file_dag.parents_of(&candidate).iter().rev() {
			stack.push(parent.clone());
		}
		record_ancestry_work(1, ancestor_nodes.len() + stack.len());
	}

	let mut remaining_children = ancestor_nodes
		.iter()
		.cloned()
		.map(|node| (node, 0usize))
		.collect::<HashMap<_, _>>();
	for child in &ancestor_nodes {
		for parent in file_dag.parents_of(child) {
			if let Some(child_count) = remaining_children.get_mut(parent) {
				*child_count += 1;
			}
		}
	}

	let word_count = branch_ids.len().div_ceil(u64::BITS as usize);
	let mut full_coverage = vec![u64::MAX; word_count];
	let final_word_bits = branch_ids.len() % u64::BITS as usize;
	if final_word_bits != 0 {
		full_coverage[word_count - 1] = (1_u64 << final_word_bits) - 1;
	}
	let mut coverage: HashMap<ModId, Vec<u64>> = HashMap::new();
	for (branch_index, branch_id) in branch_ids.iter().enumerate() {
		coverage
			.entry(branch_id.clone())
			.or_insert_with(|| vec![0; word_count])[branch_index / u64::BITS as usize] |=
			1_u64 << (branch_index % u64::BITS as usize);
	}

	let mut ready = remaining_children
		.iter()
		.filter(|(_, child_count)| **child_count == 0)
		.map(|(node, _)| (file_dag.precedence_of(node), node.clone()))
		.collect::<BTreeSet<_>>();
	record_ancestry_work(
		0,
		ancestor_nodes.len() + remaining_children.len() + coverage.len() * word_count,
	);
	let mut common = BTreeSet::new();
	let mut processed = 0;
	let traversal_node_units = ancestor_nodes.len() + remaining_children.len();
	while let Some((_, node)) = ready.pop_first() {
		let node_coverage = coverage
			.remove(&node)
			.unwrap_or_else(|| vec![0; word_count]);
		if node_coverage == full_coverage {
			common.insert(node.clone());
		}
		for parent in file_dag.parents_of(&node) {
			let Some(child_count) = remaining_children.get_mut(parent) else {
				continue;
			};
			{
				let parent_coverage = coverage
					.entry(parent.clone())
					.or_insert_with(|| vec![0; word_count]);
				for (parent_word, node_word) in parent_coverage.iter_mut().zip(&node_coverage) {
					*parent_word |= node_word;
				}
			}
			record_coverage_word_unions(
				word_count,
				traversal_node_units + coverage.len() * word_count,
			);
			*child_count -= 1;
			if *child_count == 0 {
				ready.insert((file_dag.precedence_of(parent), parent.clone()));
			}
		}
		processed += 1;
	}
	if processed != ancestor_nodes.len() {
		return Err(format!(
			"dependency cycle while resolving shared ancestry for {}",
			file_dag.file_path()
		));
	}

	let mut inherited = BTreeSet::new();
	for candidate in &common {
		record_ancestry_work(1, common.len() + inherited.len());
		for parent in file_dag.parents_of(candidate) {
			record_ancestry_work(1, common.len() + inherited.len());
			if common.contains(parent) {
				inherited.insert(parent.clone());
			}
		}
	}
	let mut frontier = common.difference(&inherited).cloned().collect::<Vec<_>>();
	frontier.sort_by(|left, right| {
		file_dag
			.precedence_of(left)
			.cmp(&file_dag.precedence_of(right))
			.then_with(|| left.cmp(right))
	});
	Ok(frontier)
}

#[cfg(test)]
std::thread_local! {
	static ANCESTRY_METRICS: std::cell::Cell<AncestryMetrics> = const {
		std::cell::Cell::new(AncestryMetrics {
			work_units: 0,
			coverage_word_unions: 0,
			peak_transient_nodes: 0,
		})
	};
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AncestryMetrics {
	pub(crate) work_units: usize,
	pub(crate) coverage_word_unions: usize,
	pub(crate) peak_transient_nodes: usize,
}

#[cfg(test)]
fn record_ancestry_work(steps: usize, transient_nodes: usize) {
	ANCESTRY_METRICS.with(|metrics| {
		let mut current = metrics.get();
		current.work_units += steps;
		current.peak_transient_nodes = current.peak_transient_nodes.max(transient_nodes);
		metrics.set(current);
	});
}

#[cfg(test)]
fn record_coverage_word_unions(words: usize, transient_nodes: usize) {
	ANCESTRY_METRICS.with(|metrics| {
		let mut current = metrics.get();
		current.work_units += words;
		current.coverage_word_unions += words;
		current.peak_transient_nodes = current.peak_transient_nodes.max(transient_nodes);
		metrics.set(current);
	});
}

#[cfg(not(test))]
#[inline]
fn record_ancestry_work(_steps: usize, _transient_nodes: usize) {}

#[cfg(not(test))]
#[inline]
fn record_coverage_word_unions(_words: usize, _transient_nodes: usize) {}

#[cfg(test)]
pub(crate) fn reset_ancestry_metrics() {
	ANCESTRY_METRICS.with(|metrics| metrics.set(AncestryMetrics::default()));
}

#[cfg(test)]
pub(crate) fn ancestry_metrics() -> AncestryMetrics {
	ANCESTRY_METRICS.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch::model::ModCandidate;
	use foch::playset::PlaysetEntry;
	use foch::playset::descriptor::ModDescriptor;

	use super::*;
	use crate::workspace::ResolvedFileContributor;

	use super::super::dag::{IgnoreReplacePath, build_mod_dag, induced_file_dag_with_overrides};

	fn mod_candidate(mod_id: &str, name: &str, dependencies: &[&str]) -> ModCandidate {
		ModCandidate {
			entry: PlaysetEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaysetEntry::default()
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: dependencies
					.iter()
					.map(|dependency| (*dependency).to_string())
					.collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	fn contributor(mod_id: &str, precedence: usize) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/common/test.txt")),
			precedence,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: None,
			mod_hash: None,
		}
	}

	#[test]
	fn join_topology_is_deterministic_without_a_kernel() {
		let mods = vec![
			mod_candidate("base", "Base", &[]),
			mod_candidate("left", "Left", &["Base"]),
			mod_candidate("right", "Right", &["Base"]),
		];
		let contributors = vec![
			contributor("base", 0),
			contributor("left", 1),
			contributor("right", 2),
		];
		let (mod_dag, diagnostics) = build_mod_dag(&mods);
		assert!(diagnostics.is_empty(), "{diagnostics:?}");
		let file_dag = induced_file_dag_with_overrides(
			&mod_dag,
			"common/test.txt",
			&contributors,
			&IgnoreReplacePath::None,
			&[],
		);

		let plan = plan_dag_join(
			&[
				ModId::from("right"),
				ModId::from("left"),
				ModId::from("left"),
			],
			&file_dag,
			DagJoinScope::Final,
		)
		.expect("plan DAG join");

		assert_eq!(
			plan.ordered_branches(),
			&[ModId::from("left"), ModId::from("right")]
		);
		assert_eq!(plan.shared_frontier(), &[ModId::from("base")]);
		assert_eq!(plan.scope(), DagJoinScope::Final);
		assert_eq!(
			sink_mods(&file_dag),
			vec![ModId::from("left"), ModId::from("right")]
		);
	}
}
