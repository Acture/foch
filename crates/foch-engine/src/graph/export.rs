use super::model::{
	GraphArtifactFormat, GraphBuildOptions, GraphBuildSummary, GraphModeSelection,
	GraphRootSelector, GraphScopeSelection,
};
use super::semantic::run_semantic_graph_with_options;
use crate::request::CheckRequest;
use crate::runtime::{
	DependencyMatchKind, OverlapStatus, build_runtime_state_for_request, dependency_hint_for_edge,
	nearest_enclosing_definition, runtime_reference_target,
};
use crate::workspace::resolve_workspace;
use foch_core::model::SymbolKind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CallsNodeKind {
	Definition,
	File,
	Unresolved,
	DiscardableDefinition,
	MergeCandidateDefinition,
	OvershadowConflictDefinition,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CallsEdgeKind {
	Calls,
	UnresolvedCall,
	Overrides,
	DeclaredDependencyHint,
}

#[derive(Clone, Debug, Serialize)]
struct CallsiteRecord {
	path: String,
	line: usize,
	column: usize,
	reference_kind: String,
	reference_name: String,
	caller_mod_id: String,
}

#[derive(Clone, Debug, Serialize)]
struct CallsNode {
	id: String,
	kind: CallsNodeKind,
	#[serde(skip_serializing_if = "Option::is_none")]
	symbol_kind: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	name: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	mod_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	path: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	line: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	column: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct CallsEdge {
	kind: CallsEdgeKind,
	from: String,
	to: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	declared_dependency: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	dependency_match_kind: Option<DependencyMatchKind>,
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	callsites: Vec<CallsiteRecord>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct CallsGraphArtifact {
	nodes: Vec<CallsNode>,
	edges: Vec<CallsEdge>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModDepNodeKind {
	Mod,
	BaseGame,
	MissingDependency,
}

#[derive(Clone, Debug, Serialize)]
struct ModDepNode {
	id: String,
	kind: ModDepNodeKind,
	name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	mod_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ModDepEdge {
	from: String,
	to: String,
	kind: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	match_kind: Option<DependencyMatchKind>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ModDepsGraphArtifact {
	nodes: Vec<ModDepNode>,
	edges: Vec<ModDepEdge>,
}

pub fn run_graph_with_options(
	request: CheckRequest,
	out_dir: &Path,
	options: GraphBuildOptions,
) -> Result<GraphBuildSummary, Box<dyn std::error::Error>> {
	if matches!(options.mode, GraphModeSelection::Semantic) {
		return run_semantic_graph_with_options(request, out_dir, options);
	}
	let state = build_runtime_state_for_request(&request, options.include_game_base)?;
	let workspace_calls = build_workspace_calls_graph(&state);
	let workspace_deps = build_workspace_mod_deps_graph(&state, &request);
	let workspace_def_deps = build_workspace_definition_deps_artifact(&state);
	let mut summary = GraphBuildSummary {
		out_dir: out_dir.to_path_buf(),
		..GraphBuildSummary::default()
	};

	match options.scope {
		GraphScopeSelection::Workspace | GraphScopeSelection::All => {
			write_graph_pair(
				&out_dir.join("workspace"),
				"calls",
				&workspace_calls,
				render_calls_dot(&workspace_calls),
				options.format,
			)?;
			write_graph_pair(
				&out_dir.join("workspace"),
				"mod-deps",
				&workspace_deps,
				render_mod_deps_dot(&workspace_deps),
				options.format,
			)?;
			write_graph_pair(
				&out_dir.join("workspace"),
				"definition-deps",
				&workspace_def_deps,
				render_definition_deps_dot(&workspace_def_deps),
				options.format,
			)?;
			summary.workspace_written = true;
		}
		_ => {}
	}

	if matches!(
		options.scope,
		GraphScopeSelection::Base | GraphScopeSelection::All
	) && let Some(base_mod_id) = state.base_game_mod_id.as_ref()
	{
		let base_calls = filter_calls_graph_by_mod(&workspace_calls, base_mod_id);
		let base_deps = filter_mod_deps_graph_by_mod(&workspace_deps, base_mod_id);
		write_graph_pair(
			&out_dir.join("base-game"),
			"calls",
			&base_calls,
			render_calls_dot(&base_calls),
			options.format,
		)?;
		write_graph_pair(
			&out_dir.join("base-game"),
			"mod-deps",
			&base_deps,
			render_mod_deps_dot(&base_deps),
			options.format,
		)?;
		summary.base_written = true;
	}

	if matches!(
		options.scope,
		GraphScopeSelection::Mods | GraphScopeSelection::All
	) {
		let mut mod_ids = state.enabled_mod_ids.iter().cloned().collect::<Vec<_>>();
		mod_ids.sort();
		for mod_id in mod_ids {
			let calls = filter_calls_graph_by_mod(&workspace_calls, &mod_id);
			let deps = filter_mod_deps_graph_by_mod(&workspace_deps, &mod_id);
			write_graph_pair(
				&out_dir.join("mods").join(&mod_id),
				"calls",
				&calls,
				render_calls_dot(&calls),
				options.format,
			)?;
			write_graph_pair(
				&out_dir.join("mods").join(&mod_id),
				"mod-deps",
				&deps,
				render_mod_deps_dot(&deps),
				options.format,
			)?;
			summary.mod_count += 1;
		}
	}

	if let Some(root) = options.root {
		let tree = filter_calls_graph_by_root(&workspace_calls, &root);
		let file_stem = sanitize_root_name(&root);
		write_graph_pair(
			&out_dir.join("trees"),
			&file_stem,
			&tree,
			render_calls_dot(&tree),
			options.format,
		)?;
		summary.tree_written = true;
	}

	Ok(summary)
}

fn build_workspace_calls_graph(state: &crate::runtime::RuntimeState) -> CallsGraphArtifact {
	let mut nodes = BTreeMap::<String, CallsNode>::new();
	let mut edges = BTreeMap::<(u8, String, String), CallsEdge>::new();

	for definition in &state.definitions {
		nodes.insert(
			definition_node_id(definition.index),
			CallsNode {
				id: definition_node_id(definition.index),
				kind: definition_kind_for_status(
					state
						.overlap_status_by_def
						.get(&definition.index)
						.copied()
						.unwrap_or(OverlapStatus::None),
				),
				symbol_kind: Some(symbol_kind_text(definition.kind).to_string()),
				name: Some(definition.name.clone()),
				mod_id: Some(definition.mod_id.clone()),
				path: Some(definition.path.clone()),
				line: Some(definition.line),
				column: Some(definition.column),
			},
		);
	}

	let mut grouped = HashMap::<(SymbolKind, String), Vec<usize>>::new();
	for definition in &state.definitions {
		grouped
			.entry((definition.kind, definition.name.clone()))
			.or_default()
			.push(definition.index);
	}
	for ((kind, name), def_indices) in grouped {
		if def_indices.len() < 2 {
			continue;
		}
		let Some(winner) = state.winner_by_symbol.get(&(kind, name.clone())).copied() else {
			continue;
		};
		for def_idx in def_indices {
			if def_idx == winner {
				continue;
			}
			edges.insert(
				(2, definition_node_id(def_idx), definition_node_id(winner)),
				CallsEdge {
					kind: CallsEdgeKind::Overrides,
					from: definition_node_id(def_idx),
					to: definition_node_id(winner),
					declared_dependency: None,
					dependency_match_kind: None,
					callsites: Vec::new(),
				},
			);
		}
	}

	for reference in state.semantic_index.references.iter().filter(|reference| {
		matches!(
			reference.kind,
			SymbolKind::Event | SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger
		)
	}) {
		let caller_id =
			if let Some(def_idx) = nearest_enclosing_definition(state, reference.scope_id) {
				definition_node_id(def_idx)
			} else {
				let node = file_node(reference.mod_id.as_str(), &reference.path);
				nodes.entry(node.id.clone()).or_insert(node.clone());
				node.id
			};
		let callsite = CallsiteRecord {
			path: normalize_path(&reference.path),
			line: reference.line,
			column: reference.column,
			reference_kind: symbol_kind_text(reference.kind).to_string(),
			reference_name: reference.name.clone(),
			caller_mod_id: reference.mod_id.clone(),
		};
		if let Some(target_idx) = runtime_reference_target(state, reference) {
			let target_id = definition_node_id(target_idx);
			let caller_mod_id = reference.mod_id.as_str();
			let callee_mod_id = state
				.definitions
				.iter()
				.find(|definition| definition.index == target_idx)
				.map(|definition| definition.mod_id.as_str())
				.unwrap_or(caller_mod_id);
			let dependency_hint = if caller_mod_id != callee_mod_id {
				let (declared, match_kind) =
					dependency_hint_for_edge(state, caller_mod_id, callee_mod_id);
				Some((declared, match_kind))
			} else {
				None
			};
			append_callsite_edge(
				&mut edges,
				CallsEdgeKind::Calls,
				caller_id.clone(),
				target_id.clone(),
				callsite.clone(),
				dependency_hint.map(|(declared, _)| declared),
				dependency_hint.map(|(_, match_kind)| match_kind),
			);
			if caller_mod_id != callee_mod_id {
				let (declared, match_kind) =
					dependency_hint.expect("cross-mod calls should have dependency hint");
				append_callsite_edge(
					&mut edges,
					CallsEdgeKind::DeclaredDependencyHint,
					caller_id.clone(),
					target_id,
					callsite,
					Some(declared),
					Some(match_kind),
				);
			}
		} else {
			let unresolved_id = format!(
				"unresolved:{}:{}",
				symbol_kind_text(reference.kind),
				reference.name
			);
			nodes.entry(unresolved_id.clone()).or_insert(CallsNode {
				id: unresolved_id.clone(),
				kind: CallsNodeKind::Unresolved,
				symbol_kind: Some(symbol_kind_text(reference.kind).to_string()),
				name: Some(reference.name.clone()),
				mod_id: None,
				path: None,
				line: None,
				column: None,
			});
			append_callsite_edge(
				&mut edges,
				CallsEdgeKind::UnresolvedCall,
				caller_id,
				unresolved_id,
				callsite,
				None,
				None,
			);
		}
	}

	CallsGraphArtifact {
		nodes: nodes.into_values().collect(),
		edges: edges.into_values().collect(),
	}
}

fn build_workspace_mod_deps_graph(
	state: &crate::runtime::RuntimeState,
	request: &CheckRequest,
) -> ModDepsGraphArtifact {
	let mut nodes = BTreeMap::<String, ModDepNode>::new();
	let mut edges = BTreeSet::<(String, String, Option<DependencyMatchKind>)>::new();

	if let Some(base_mod_id) = state.base_game_mod_id.as_ref() {
		nodes.insert(
			base_mod_id.clone(),
			ModDepNode {
				id: base_mod_id.clone(),
				kind: ModDepNodeKind::BaseGame,
				name: "base-game".to_string(),
				mod_id: Some(base_mod_id.clone()),
			},
		);
	}

	let workspace = resolve_workspace(request, state.base_game_mod_id.is_some())
		.map_err(|err| err.message)
		.ok();
	if let Some(workspace) = workspace {
		let mut id_lookup = HashMap::<String, String>::new();
		let mut name_lookup = HashMap::<String, String>::new();
		for mod_item in workspace.mods.iter().filter(|item| item.entry.enabled) {
			nodes.insert(
				mod_item.mod_id.clone(),
				ModDepNode {
					id: mod_item.mod_id.clone(),
					kind: ModDepNodeKind::Mod,
					name: mod_item
						.descriptor
						.as_ref()
						.map(|descriptor| descriptor.name.clone())
						.unwrap_or_else(|| mod_item.mod_id.clone()),
					mod_id: Some(mod_item.mod_id.clone()),
				},
			);
			id_lookup.insert(mod_item.mod_id.clone(), mod_item.mod_id.clone());
			if let Some(descriptor) = mod_item.descriptor.as_ref() {
				name_lookup.insert(descriptor.name.clone(), mod_item.mod_id.clone());
			}
		}
		for mod_item in workspace.mods.iter().filter(|item| item.entry.enabled) {
			let Some(descriptor) = mod_item.descriptor.as_ref() else {
				continue;
			};
			for dependency in &descriptor.dependencies {
				let resolved = id_lookup
					.get(dependency)
					.map(|target| (target.clone(), DependencyMatchKind::ModId))
					.or_else(|| {
						name_lookup
							.get(dependency)
							.map(|target| (target.clone(), DependencyMatchKind::DescriptorName))
					});
				match resolved {
					Some((target, kind)) => {
						edges.insert((mod_item.mod_id.clone(), target, Some(kind)));
					}
					None => {
						let missing_id = format!("missing:{dependency}");
						nodes.entry(missing_id.clone()).or_insert(ModDepNode {
							id: missing_id.clone(),
							kind: ModDepNodeKind::MissingDependency,
							name: dependency.clone(),
							mod_id: None,
						});
						edges.insert((mod_item.mod_id.clone(), missing_id, None));
					}
				}
			}
		}
	}

	ModDepsGraphArtifact {
		nodes: nodes.into_values().collect(),
		edges: edges
			.into_iter()
			.map(|(from, to, match_kind)| ModDepEdge {
				from,
				to,
				kind: "declares_dependency",
				match_kind,
			})
			.collect(),
	}
}

// ---------------------------------------------------------------------------
// Definition-level mod dependency artifact.
//
// Existing `mod-deps.json` only records edges declared in `descriptor.mod`'s
// `dependencies={}` block. That misses the much richer signal of which
// definitions one mod actually references in another mod (e.g. mod A's
// event triggers mod B's scripted_effect).
//
// This artifact emits one node per provider definition that some other
// enabled mod references, plus one edge per (referencing-mod,
// provider-definition) pair, aggregating each call site so the user can
// trace exactly what made the dependency real.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
struct DefinitionDepsRefSite {
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, Serialize)]
struct DefinitionDepsNode {
	id: String,
	mod_id: String,
	symbol_kind: String,
	name: String,
	path: String,
	line: usize,
}

#[derive(Clone, Debug, Serialize)]
struct DefinitionDepsEdge {
	from_mod_id: String,
	to_node_id: String,
	to_mod_id: String,
	symbol_kind: String,
	name: String,
	sites: Vec<DefinitionDepsRefSite>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct DefinitionDepsArtifact {
	nodes: Vec<DefinitionDepsNode>,
	edges: Vec<DefinitionDepsEdge>,
}

fn definition_deps_node_id(mod_id: &str, kind: SymbolKind, name: &str) -> String {
	format!("{mod_id}:{}:{name}", symbol_kind_text(kind))
}

fn build_workspace_definition_deps_artifact(
	state: &crate::runtime::RuntimeState,
) -> DefinitionDepsArtifact {
	let mut nodes = BTreeMap::<String, DefinitionDepsNode>::new();
	let mut edges = BTreeMap::<(String, String), DefinitionDepsEdge>::new();

	let mut by_symbol = HashMap::<(SymbolKind, String), Vec<usize>>::new();
	for (idx, def) in state.semantic_index.definitions.iter().enumerate() {
		by_symbol
			.entry((def.kind, def.name.clone()))
			.or_default()
			.push(idx);
	}

	for reference in &state.semantic_index.references {
		if !state.enabled_mod_ids.contains(&reference.mod_id) {
			continue;
		}
		let key = (reference.kind, reference.name.clone());
		let candidates = match by_symbol.get(&key) {
			Some(indices) if !indices.is_empty() => indices,
			_ => continue,
		};
		let provider_idx = state
			.winner_by_symbol
			.get(&key)
			.copied()
			.unwrap_or(candidates[0]);
		let Some(provider) = state.semantic_index.definitions.get(provider_idx) else {
			continue;
		};
		if provider.mod_id == reference.mod_id {
			continue;
		}
		if !state.enabled_mod_ids.contains(&provider.mod_id) {
			continue;
		}

		let node_id = definition_deps_node_id(&provider.mod_id, provider.kind, &provider.name);
		nodes
			.entry(node_id.clone())
			.or_insert_with(|| DefinitionDepsNode {
				id: node_id.clone(),
				mod_id: provider.mod_id.clone(),
				symbol_kind: symbol_kind_text(provider.kind).to_string(),
				name: provider.name.clone(),
				path: provider.path.to_string_lossy().replace('\\', "/"),
				line: provider.line,
			});

		let edge = edges
			.entry((reference.mod_id.clone(), node_id.clone()))
			.or_insert_with(|| DefinitionDepsEdge {
				from_mod_id: reference.mod_id.clone(),
				to_node_id: node_id.clone(),
				to_mod_id: provider.mod_id.clone(),
				symbol_kind: symbol_kind_text(provider.kind).to_string(),
				name: provider.name.clone(),
				sites: Vec::new(),
			});
		let site = DefinitionDepsRefSite {
			path: reference.path.to_string_lossy().replace('\\', "/"),
			line: reference.line,
			column: reference.column,
		};
		if !edge.sites.iter().any(|existing| existing == &site) {
			edge.sites.push(site);
		}
	}

	for edge in edges.values_mut() {
		edge.sites
			.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
	}

	DefinitionDepsArtifact {
		nodes: nodes.into_values().collect(),
		edges: edges.into_values().collect(),
	}
}

fn render_definition_deps_dot(graph: &DefinitionDepsArtifact) -> String {
	let mut lines = vec!["digraph foch_definition_deps {".to_string()];
	lines.push("  rankdir=LR;".to_string());

	let mut by_mod: BTreeMap<&str, Vec<&DefinitionDepsNode>> = BTreeMap::new();
	for node in &graph.nodes {
		by_mod.entry(node.mod_id.as_str()).or_default().push(node);
	}
	for (mod_id, mod_nodes) in &by_mod {
		lines.push(format!(
			"  subgraph \"cluster_{mod_id}\" {{ label=\"{mod_id}\"; style=dashed;"
		));
		for node in mod_nodes {
			lines.push(format!(
				"    \"{}\" [label=\"{}: {}\", shape=box];",
				node.id, node.symbol_kind, node.name
			));
		}
		lines.push("  }".to_string());
	}

	let referencing_mods: BTreeSet<&str> = graph
		.edges
		.iter()
		.map(|edge| edge.from_mod_id.as_str())
		.filter(|mod_id| !by_mod.contains_key(mod_id))
		.collect();
	for mod_id in referencing_mods {
		lines.push(format!(
			"  \"mod:{mod_id}\" [label=\"{mod_id}\", shape=ellipse, style=dashed];"
		));
	}

	for edge in &graph.edges {
		let from = format!("mod:{}", edge.from_mod_id);
		let count_label = if edge.sites.len() > 1 {
			format!(" ×{}", edge.sites.len())
		} else {
			String::new()
		};
		lines.push(format!(
			"  \"{from}\" -> \"{}\" [label=\"refs{count_label}\"];",
			edge.to_node_id
		));
	}
	lines.push("}".to_string());
	lines.join("\n")
}

fn filter_calls_graph_by_mod(graph: &CallsGraphArtifact, mod_id: &str) -> CallsGraphArtifact {
	let seed = graph
		.nodes
		.iter()
		.filter(|node| node.mod_id.as_deref() == Some(mod_id))
		.map(|node| node.id.clone())
		.collect::<HashSet<_>>();
	filter_calls_graph(graph, seed)
}

fn filter_calls_graph_by_root(
	graph: &CallsGraphArtifact,
	root: &GraphRootSelector,
) -> CallsGraphArtifact {
	let seed = graph
		.nodes
		.iter()
		.filter(|node| {
			node.symbol_kind.as_deref() == Some(symbol_kind_text(root.kind))
				&& node.name.as_deref().is_some_and(|name| {
					name == root.name
						|| name
							.rsplit_once("::")
							.is_some_and(|(_, local_name)| local_name == root.name)
				})
		})
		.map(|node| node.id.clone())
		.collect::<HashSet<_>>();
	let mut queue = seed.iter().cloned().collect::<VecDeque<_>>();
	let mut visited = seed.clone();
	while let Some(node_id) = queue.pop_front() {
		for edge in &graph.edges {
			let neighbor = if edge.from == node_id {
				Some(edge.to.clone())
			} else if edge.to == node_id {
				Some(edge.from.clone())
			} else {
				None
			};
			if let Some(next) = neighbor
				&& visited.insert(next.clone())
			{
				queue.push_back(next);
			}
		}
	}
	filter_calls_graph(graph, visited)
}

fn filter_calls_graph(graph: &CallsGraphArtifact, seed: HashSet<String>) -> CallsGraphArtifact {
	let mut related = seed.clone();
	for edge in &graph.edges {
		if seed.contains(&edge.from) || seed.contains(&edge.to) {
			related.insert(edge.from.clone());
			related.insert(edge.to.clone());
		}
	}
	CallsGraphArtifact {
		nodes: graph
			.nodes
			.iter()
			.filter(|node| related.contains(&node.id))
			.cloned()
			.collect(),
		edges: graph
			.edges
			.iter()
			.filter(|edge| related.contains(&edge.from) && related.contains(&edge.to))
			.cloned()
			.collect(),
	}
}

fn filter_mod_deps_graph_by_mod(
	graph: &ModDepsGraphArtifact,
	mod_id: &str,
) -> ModDepsGraphArtifact {
	let mut related = HashSet::from([mod_id.to_string()]);
	for edge in &graph.edges {
		if edge.from == mod_id || edge.to == mod_id {
			related.insert(edge.from.clone());
			related.insert(edge.to.clone());
		}
	}
	ModDepsGraphArtifact {
		nodes: graph
			.nodes
			.iter()
			.filter(|node| related.contains(&node.id))
			.cloned()
			.collect(),
		edges: graph
			.edges
			.iter()
			.filter(|edge| related.contains(&edge.from) && related.contains(&edge.to))
			.cloned()
			.collect(),
	}
}

fn definition_kind_for_status(status: OverlapStatus) -> CallsNodeKind {
	match status {
		OverlapStatus::None => CallsNodeKind::Definition,
		OverlapStatus::DiscardableBaseCopy => CallsNodeKind::DiscardableDefinition,
		OverlapStatus::MergeCandidate => CallsNodeKind::MergeCandidateDefinition,
		OverlapStatus::OvershadowConflict => CallsNodeKind::OvershadowConflictDefinition,
	}
}

fn definition_node_id(def_idx: usize) -> String {
	format!("def:{def_idx}")
}

fn file_node(mod_id: &str, path: &Path) -> CallsNode {
	CallsNode {
		id: format!("file:{mod_id}:{}", normalize_path(path)),
		kind: CallsNodeKind::File,
		symbol_kind: None,
		name: None,
		mod_id: Some(mod_id.to_string()),
		path: Some(normalize_path(path)),
		line: None,
		column: None,
	}
}

fn append_callsite_edge(
	edges: &mut BTreeMap<(u8, String, String), CallsEdge>,
	kind: CallsEdgeKind,
	from: String,
	to: String,
	callsite: CallsiteRecord,
	declared_dependency: Option<bool>,
	dependency_match_kind: Option<DependencyMatchKind>,
) {
	let discriminator = match kind {
		CallsEdgeKind::Calls => 0,
		CallsEdgeKind::UnresolvedCall => 1,
		CallsEdgeKind::Overrides => 2,
		CallsEdgeKind::DeclaredDependencyHint => 3,
	};
	let key = (discriminator, from.clone(), to.clone());
	let edge = edges.entry(key).or_insert(CallsEdge {
		kind,
		from,
		to,
		declared_dependency,
		dependency_match_kind,
		callsites: Vec::new(),
	});
	edge.callsites.push(callsite);
}

fn write_graph_pair<T: Serialize>(
	dir: &Path,
	stem: &str,
	json_value: &T,
	dot_value: String,
	format: GraphArtifactFormat,
) -> Result<(), Box<dyn std::error::Error>> {
	fs::create_dir_all(dir)?;
	if matches!(
		format,
		GraphArtifactFormat::Json | GraphArtifactFormat::Both
	) {
		fs::write(
			dir.join(format!("{stem}.json")),
			serde_json::to_vec_pretty(json_value)?,
		)?;
	}
	if matches!(format, GraphArtifactFormat::Dot | GraphArtifactFormat::Both) {
		fs::write(dir.join(format!("{stem}.dot")), dot_value)?;
	}
	Ok(())
}

fn render_calls_dot(graph: &CallsGraphArtifact) -> String {
	let mut lines = vec![
		"digraph foch_calls {".to_string(),
		"\trankdir=LR;".to_string(),
		"\tnode [fontname=\"monospace\"];".to_string(),
	];
	for node in &graph.nodes {
		let (shape, color) = match node.kind {
			CallsNodeKind::Definition => ("box", "lightskyblue"),
			CallsNodeKind::File => ("note", "lightgoldenrod1"),
			CallsNodeKind::Unresolved => ("diamond", "indianred1"),
			CallsNodeKind::DiscardableDefinition => ("box", "gray80"),
			CallsNodeKind::MergeCandidateDefinition => ("box", "khaki1"),
			CallsNodeKind::OvershadowConflictDefinition => ("box", "tomato"),
		};
		let label = [
			node.symbol_kind.as_deref().unwrap_or("file"),
			node.name.as_deref().unwrap_or(""),
			node.mod_id.as_deref().unwrap_or(""),
			node.path.as_deref().unwrap_or(""),
		]
		.into_iter()
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>()
		.join("\\n");
		lines.push(format!(
			"\t\"{}\" [shape={},style=filled,fillcolor=\"{}\",label=\"{}\"]",
			node.id,
			shape,
			color,
			escape_dot(&label)
		));
	}
	for edge in &graph.edges {
		let style = match edge.kind {
			CallsEdgeKind::Calls => "solid",
			CallsEdgeKind::UnresolvedCall => "dashed",
			CallsEdgeKind::Overrides => "dotted",
			CallsEdgeKind::DeclaredDependencyHint => "bold",
		};
		let label = match edge.kind {
			CallsEdgeKind::DeclaredDependencyHint => format!(
				"declared={} {:?}",
				edge.declared_dependency.unwrap_or(false),
				edge.dependency_match_kind
			),
			_ => format!("{:?}", edge.kind),
		};
		lines.push(format!(
			"\t\"{}\" -> \"{}\" [style={},label=\"{}\"]",
			edge.from,
			edge.to,
			style,
			escape_dot(&label)
		));
	}
	lines.push("}".to_string());
	lines.join("\n")
}

fn render_mod_deps_dot(graph: &ModDepsGraphArtifact) -> String {
	let mut lines = vec![
		"digraph foch_mod_deps {".to_string(),
		"\trankdir=LR;".to_string(),
		"\tnode [fontname=\"monospace\"];".to_string(),
	];
	for node in &graph.nodes {
		let (shape, color) = match node.kind {
			ModDepNodeKind::Mod => ("box", "lightskyblue"),
			ModDepNodeKind::BaseGame => ("box", "gray80"),
			ModDepNodeKind::MissingDependency => ("diamond", "indianred1"),
		};
		lines.push(format!(
			"\t\"{}\" [shape={},style=filled,fillcolor=\"{}\",label=\"{}\"]",
			node.id,
			shape,
			color,
			escape_dot(&node.name)
		));
	}
	for edge in &graph.edges {
		let label = edge
			.match_kind
			.map(|kind| format!("{:?}", kind))
			.unwrap_or_else(|| edge.kind.to_string());
		lines.push(format!(
			"\t\"{}\" -> \"{}\" [label=\"{}\"]",
			edge.from,
			edge.to,
			escape_dot(&label)
		));
	}
	lines.push("}".to_string());
	lines.join("\n")
}

fn sanitize_root_name(root: &GraphRootSelector) -> String {
	format!(
		"{}-{}",
		symbol_kind_text(root.kind),
		root.name
			.chars()
			.map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
			.collect::<String>()
	)
}

fn symbol_kind_text(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "scripted_effect",
		SymbolKind::ScriptedTrigger => "scripted_trigger",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn escape_dot(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod definition_deps_tests {
	use super::*;
	use crate::runtime::RuntimeState;
	use foch_core::model::{ScopeType, SymbolDefinition, SymbolKind, SymbolReference};
	use std::collections::{HashMap, HashSet};
	use std::path::PathBuf;

	fn definition(
		mod_id: &str,
		kind: SymbolKind,
		name: &str,
		path: &str,
		line: usize,
	) -> SymbolDefinition {
		SymbolDefinition {
			kind,
			name: name.to_string(),
			module: "test".to_string(),
			local_name: name.to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from(path),
			line,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Unknown,
			inferred_this_type: ScopeType::Unknown,
			inferred_this_mask: 0,
			inferred_from_mask: 0,
			inferred_root_mask: 0,
			required_params: Vec::new(),
			optional_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		}
	}

	fn reference(
		mod_id: &str,
		kind: SymbolKind,
		name: &str,
		path: &str,
		line: usize,
	) -> SymbolReference {
		SymbolReference {
			kind,
			name: name.to_string(),
			module: "test".to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from(path),
			line,
			column: 1,
			scope_id: 0,
			provided_params: Vec::new(),
			param_bindings: Vec::new(),
		}
	}

	fn state_with(
		definitions: Vec<SymbolDefinition>,
		references: Vec<SymbolReference>,
		enabled_mod_ids: Vec<&str>,
		winners: Vec<((SymbolKind, &str), usize)>,
	) -> RuntimeState {
		let semantic_index = foch_core::model::SemanticIndex {
			definitions,
			references,
			..Default::default()
		};
		let enabled_mod_ids: HashSet<String> =
			enabled_mod_ids.into_iter().map(|s| s.to_string()).collect();
		let winner_by_symbol: HashMap<(SymbolKind, String), usize> = winners
			.into_iter()
			.map(|((kind, name), idx)| ((kind, name.to_string()), idx))
			.collect();
		RuntimeState {
			semantic_index,
			definitions: Vec::new(),
			overlap_status_by_def: HashMap::new(),
			winner_by_symbol,
			dependency_hints: HashMap::new(),
			scope_definition_map: HashMap::new(),
			enabled_mod_ids,
			base_game_mod_id: None,
		}
	}

	#[test]
	fn definition_deps_emits_cross_mod_edge_with_call_site() {
		// mod-a defines my_effect; mod-b references it.
		let state = state_with(
			vec![definition(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"common/scripted_effects/a.txt",
				10,
			)],
			vec![reference(
				"mod-b",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"events/b_event.txt",
				42,
			)],
			vec!["mod-a", "mod-b"],
			vec![((SymbolKind::ScriptedEffect, "my_effect"), 0)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert_eq!(artifact.nodes.len(), 1);
		let node = &artifact.nodes[0];
		assert_eq!(node.mod_id, "mod-a");
		assert_eq!(node.symbol_kind, "scripted_effect");
		assert_eq!(node.name, "my_effect");
		assert_eq!(artifact.edges.len(), 1);
		let edge = &artifact.edges[0];
		assert_eq!(edge.from_mod_id, "mod-b");
		assert_eq!(edge.to_mod_id, "mod-a");
		assert_eq!(edge.symbol_kind, "scripted_effect");
		assert_eq!(edge.name, "my_effect");
		assert_eq!(edge.sites.len(), 1);
		assert_eq!(edge.sites[0].path, "events/b_event.txt");
		assert_eq!(edge.sites[0].line, 42);
	}

	#[test]
	fn definition_deps_skips_intra_mod_references() {
		let state = state_with(
			vec![definition(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"common/scripted_effects/a.txt",
				10,
			)],
			vec![reference(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"events/a_event.txt",
				42,
			)],
			vec!["mod-a"],
			vec![((SymbolKind::ScriptedEffect, "my_effect"), 0)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert!(artifact.nodes.is_empty());
		assert!(artifact.edges.is_empty());
	}

	#[test]
	fn definition_deps_skips_unresolved_references() {
		let state = state_with(
			Vec::new(),
			vec![reference(
				"mod-b",
				SymbolKind::Event,
				"missing.event",
				"events/b.txt",
				1,
			)],
			vec!["mod-b"],
			Vec::new(),
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert!(artifact.nodes.is_empty());
		assert!(artifact.edges.is_empty());
	}

	#[test]
	fn definition_deps_skips_disabled_mod_references() {
		let state = state_with(
			vec![definition(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"a.txt",
				1,
			)],
			vec![reference(
				"mod-disabled",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"x.txt",
				1,
			)],
			vec!["mod-a"], // mod-disabled NOT in enabled set
			vec![((SymbolKind::ScriptedEffect, "my_effect"), 0)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert!(artifact.edges.is_empty());
	}

	#[test]
	fn definition_deps_aggregates_multiple_sites_per_edge() {
		let state = state_with(
			vec![definition(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"shared",
				"a.txt",
				1,
			)],
			vec![
				reference("mod-b", SymbolKind::ScriptedEffect, "shared", "b1.txt", 5),
				reference("mod-b", SymbolKind::ScriptedEffect, "shared", "b1.txt", 22),
				reference("mod-b", SymbolKind::ScriptedEffect, "shared", "b2.txt", 99),
				// duplicate site should be deduped
				reference("mod-b", SymbolKind::ScriptedEffect, "shared", "b1.txt", 5),
			],
			vec!["mod-a", "mod-b"],
			vec![((SymbolKind::ScriptedEffect, "shared"), 0)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert_eq!(artifact.edges.len(), 1);
		let edge = &artifact.edges[0];
		assert_eq!(edge.sites.len(), 3);
		// stable ordering: by path then line
		assert_eq!(edge.sites[0].path, "b1.txt");
		assert_eq!(edge.sites[0].line, 5);
		assert_eq!(edge.sites[1].path, "b1.txt");
		assert_eq!(edge.sites[1].line, 22);
		assert_eq!(edge.sites[2].path, "b2.txt");
		assert_eq!(edge.sites[2].line, 99);
	}

	#[test]
	fn definition_deps_uses_winner_when_definition_overridden() {
		// mod-a and mod-b both define `same_effect`; mod-c references it.
		// winner_by_symbol points at mod-b (precedence 1) — edge should
		// terminate at mod-b's node, not mod-a's.
		let state = state_with(
			vec![
				definition(
					"mod-a",
					SymbolKind::ScriptedEffect,
					"same_effect",
					"a.txt",
					1,
				),
				definition(
					"mod-b",
					SymbolKind::ScriptedEffect,
					"same_effect",
					"b.txt",
					1,
				),
			],
			vec![reference(
				"mod-c",
				SymbolKind::ScriptedEffect,
				"same_effect",
				"c.txt",
				9,
			)],
			vec!["mod-a", "mod-b", "mod-c"],
			vec![((SymbolKind::ScriptedEffect, "same_effect"), 1)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		assert_eq!(artifact.edges.len(), 1);
		assert_eq!(artifact.edges[0].to_mod_id, "mod-b");
	}

	#[test]
	fn render_definition_deps_dot_emits_cluster_per_provider_mod() {
		let state = state_with(
			vec![definition(
				"mod-a",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"a.txt",
				1,
			)],
			vec![reference(
				"mod-b",
				SymbolKind::ScriptedEffect,
				"my_effect",
				"b.txt",
				5,
			)],
			vec!["mod-a", "mod-b"],
			vec![((SymbolKind::ScriptedEffect, "my_effect"), 0)],
		);
		let artifact = build_workspace_definition_deps_artifact(&state);
		let dot = render_definition_deps_dot(&artifact);
		assert!(dot.starts_with("digraph foch_definition_deps {"));
		assert!(dot.contains("subgraph \"cluster_mod-a\""));
		assert!(dot.contains("scripted_effect: my_effect"));
		assert!(dot.contains("\"mod:mod-b\" -> \"mod-a:scripted_effect:my_effect\""));
	}
}
