//! Kernel-neutral execution over a per-file dependency DAG.
//!
//! The walker owns topology, parent-view caching, shared-ancestor resolution,
//! and intermediate/final join scheduling. Kernel implementations supply only
//! effective-node construction and join semantics through the two protocols.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use foch_language::analyzer::semantic_index::ParsedScriptFile;

use super::dag::{FileDag, ModId, topo_levels};
use super::dag_join::{DagJoinPlan, DagJoinScope, plan_dag_join, sink_mods};

pub(crate) struct EffectiveNodeRequest<'a, State> {
	pub mod_id: &'a ModId,
	pub precedence: usize,
	pub parent: &'a State,
	pub source: &'a ParsedScriptFile,
}

/// Builds one mod node's effective state from its resolved parent and source.
pub(crate) trait EffectiveNodeProtocol<State: Clone> {
	fn effective_node(&mut self, request: EffectiveNodeRequest<'_, State>)
	-> Result<State, String>;
}

pub(crate) struct DagJoinRevision<'a, State> {
	pub mod_id: &'a ModId,
	pub precedence: usize,
	pub state: &'a State,
}

pub(crate) struct DagJoinRequest<'a, State> {
	pub plan: &'a DagJoinPlan,
	pub file_dag: &'a FileDag,
	pub base: &'a State,
	pub revisions: Vec<DagJoinRevision<'a, State>>,
}

/// Merges incomparable DAG revisions against their shared base.
pub(crate) trait DagJoinProtocol<State: Clone> {
	fn validate_final_frontier(
		&self,
		_file_dag: &FileDag,
		_root: &State,
		_sinks: &[ModId],
	) -> Result<(), String> {
		Ok(())
	}

	fn join(&mut self, request: DagJoinRequest<'_, State>) -> Result<State, String>;
}

pub(crate) struct DagPipelineResult<State> {
	pub final_state: State,
	pub parent_states: HashMap<ModId, State>,
}

pub(crate) fn execute_dag_pipeline<State, Protocol>(
	file_dag: &FileDag,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	root: State,
	protocol: &mut Protocol,
) -> Result<DagPipelineResult<State>, String>
where
	State: Clone,
	Protocol: EffectiveNodeProtocol<State> + DagJoinProtocol<State>,
{
	let all_contributors = file_dag
		.contributors()
		.iter()
		.cloned()
		.collect::<BTreeSet<_>>();
	let mut node_states = HashMap::new();
	let mut parent_states = HashMap::new();
	let mut join_cache = BTreeMap::new();

	for level in topo_levels(&all_contributors, file_dag) {
		for mod_id in &level {
			let parent = parent_state_for(
				mod_id,
				file_dag,
				&root,
				&node_states,
				protocol,
				&mut join_cache,
			)?;
			let source = contributors.get(mod_id).ok_or_else(|| {
				format!(
					"missing parsed contributor {} for {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
			let state = protocol.effective_node(EffectiveNodeRequest {
				mod_id,
				precedence: file_dag.precedence_of(mod_id),
				parent: &parent,
				source,
			})?;
			parent_states.insert(mod_id.clone(), parent);
			node_states.insert(mod_id.clone(), state);
		}
	}

	let sinks = sink_mods(file_dag);
	protocol.validate_final_frontier(file_dag, &root, &sinks)?;
	let final_state = match sinks.as_slice() {
		[] => root.clone(),
		[sink] => node_states
			.get(sink)
			.cloned()
			.ok_or_else(|| format!("missing final state for sink {}", sink.as_str()))?,
		_ => resolve_join(
			&sinks,
			DagJoinScope::Final,
			file_dag,
			&root,
			&node_states,
			protocol,
			&mut join_cache,
		)?,
	};

	Ok(DagPipelineResult {
		final_state,
		parent_states,
	})
}

fn parent_state_for<State, Protocol>(
	mod_id: &ModId,
	file_dag: &FileDag,
	root: &State,
	node_states: &HashMap<ModId, State>,
	protocol: &mut Protocol,
	join_cache: &mut BTreeMap<BTreeSet<ModId>, State>,
) -> Result<State, String>
where
	State: Clone,
	Protocol: DagJoinProtocol<State>,
{
	match file_dag.parents_of(mod_id) {
		[] => Ok(root.clone()),
		[parent] => node_states.get(parent).cloned().ok_or_else(|| {
			format!(
				"missing direct parent state {} for {}",
				parent.as_str(),
				mod_id.as_str()
			)
		}),
		parents => {
			let cache_key = parents.iter().cloned().collect::<BTreeSet<_>>();
			if let Some(cached) = join_cache.get(&cache_key) {
				return Ok(cached.clone());
			}
			let state = resolve_join(
				parents,
				DagJoinScope::Intermediate,
				file_dag,
				root,
				node_states,
				protocol,
				join_cache,
			)?;
			join_cache.insert(cache_key, state.clone());
			Ok(state)
		}
	}
}

fn resolve_join<State, Protocol>(
	branch_ids: &[ModId],
	scope: DagJoinScope,
	file_dag: &FileDag,
	root: &State,
	node_states: &HashMap<ModId, State>,
	protocol: &mut Protocol,
	join_cache: &mut BTreeMap<BTreeSet<ModId>, State>,
) -> Result<State, String>
where
	State: Clone,
	Protocol: DagJoinProtocol<State>,
{
	let plan = plan_dag_join(branch_ids, file_dag, scope)?;
	let base = match plan.shared_frontier() {
		[] => root.clone(),
		[shared] => node_states.get(shared).cloned().ok_or_else(|| {
			format!(
				"missing shared ancestor state {} for {}",
				shared.as_str(),
				file_dag.file_path()
			)
		})?,
		shared_frontier => {
			let cache_key = shared_frontier.iter().cloned().collect::<BTreeSet<_>>();
			if let Some(cached) = join_cache.get(&cache_key) {
				cached.clone()
			} else {
				let state = resolve_join(
					shared_frontier,
					DagJoinScope::Intermediate,
					file_dag,
					root,
					node_states,
					protocol,
					join_cache,
				)?;
				join_cache.insert(cache_key, state.clone());
				state
			}
		}
	};
	let revisions = plan
		.ordered_branches()
		.iter()
		.map(|mod_id| {
			let state = node_states.get(mod_id).ok_or_else(|| {
				format!(
					"missing revision state {} for {}",
					mod_id.as_str(),
					file_dag.file_path()
				)
			})?;
			Ok(DagJoinRevision {
				mod_id,
				precedence: file_dag.precedence_of(mod_id),
				state,
			})
		})
		.collect::<Result<Vec<_>, String>>()?;
	protocol.join(DagJoinRequest {
		plan: &plan,
		file_dag,
		base: &base,
		revisions,
	})
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::ModCandidate;
	use foch_language::analyzer::content_family::CwtType;
	use foch_language::analyzer::parser::parse_clausewitz_content;

	use super::*;
	use crate::workspace::ResolvedFileContributor;

	use super::super::dag::{IgnoreReplacePath, build_mod_dag, induced_file_dag_with_overrides};

	#[derive(Clone, Debug, Eq, PartialEq)]
	struct TestState(Vec<String>);

	#[derive(Default)]
	struct RecordingProtocol {
		effective_parents: Vec<(String, Vec<String>)>,
		joins: Vec<(DagJoinScope, Vec<String>)>,
	}

	impl EffectiveNodeProtocol<TestState> for RecordingProtocol {
		fn effective_node(
			&mut self,
			request: EffectiveNodeRequest<'_, TestState>,
		) -> Result<TestState, String> {
			self.effective_parents
				.push((request.mod_id.0.clone(), request.parent.0.clone()));
			Ok(TestState(vec![request.mod_id.0.clone()]))
		}
	}

	impl DagJoinProtocol<TestState> for RecordingProtocol {
		fn join(&mut self, request: DagJoinRequest<'_, TestState>) -> Result<TestState, String> {
			let branch_ids = request
				.revisions
				.iter()
				.map(|revision| revision.mod_id.0.clone())
				.collect::<Vec<_>>();
			self.joins.push((request.plan.scope(), branch_ids.clone()));
			Ok(TestState(vec![format!(
				"{:?}:{}",
				request.plan.scope(),
				branch_ids.join("+")
			)]))
		}
	}

	fn mod_candidate(mod_id: &str, name: &str, dependencies: &[&str]) -> ModCandidate {
		ModCandidate {
			entry: PlaylistEntry {
				steam_id: Some(mod_id.to_string()),
				..PlaylistEntry::default()
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

	fn parsed(mod_id: &str) -> ParsedScriptFile {
		let path = PathBuf::from("common/test.txt");
		let parsed = parse_clausewitz_content(path.clone(), &format!("{mod_id} = yes\n"));
		ParsedScriptFile {
			mod_id: mod_id.to_string(),
			path: path.clone(),
			relative_path: path,
			content_family: None,
			file_kind: CwtType::new("other"),
			module_name: "test".to_string(),
			ast: parsed.ast,
			source: format!("{mod_id} = yes\n"),
			parse_issues: Vec::new(),
			parse_cache_hit: false,
		}
	}

	#[test]
	fn walker_routes_effective_nodes_and_every_join_through_protocols() {
		let mods = vec![
			mod_candidate("a", "A", &[]),
			mod_candidate("b", "B", &[]),
			mod_candidate("c", "C", &["A", "B"]),
			mod_candidate("d", "D", &[]),
		];
		let contributors = vec![
			contributor("a", 0),
			contributor("b", 1),
			contributor("c", 2),
			contributor("d", 3),
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
		let parsed = ["a", "b", "c", "d"]
			.into_iter()
			.map(|mod_id| (ModId::from(mod_id), parsed(mod_id)))
			.collect::<HashMap<_, _>>();
		let mut protocol = RecordingProtocol::default();

		let result = execute_dag_pipeline(
			&file_dag,
			&parsed,
			TestState(vec!["root".to_string()]),
			&mut protocol,
		)
		.expect("execute protocol-driven DAG");

		assert_eq!(
			protocol.joins,
			vec![
				(
					DagJoinScope::Intermediate,
					vec!["a".to_string(), "b".to_string()]
				),
				(DagJoinScope::Final, vec!["c".to_string(), "d".to_string()]),
			]
		);
		assert_eq!(
			protocol
				.effective_parents
				.iter()
				.find(|(mod_id, _)| mod_id == "c")
				.map(|(_, parent)| parent.as_slice()),
			Some(["Intermediate:a+b".to_string()].as_slice())
		);
		assert_eq!(result.final_state.0, vec!["Final:c+d"]);
	}
}
