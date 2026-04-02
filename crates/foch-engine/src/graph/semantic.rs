use super::model::{GraphBuildOptions, GraphBuildSummary};
use crate::request::CheckRequest;
use crate::runtime::{
	RuntimeState, build_runtime_state_from_workspace, nearest_enclosing_definition,
};
use crate::workspace::{ResolvedWorkspace, normalize_relative_path, resolve_workspace};
use foch_core::model::{
	AliasUsage, KeyUsage, ResourceReference, ScalarAssignment, ScopeKind, ScopeNode,
	SymbolReference,
};
use foch_language::analyzer::content_family::GameProfile;
use foch_language::analyzer::eu4_profile::eu4_profile;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const SEMANTIC_GRAPH_PROGRESS_TARGET: &str = "foch::graph::progress";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SemanticGraphNodeKind {
	Family,
	ContributorFile,
	Definition,
	SemanticBlock,
	ExternalTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum SemanticGraphEdgeKind {
	Contains,
	Overrides,
	ReferencesIntraFamily,
	ReferencesCrossFamily,
	ReferencesExternal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExternalTargetKind {
	Family,
	Localisation,
	AssetUi,
	Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SemanticGraphViewDefaults {
	pub show_contains: bool,
	pub show_overrides: bool,
	pub show_intra_family_refs: bool,
	pub show_cross_family_refs: bool,
	pub show_external_refs: bool,
	pub show_nested_blocks: bool,
	pub show_evidence_leaves: bool,
	pub show_unreferenced_blocks: bool,
	pub show_unreferenced_definitions: bool,
}

impl Default for SemanticGraphViewDefaults {
	fn default() -> Self {
		Self {
			show_contains: true,
			show_overrides: true,
			show_intra_family_refs: false,
			show_cross_family_refs: false,
			show_external_refs: false,
			show_nested_blocks: false,
			show_evidence_leaves: false,
			show_unreferenced_blocks: false,
			show_unreferenced_definitions: true,
		}
	}
}

#[derive(Clone, Debug, Default, Serialize)]
struct SemanticNodeEvidence {
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	resource_references: Vec<SemanticEvidenceItem>,
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	scalar_assignments: Vec<SemanticEvidenceItem>,
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	symbol_references: Vec<SemanticEvidenceItem>,
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	key_usages: Vec<SemanticEvidenceItem>,
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	alias_usages: Vec<SemanticEvidenceItem>,
}

#[derive(Clone, Debug, Serialize)]
struct SemanticEvidenceItem {
	label: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	value: Option<String>,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SemanticGraphNode {
	id: String,
	kind: SemanticGraphNodeKind,
	label: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	mod_id: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	path: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	line: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	column: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	precedence: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	is_base_game: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	parse_ok_hint: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	scope_id: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	scope_kind: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	definition_key: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	definition_value: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	target_kind: Option<ExternalTargetKind>,
	referenced: bool,
	resource_reference_count: usize,
	scalar_assignment_count: usize,
	symbol_reference_count: usize,
	key_usage_count: usize,
	alias_usage_count: usize,
	#[serde(skip_serializing_if = "is_false")]
	default_visible: bool,
	#[serde(flatten)]
	evidence: SemanticNodeEvidence,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SemanticGraphEdge {
	kind: SemanticGraphEdgeKind,
	from: String,
	to: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	label: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	count: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	sample: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SemanticGraphArtifact {
	family_id: String,
	defaults: SemanticGraphViewDefaults,
	nodes: Vec<SemanticGraphNode>,
	edges: Vec<SemanticGraphEdge>,
}

#[derive(Clone, Debug)]
struct FamilyContributor {
	mod_id: String,
	relative_path: String,
	absolute_path: PathBuf,
	precedence: usize,
	is_base_game: bool,
	parse_ok_hint: Option<bool>,
}

#[derive(Clone, Debug)]
struct DefinitionSeed {
	node_id: String,
	label: String,
	mod_id: String,
	relative_path: String,
	line: usize,
	column: usize,
	precedence: usize,
	definition_key: String,
	definition_value: String,
}

struct ResourceReferenceContext<'a> {
	family_id: &'a str,
	contributor_ids: &'a HashMap<(String, String), String>,
	resource_definition_lines: &'a HashMap<(String, String), Vec<&'a DefinitionSeed>>,
	best_definition_by_identity: &'a HashMap<(String, String), String>,
	family_node_id: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReferenceTargetClass {
	SameFamily,
	OtherFamily(&'static str),
	Localisation,
	AssetUi,
	Unknown,
}

pub(crate) fn run_semantic_graph_with_options(
	request: CheckRequest,
	out_dir: &Path,
	options: GraphBuildOptions,
) -> Result<GraphBuildSummary, Box<dyn std::error::Error>> {
	let family_id = options
		.family
		.clone()
		.ok_or("semantic graph mode requires --family")?;
	let _span = tracing::debug_span!("semantic_graph_run", family_id = %family_id).entered();
	let workspace = run_progress_stage(
		&family_id,
		"resolve workspace",
		|| resolve_workspace(&request, options.include_game_base).map_err(|err| err.message),
		summarize_workspace,
	)
	.map_err(boxed_err)?;
	let state = run_progress_stage(
		&family_id,
		"build runtime state",
		|| build_runtime_state_from_workspace(&workspace),
		summarize_runtime_state,
	)
	.map_err(boxed_err)?;
	let artifact = run_progress_stage(
		&family_id,
		"build semantic artifact",
		|| build_semantic_graph_artifact(&workspace, &state, &family_id),
		summarize_artifact,
	)?;
	let artifact_dir = out_dir.join("semantic").join(&family_id);
	fs::create_dir_all(&artifact_dir)?;
	let json_path = artifact_dir.join("semantic-graph.json");
	run_progress_stage(
		&family_id,
		"write semantic graph json",
		|| -> Result<(), Box<dyn std::error::Error>> {
			fs::write(&json_path, serde_json::to_vec_pretty(&artifact)?)?;
			Ok(())
		},
		|()| String::new(),
	)?;
	let html_path = artifact_dir.join("index.html");
	run_progress_stage(
		&family_id,
		"write semantic graph html",
		|| -> Result<(), Box<dyn std::error::Error>> {
			fs::write(&html_path, render_semantic_graph_html(&artifact)?)?;
			Ok(())
		},
		|()| String::new(),
	)?;
	Ok(GraphBuildSummary {
		out_dir: out_dir.to_path_buf(),
		semantic_written: true,
		..GraphBuildSummary::default()
	})
}

fn boxed_err(message: String) -> Box<dyn std::error::Error> {
	message.into()
}

fn run_progress_stage<T, E, F, S>(
	family_id: &str,
	stage: &'static str,
	f: F,
	summarize: S,
) -> Result<T, E>
where
	F: FnOnce() -> Result<T, E>,
	S: FnOnce(&T) -> String,
	E: std::fmt::Display,
{
	let _span =
		tracing::debug_span!("semantic_graph_stage", family_id = %family_id, stage).entered();
	tracing::info!(
		target: SEMANTIC_GRAPH_PROGRESS_TARGET,
		family_id = %family_id,
		"semantic graph {stage}: start"
	);
	let started = Instant::now();
	let result = f();
	let elapsed_ms = started.elapsed().as_millis() as u64;
	match &result {
		Ok(value) => {
			let summary = summarize(value);
			if summary.is_empty() {
				tracing::info!(
					target: SEMANTIC_GRAPH_PROGRESS_TARGET,
					family_id = %family_id,
					elapsed_ms,
					"semantic graph {stage}: done"
				);
			} else {
				tracing::info!(
					target: SEMANTIC_GRAPH_PROGRESS_TARGET,
					family_id = %family_id,
					elapsed_ms,
					summary = %summary,
					"semantic graph {stage}: done"
				);
			}
		}
		Err(err) => {
			tracing::error!(
				target: SEMANTIC_GRAPH_PROGRESS_TARGET,
				family_id = %family_id,
				elapsed_ms,
				error = %err,
				"semantic graph {stage}: failed"
			);
		}
	}
	result
}

fn summarize_workspace(workspace: &ResolvedWorkspace) -> String {
	let enabled_mod_count = workspace
		.mods
		.iter()
		.filter(|item| item.entry.enabled)
		.count();
	let inventory_file_count = workspace.file_inventory.len();
	let contributor_file_count = workspace
		.file_inventory
		.values()
		.map(|contributors| contributors.len())
		.sum::<usize>();
	tracing::debug!(
		family_inventory_files = inventory_file_count,
		family_contributor_files = contributor_file_count,
		enabled_mod_count,
		has_base_game = workspace.installed_base_snapshot.is_some(),
		"semantic graph workspace resolved"
	);
	format!(
		"enabled_mods={enabled_mod_count} inventory_files={inventory_file_count} contributors={contributor_file_count}"
	)
}

fn summarize_runtime_state(state: &RuntimeState) -> String {
	let document_count = state.semantic_index.documents.len();
	let definition_count = state.definitions.len();
	let reference_count = state.semantic_index.references.len();
	let resource_reference_count = state.semantic_index.resource_references.len();
	tracing::debug!(
		document_count,
		definition_count,
		reference_count,
		resource_reference_count,
		"semantic graph runtime state built"
	);
	format!(
		"documents={document_count} definitions={definition_count} references={reference_count} resource_refs={resource_reference_count}"
	)
}

fn summarize_artifact(artifact: &SemanticGraphArtifact) -> String {
	let node_count = artifact.nodes.len();
	let edge_count = artifact.edges.len();
	tracing::debug!(node_count, edge_count, "semantic graph artifact built");
	format!("nodes={node_count} edges={edge_count}")
}

fn build_semantic_graph_artifact(
	workspace: &ResolvedWorkspace,
	state: &RuntimeState,
	family_id: &str,
) -> Result<SemanticGraphArtifact, Box<dyn std::error::Error>> {
	let profile = eu4_profile();
	let Some(_descriptor) = profile.descriptor_for_root_family(family_id) else {
		return Err(format!("unknown content family {family_id}").into());
	};
	let contributors = collect_family_contributors(workspace, family_id);
	if contributors.is_empty() {
		return Err(format!("no contributors found for content family {family_id}").into());
	}

	let mut nodes = BTreeMap::<String, SemanticGraphNode>::new();
	let mut edges = BTreeMap::<(u8, String, String, String), SemanticGraphEdge>::new();
	let mut contributor_ids = HashMap::<(String, String), String>::new();
	let mut contributor_lookup = HashMap::<(String, String), &FamilyContributor>::new();

	let family_node_id = format!("family:{family_id}");
	nodes.insert(
		family_node_id.clone(),
		SemanticGraphNode {
			id: family_node_id.clone(),
			kind: SemanticGraphNodeKind::Family,
			label: family_id.to_string(),
			mod_id: None,
			path: None,
			line: None,
			column: None,
			precedence: None,
			is_base_game: None,
			parse_ok_hint: None,
			scope_id: None,
			scope_kind: None,
			definition_key: None,
			definition_value: None,
			target_kind: None,
			referenced: true,
			resource_reference_count: 0,
			scalar_assignment_count: 0,
			symbol_reference_count: 0,
			key_usage_count: 0,
			alias_usage_count: 0,
			default_visible: true,
			evidence: SemanticNodeEvidence::default(),
		},
	);

	for contributor in &contributors {
		let file_node_id =
			contributor_file_node_id(&contributor.mod_id, &contributor.relative_path);
		contributor_ids.insert(
			(
				contributor.mod_id.clone(),
				contributor.relative_path.clone(),
			),
			file_node_id.clone(),
		);
		contributor_lookup.insert(
			(
				contributor.mod_id.clone(),
				contributor.relative_path.clone(),
			),
			contributor,
		);
		nodes.insert(
			file_node_id.clone(),
			SemanticGraphNode {
				id: file_node_id.clone(),
				kind: SemanticGraphNodeKind::ContributorFile,
				label: contributor.relative_path.clone(),
				mod_id: Some(contributor.mod_id.clone()),
				path: Some(contributor.relative_path.clone()),
				line: None,
				column: None,
				precedence: Some(contributor.precedence),
				is_base_game: Some(contributor.is_base_game),
				parse_ok_hint: contributor.parse_ok_hint,
				scope_id: None,
				scope_kind: None,
				definition_key: None,
				definition_value: None,
				target_kind: None,
				referenced: true,
				resource_reference_count: 0,
				scalar_assignment_count: 0,
				symbol_reference_count: 0,
				key_usage_count: 0,
				alias_usage_count: 0,
				default_visible: true,
				evidence: SemanticNodeEvidence::default(),
			},
		);
		insert_edge(
			&mut edges,
			SemanticGraphEdge {
				kind: SemanticGraphEdgeKind::Contains,
				from: family_node_id.clone(),
				to: file_node_id,
				label: None,
				count: None,
				sample: None,
			},
		);
	}

	let definition_seeds = collect_definition_seeds(state, family_id, &contributor_lookup);
	let mut best_definition_by_identity = HashMap::<(String, String), String>::new();
	let mut definitions_by_file = HashMap::<(String, String), Vec<DefinitionSeed>>::new();
	for seed in &definition_seeds {
		let Some(file_node_id) = contributor_ids
			.get(&(seed.mod_id.clone(), seed.relative_path.clone()))
			.cloned()
		else {
			continue;
		};
		nodes.insert(
			seed.node_id.clone(),
			SemanticGraphNode {
				id: seed.node_id.clone(),
				kind: SemanticGraphNodeKind::Definition,
				label: seed.label.clone(),
				mod_id: Some(seed.mod_id.clone()),
				path: Some(seed.relative_path.clone()),
				line: Some(seed.line),
				column: Some(seed.column),
				precedence: Some(seed.precedence),
				is_base_game: None,
				parse_ok_hint: None,
				scope_id: None,
				scope_kind: None,
				definition_key: Some(seed.definition_key.clone()),
				definition_value: Some(seed.definition_value.clone()),
				target_kind: None,
				referenced: false,
				resource_reference_count: 0,
				scalar_assignment_count: 0,
				symbol_reference_count: 0,
				key_usage_count: 0,
				alias_usage_count: 0,
				default_visible: true,
				evidence: SemanticNodeEvidence::default(),
			},
		);
		insert_edge(
			&mut edges,
			SemanticGraphEdge {
				kind: SemanticGraphEdgeKind::Contains,
				from: file_node_id,
				to: seed.node_id.clone(),
				label: None,
				count: None,
				sample: None,
			},
		);
		definitions_by_file
			.entry((seed.mod_id.clone(), seed.relative_path.clone()))
			.or_default()
			.push(seed.clone());
		let identity = (seed.definition_key.clone(), seed.definition_value.clone());
		match best_definition_by_identity.get(&identity) {
			Some(existing) => {
				let existing_precedence = nodes
					.get(existing)
					.and_then(|item| item.precedence)
					.unwrap_or_default();
				if seed.precedence >= existing_precedence {
					best_definition_by_identity.insert(identity, seed.node_id.clone());
				}
			}
			None => {
				best_definition_by_identity.insert(identity, seed.node_id.clone());
			}
		}
	}

	for seeds in definitions_by_file.values_mut() {
		seeds.sort_by_key(|item| (item.line, item.column, item.label.clone()));
	}

	let mut override_groups = BTreeMap::<(String, String), Vec<&DefinitionSeed>>::new();
	for seed in &definition_seeds {
		override_groups
			.entry((seed.definition_key.clone(), seed.definition_value.clone()))
			.or_default()
			.push(seed);
	}
	for seeds in override_groups.values_mut() {
		seeds.sort_by_key(|item| (item.precedence, item.line, item.column));
		for pair in seeds.windows(2) {
			let from = pair[0];
			let to = pair[1];
			insert_edge(
				&mut edges,
				SemanticGraphEdge {
					kind: SemanticGraphEdgeKind::Overrides,
					from: from.node_id.clone(),
					to: to.node_id.clone(),
					label: None,
					count: None,
					sample: None,
				},
			);
		}
	}

	for relative_path in contributors
		.iter()
		.map(|item| item.relative_path.clone())
		.collect::<BTreeSet<_>>()
	{
		let mut same_path = contributors
			.iter()
			.filter(|item| item.relative_path == relative_path)
			.collect::<Vec<_>>();
		same_path.sort_by_key(|item| item.precedence);
		for pair in same_path.windows(2) {
			let from = contributor_file_node_id(&pair[0].mod_id, &pair[0].relative_path);
			let to = contributor_file_node_id(&pair[1].mod_id, &pair[1].relative_path);
			insert_edge(
				&mut edges,
				SemanticGraphEdge {
					kind: SemanticGraphEdgeKind::Overrides,
					from,
					to,
					label: None,
					count: None,
					sample: None,
				},
			);
		}
	}

	let mut resource_definition_lines = HashMap::<(String, String), Vec<&DefinitionSeed>>::new();
	for seed in &definition_seeds {
		resource_definition_lines
			.entry((seed.mod_id.clone(), seed.relative_path.clone()))
			.or_default()
			.push(seed);
	}
	for seeds in resource_definition_lines.values_mut() {
		seeds.sort_by_key(|item| (item.line, item.column));
	}

	let block_attachments = build_block_nodes(
		state,
		family_id,
		&contributor_ids,
		&resource_definition_lines,
		&mut nodes,
		&mut edges,
	);

	attach_scalar_assignments(
		state,
		family_id,
		&contributor_ids,
		&block_attachments,
		&mut nodes,
	);
	attach_key_usages(
		state,
		family_id,
		&contributor_ids,
		&block_attachments,
		&mut nodes,
	);
	attach_alias_usages(
		state,
		family_id,
		&contributor_ids,
		&block_attachments,
		&mut nodes,
	);
	attach_symbol_references(
		state,
		family_id,
		&contributor_ids,
		&block_attachments,
		&mut nodes,
	);
	attach_resource_references(
		state,
		ResourceReferenceContext {
			family_id,
			contributor_ids: &contributor_ids,
			resource_definition_lines: &resource_definition_lines,
			best_definition_by_identity: &best_definition_by_identity,
			family_node_id: &family_node_id,
		},
		&mut nodes,
		&mut edges,
	);
	mark_referenced_nodes(&mut nodes, &edges);

	let artifact = SemanticGraphArtifact {
		family_id: family_id.to_string(),
		defaults: SemanticGraphViewDefaults::default(),
		nodes: nodes.into_values().collect(),
		edges: edges.into_values().collect(),
	};
	Ok(artifact)
}

fn collect_family_contributors(
	workspace: &ResolvedWorkspace,
	family_id: &str,
) -> Vec<FamilyContributor> {
	let profile = eu4_profile();
	let mut contributors = Vec::new();
	for (relative_path, items) in &workspace.file_inventory {
		let Some(descriptor) = profile.classify_content_family(Path::new(relative_path)) else {
			continue;
		};
		if descriptor.id != family_id {
			continue;
		}
		for item in items {
			contributors.push(FamilyContributor {
				mod_id: item.mod_id.clone(),
				relative_path: relative_path.clone(),
				absolute_path: item.absolute_path.clone(),
				precedence: item.precedence,
				is_base_game: item.is_base_game,
				parse_ok_hint: item.parse_ok_hint,
			});
		}
	}
	contributors.sort_by_key(|item| {
		(
			item.relative_path.clone(),
			item.precedence,
			item.mod_id.clone(),
			item.absolute_path.clone(),
		)
	});
	contributors
}

fn collect_definition_seeds(
	state: &RuntimeState,
	family_id: &str,
	contributor_lookup: &HashMap<(String, String), &FamilyContributor>,
) -> Vec<DefinitionSeed> {
	let mut seeds = BTreeMap::<String, DefinitionSeed>::new();
	for definition in &state.semantic_index.definitions {
		let relative_path = normalize_relative_path(&definition.path);
		if !contributor_lookup.contains_key(&(definition.mod_id.clone(), relative_path.clone())) {
			continue;
		}
		let precedence = contributor_lookup
			.get(&(definition.mod_id.clone(), relative_path.clone()))
			.map(|item| item.precedence)
			.unwrap_or_default();
		let seed = DefinitionSeed {
			node_id: format!(
				"definition:symbol:{}:{}:{}:{}:{}",
				symbol_kind_text(definition.kind),
				definition.mod_id,
				relative_path,
				definition.line,
				definition.name
			),
			label: definition.name.clone(),
			mod_id: definition.mod_id.clone(),
			relative_path,
			line: definition.line,
			column: definition.column,
			precedence,
			definition_key: format!("symbol:{}", symbol_kind_text(definition.kind)),
			definition_value: definition.name.clone(),
		};
		seeds.insert(seed.node_id.clone(), seed);
	}

	for reference in &state.semantic_index.resource_references {
		let relative_path = normalize_relative_path(&reference.path);
		if !contributor_lookup.contains_key(&(reference.mod_id.clone(), relative_path.clone())) {
			continue;
		}
		let Some(target_family) = definition_reference_family(&reference.key) else {
			continue;
		};
		if target_family != family_id {
			continue;
		}
		let precedence = contributor_lookup
			.get(&(reference.mod_id.clone(), relative_path.clone()))
			.map(|item| item.precedence)
			.unwrap_or_default();
		let seed = DefinitionSeed {
			node_id: format!(
				"definition:resource:{}:{}:{}:{}:{}",
				reference.key, reference.mod_id, relative_path, reference.line, reference.value
			),
			label: reference.value.clone(),
			mod_id: reference.mod_id.clone(),
			relative_path,
			line: reference.line,
			column: reference.column,
			precedence,
			definition_key: reference.key.clone(),
			definition_value: reference.value.clone(),
		};
		seeds.insert(seed.node_id.clone(), seed);
	}

	seeds.into_values().collect()
}

fn build_block_nodes(
	state: &RuntimeState,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	resource_definition_lines: &HashMap<(String, String), Vec<&DefinitionSeed>>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	edges: &mut BTreeMap<(u8, String, String, String), SemanticGraphEdge>,
) -> HashMap<usize, String> {
	let mut attachments = HashMap::new();
	for scope in &state.semantic_index.scopes {
		if scope.kind == ScopeKind::File {
			continue;
		}
		let relative_path = normalize_relative_path(&scope.path);
		let profile = eu4_profile();
		let Some(descriptor) = profile.classify_content_family(Path::new(&relative_path)) else {
			continue;
		};
		if descriptor.id != family_id {
			continue;
		}
		let Some(file_node_id) = contributor_ids
			.get(&(scope.mod_id.clone(), relative_path.clone()))
			.cloned()
		else {
			continue;
		};
		let node_id = format!("block:{}:{}:{}", scope.mod_id, relative_path, scope.id);
		let parent_id = definition_parent_for_scope(state, scope, resource_definition_lines, nodes)
			.unwrap_or(file_node_id);
		nodes.insert(
			node_id.clone(),
			SemanticGraphNode {
				id: node_id.clone(),
				kind: SemanticGraphNodeKind::SemanticBlock,
				label: format!("{} @ {}", scope_kind_text(scope.kind), scope.span.line),
				mod_id: Some(scope.mod_id.clone()),
				path: Some(relative_path.clone()),
				line: Some(scope.span.line),
				column: Some(scope.span.column),
				precedence: nodes.get(&parent_id).and_then(|item| item.precedence),
				is_base_game: None,
				parse_ok_hint: None,
				scope_id: Some(scope.id),
				scope_kind: Some(scope_kind_text(scope.kind).to_string()),
				definition_key: None,
				definition_value: None,
				target_kind: None,
				referenced: false,
				resource_reference_count: 0,
				scalar_assignment_count: 0,
				symbol_reference_count: 0,
				key_usage_count: 0,
				alias_usage_count: 0,
				default_visible: false,
				evidence: SemanticNodeEvidence::default(),
			},
		);
		insert_edge(
			edges,
			SemanticGraphEdge {
				kind: SemanticGraphEdgeKind::Contains,
				from: parent_id,
				to: node_id.clone(),
				label: None,
				count: None,
				sample: None,
			},
		);
		attachments.insert(scope.id, node_id);
	}
	attachments
}

fn definition_parent_for_scope(
	state: &RuntimeState,
	scope: &ScopeNode,
	resource_definition_lines: &HashMap<(String, String), Vec<&DefinitionSeed>>,
	nodes: &BTreeMap<String, SemanticGraphNode>,
) -> Option<String> {
	if let Some(def_idx) = nearest_enclosing_definition(state, scope.id)
		&& let Some(definition) = state.semantic_index.definitions.get(def_idx)
	{
		let relative_path = normalize_relative_path(&definition.path);
		let node_id = format!(
			"definition:symbol:{}:{}:{}:{}:{}",
			symbol_kind_text(definition.kind),
			definition.mod_id,
			relative_path,
			definition.line,
			definition.name
		);
		if nodes.contains_key(&node_id) {
			return Some(node_id);
		}
	}

	let relative_path = normalize_relative_path(&scope.path);
	resource_definition_lines
		.get(&(scope.mod_id.clone(), relative_path))
		.and_then(|items| {
			items
				.iter()
				.rev()
				.find(|item| item.line <= scope.span.line)
				.map(|item| item.node_id.clone())
		})
}

fn attach_scalar_assignments(
	state: &RuntimeState,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	block_attachments: &HashMap<usize, String>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
) {
	for item in &state.semantic_index.scalar_assignments {
		if let Some(node_id) = attachment_node_for_scoped_item(
			&item.mod_id,
			&item.path,
			item.scope_id,
			family_id,
			contributor_ids,
			block_attachments,
		) {
			push_scalar_evidence(nodes, &node_id, item);
		}
	}
}

fn attach_key_usages(
	state: &RuntimeState,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	block_attachments: &HashMap<usize, String>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
) {
	for item in &state.semantic_index.key_usages {
		if let Some(node_id) = attachment_node_for_scoped_item(
			&item.mod_id,
			&item.path,
			item.scope_id,
			family_id,
			contributor_ids,
			block_attachments,
		) {
			push_key_usage_evidence(nodes, &node_id, item);
		}
	}
}

fn attach_alias_usages(
	state: &RuntimeState,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	block_attachments: &HashMap<usize, String>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
) {
	for item in &state.semantic_index.alias_usages {
		if let Some(node_id) = attachment_node_for_scoped_item(
			&item.mod_id,
			&item.path,
			item.scope_id,
			family_id,
			contributor_ids,
			block_attachments,
		) {
			push_alias_usage_evidence(nodes, &node_id, item);
		}
	}
}

fn attach_symbol_references(
	state: &RuntimeState,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	block_attachments: &HashMap<usize, String>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
) {
	for item in &state.semantic_index.references {
		if let Some(node_id) = attachment_node_for_scoped_item(
			&item.mod_id,
			&item.path,
			item.scope_id,
			family_id,
			contributor_ids,
			block_attachments,
		) {
			push_symbol_reference_evidence(nodes, &node_id, item);
		}
	}
}

fn attachment_node_for_scoped_item(
	mod_id: &str,
	path: &Path,
	scope_id: usize,
	family_id: &str,
	contributor_ids: &HashMap<(String, String), String>,
	block_attachments: &HashMap<usize, String>,
) -> Option<String> {
	let relative_path = normalize_relative_path(path);
	let profile = eu4_profile();
	let descriptor = profile.classify_content_family(Path::new(&relative_path))?;
	if descriptor.id != family_id {
		return None;
	}
	if let Some(node_id) = block_attachments.get(&scope_id) {
		return Some(node_id.clone());
	}
	contributor_ids
		.get(&(mod_id.to_string(), relative_path))
		.cloned()
}

fn attach_resource_references(
	state: &RuntimeState,
	ctx: ResourceReferenceContext<'_>,
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	edges: &mut BTreeMap<(u8, String, String, String), SemanticGraphEdge>,
) {
	let mut aggregate_edges =
		HashMap::<(SemanticGraphEdgeKind, String, String), (usize, String)>::new();
	for item in &state.semantic_index.resource_references {
		let relative_path = normalize_relative_path(&item.path);
		let Some(file_node_id) = ctx
			.contributor_ids
			.get(&(item.mod_id.clone(), relative_path.clone()))
			.cloned()
		else {
			continue;
		};
		let profile = eu4_profile();
		let Some(descriptor) = profile.classify_content_family(Path::new(&relative_path)) else {
			continue;
		};
		if descriptor.id != ctx.family_id {
			continue;
		}
		if definition_reference_family(&item.key) == Some(ctx.family_id) {
			continue;
		}
		let source =
			resource_reference_source_node(item, &file_node_id, ctx.resource_definition_lines);
		push_resource_reference_evidence(nodes, &source, item);
		match classify_reference_target(ctx.family_id, &item.key) {
			ReferenceTargetClass::SameFamily => {
				let target = ctx
					.best_definition_by_identity
					.get(&(item.key.clone(), item.value.clone()))
					.cloned()
					.unwrap_or_else(|| ctx.family_node_id.to_string());
				let entry = aggregate_edges
					.entry((
						SemanticGraphEdgeKind::ReferencesIntraFamily,
						source.clone(),
						target,
					))
					.or_insert((0, format!("{} = {}", item.key, item.value)));
				entry.0 += 1;
			}
			ReferenceTargetClass::OtherFamily(target_family) => {
				let target =
					ensure_external_target_node(nodes, target_family, ExternalTargetKind::Family);
				let entry = aggregate_edges
					.entry((
						SemanticGraphEdgeKind::ReferencesCrossFamily,
						source.clone(),
						target,
					))
					.or_insert((0, format!("{} = {}", item.key, item.value)));
				entry.0 += 1;
			}
			ReferenceTargetClass::Localisation => {
				let target = ensure_external_target_node(
					nodes,
					"localisation",
					ExternalTargetKind::Localisation,
				);
				let entry = aggregate_edges
					.entry((
						SemanticGraphEdgeKind::ReferencesExternal,
						source.clone(),
						target,
					))
					.or_insert((0, format!("{} = {}", item.key, item.value)));
				entry.0 += 1;
			}
			ReferenceTargetClass::AssetUi => {
				let target =
					ensure_external_target_node(nodes, "asset_ui", ExternalTargetKind::AssetUi);
				let entry = aggregate_edges
					.entry((
						SemanticGraphEdgeKind::ReferencesExternal,
						source.clone(),
						target,
					))
					.or_insert((0, format!("{} = {}", item.key, item.value)));
				entry.0 += 1;
			}
			ReferenceTargetClass::Unknown => {
				let target =
					ensure_external_target_node(nodes, "unknown", ExternalTargetKind::Unknown);
				let entry = aggregate_edges
					.entry((
						SemanticGraphEdgeKind::ReferencesExternal,
						source.clone(),
						target,
					))
					.or_insert((0, format!("{} = {}", item.key, item.value)));
				entry.0 += 1;
			}
		}
	}

	for ((kind, from, to), (count, sample)) in aggregate_edges {
		insert_edge(
			edges,
			SemanticGraphEdge {
				kind,
				from,
				to,
				label: Some(sample.clone()),
				count: Some(count),
				sample: Some(sample),
			},
		);
	}
}

fn resource_reference_source_node(
	item: &ResourceReference,
	file_node_id: &str,
	resource_definition_lines: &HashMap<(String, String), Vec<&DefinitionSeed>>,
) -> String {
	let relative_path = normalize_relative_path(&item.path);
	resource_definition_lines
		.get(&(item.mod_id.clone(), relative_path))
		.and_then(|items| {
			items
				.iter()
				.rev()
				.find(|seed| seed.line <= item.line)
				.map(|seed| seed.node_id.clone())
		})
		.unwrap_or_else(|| file_node_id.to_string())
}

fn ensure_external_target_node(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	label: &str,
	target_kind: ExternalTargetKind,
) -> String {
	let node_id = format!("external_target:{target_kind:?}:{label}");
	nodes
		.entry(node_id.clone())
		.or_insert_with(|| SemanticGraphNode {
			id: node_id.clone(),
			kind: SemanticGraphNodeKind::ExternalTarget,
			label: label.to_string(),
			mod_id: None,
			path: None,
			line: None,
			column: None,
			precedence: None,
			is_base_game: None,
			parse_ok_hint: None,
			scope_id: None,
			scope_kind: None,
			definition_key: None,
			definition_value: None,
			target_kind: Some(target_kind),
			referenced: true,
			resource_reference_count: 0,
			scalar_assignment_count: 0,
			symbol_reference_count: 0,
			key_usage_count: 0,
			alias_usage_count: 0,
			default_visible: false,
			evidence: SemanticNodeEvidence::default(),
		});
	node_id
}

fn mark_referenced_nodes(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	edges: &BTreeMap<(u8, String, String, String), SemanticGraphEdge>,
) {
	for edge in edges.values() {
		if matches!(
			edge.kind,
			SemanticGraphEdgeKind::Contains | SemanticGraphEdgeKind::Overrides
		) {
			continue;
		}
		if let Some(node) = nodes.get_mut(&edge.from) {
			node.referenced = true;
		}
		if let Some(node) = nodes.get_mut(&edge.to) {
			node.referenced = true;
		}
	}
	for node in nodes.values_mut() {
		if node.kind == SemanticGraphNodeKind::SemanticBlock
			&& (node.resource_reference_count > 0
				|| node.scalar_assignment_count > 0
				|| node.symbol_reference_count > 0
				|| node.key_usage_count > 0
				|| node.alias_usage_count > 0)
		{
			node.referenced = true;
		}
	}
}

fn classify_reference_target(current_family_id: &str, key: &str) -> ReferenceTargetClass {
	if let Some(target_family) = definition_reference_family(key) {
		if target_family == current_family_id {
			return ReferenceTargetClass::SameFamily;
		}
		return ReferenceTargetClass::OtherFamily(target_family);
	}
	if is_localisation_reference_key(key) {
		return ReferenceTargetClass::Localisation;
	}
	if is_asset_reference_key(key) {
		return ReferenceTargetClass::AssetUi;
	}
	if let Some(target_family) = gameplay_target_family(key) {
		if target_family == current_family_id {
			return ReferenceTargetClass::SameFamily;
		}
		return ReferenceTargetClass::OtherFamily(target_family);
	}
	ReferenceTargetClass::Unknown
}

fn definition_reference_family(key: &str) -> Option<&'static str> {
	match key {
		"fervor_definition" => Some("common/fervor"),
		"decree_definition" => Some("common/decrees"),
		"federation_advancement_definition" => Some("common/federation_advancements"),
		"golden_bull_definition" => Some("common/golden_bulls"),
		"flagship_modification_definition" => Some("common/flagship_modifications"),
		"holy_order_definition" => Some("common/holy_orders"),
		"naval_doctrine_definition" => Some("common/naval_doctrines"),
		"defender_of_faith_definition" => Some("common/defender_of_faith"),
		"isolationism_definition" => Some("common/isolationism"),
		"professionalism_definition" => Some("common/professionalism"),
		"powerprojection_definition" => Some("common/powerprojection"),
		"subject_type_upgrade_definition" => Some("common/subject_type_upgrades"),
		"government_rank_definition" => Some("common/government_ranks"),
		"province_name_table" => Some("common/province_names"),
		"tile_definition" => Some("map/random/tiles"),
		"random_name_table" => Some("map/random_names"),
		"random_map_scenario" => Some("map/random/scenarios"),
		"advisor_definition" => Some("history/advisors"),
		"technology_definition" => Some("common/technologies"),
		_ => None,
	}
}

fn gameplay_target_family(key: &str) -> Option<&'static str> {
	match key {
		"region" => Some("map/region"),
		"area" => Some("map/area"),
		"superregion" => Some("map/superregion"),
		"continent" => Some("map/continent"),
		"provincegroup" => Some("map/provincegroup"),
		"religion" => Some("common/religions"),
		"culture" => Some("common/cultures"),
		"technology_group" => Some("common/technology"),
		"government_reform" => Some("common/government_reforms"),
		"estate" => Some("common/estates"),
		"province_id" | "location" => Some("history/provinces"),
		"first" | "second" => Some("common/country_tags"),
		_ => None,
	}
}

fn is_localisation_reference_key(key: &str) -> bool {
	matches!(
		key,
		"localisation" | "custom_tooltip" | "scenario_name_key" | "province_name_literal"
	)
}

fn is_asset_reference_key(key: &str) -> bool {
	matches!(
		key,
		"icon"
			| "gfx" | "button_gfx"
			| "marker_sprite"
			| "unit_sprite_start"
			| "tile_color_group"
			| "tile_color_rgb"
	)
}

fn push_resource_reference_evidence(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	node_id: &str,
	item: &ResourceReference,
) {
	if let Some(node) = nodes.get_mut(node_id) {
		node.resource_reference_count += 1;
		node.evidence
			.resource_references
			.push(SemanticEvidenceItem {
				label: item.key.clone(),
				value: Some(item.value.clone()),
				line: item.line,
				column: item.column,
			});
	}
}

fn push_scalar_evidence(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	node_id: &str,
	item: &ScalarAssignment,
) {
	if let Some(node) = nodes.get_mut(node_id) {
		node.scalar_assignment_count += 1;
		node.evidence.scalar_assignments.push(SemanticEvidenceItem {
			label: item.key.clone(),
			value: Some(item.value.clone()),
			line: item.line,
			column: item.column,
		});
	}
}

fn push_symbol_reference_evidence(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	node_id: &str,
	item: &SymbolReference,
) {
	if let Some(node) = nodes.get_mut(node_id) {
		node.symbol_reference_count += 1;
		node.evidence.symbol_references.push(SemanticEvidenceItem {
			label: symbol_kind_text(item.kind).to_string(),
			value: Some(item.name.clone()),
			line: item.line,
			column: item.column,
		});
	}
}

fn push_key_usage_evidence(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	node_id: &str,
	item: &KeyUsage,
) {
	if let Some(node) = nodes.get_mut(node_id) {
		node.key_usage_count += 1;
		node.evidence.key_usages.push(SemanticEvidenceItem {
			label: item.key.clone(),
			value: None,
			line: item.line,
			column: item.column,
		});
	}
}

fn push_alias_usage_evidence(
	nodes: &mut BTreeMap<String, SemanticGraphNode>,
	node_id: &str,
	item: &AliasUsage,
) {
	if let Some(node) = nodes.get_mut(node_id) {
		node.alias_usage_count += 1;
		node.evidence.alias_usages.push(SemanticEvidenceItem {
			label: item.alias.clone(),
			value: None,
			line: item.line,
			column: item.column,
		});
	}
}

fn insert_edge(
	edges: &mut BTreeMap<(u8, String, String, String), SemanticGraphEdge>,
	edge: SemanticGraphEdge,
) {
	let edge_order = match edge.kind {
		SemanticGraphEdgeKind::Contains => 0,
		SemanticGraphEdgeKind::Overrides => 1,
		SemanticGraphEdgeKind::ReferencesIntraFamily => 2,
		SemanticGraphEdgeKind::ReferencesCrossFamily => 3,
		SemanticGraphEdgeKind::ReferencesExternal => 4,
	};
	let key = (
		edge_order,
		edge.from.clone(),
		edge.to.clone(),
		edge.label.clone().unwrap_or_default(),
	);
	edges.insert(key, edge);
}

fn contributor_file_node_id(mod_id: &str, relative_path: &str) -> String {
	format!("file:{mod_id}:{relative_path}")
}

fn scope_kind_text(kind: ScopeKind) -> &'static str {
	match kind {
		ScopeKind::File => "file",
		ScopeKind::Event => "event",
		ScopeKind::Decision => "decision",
		ScopeKind::ScriptedEffect => "scripted_effect",
		ScopeKind::Trigger => "trigger",
		ScopeKind::Effect => "effect",
		ScopeKind::Loop => "loop",
		ScopeKind::AliasBlock => "alias_block",
		ScopeKind::Block => "block",
	}
}

fn symbol_kind_text(kind: foch_core::model::SymbolKind) -> &'static str {
	match kind {
		foch_core::model::SymbolKind::Event => "event",
		foch_core::model::SymbolKind::Decision => "decision",
		foch_core::model::SymbolKind::ScriptedEffect => "scripted_effect",
		foch_core::model::SymbolKind::ScriptedTrigger => "scripted_trigger",
		foch_core::model::SymbolKind::DiplomaticAction => "diplomatic_action",
		foch_core::model::SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}

fn is_false(value: &bool) -> bool {
	!*value
}

fn render_semantic_graph_html(
	artifact: &SemanticGraphArtifact,
) -> Result<String, Box<dyn std::error::Error>> {
	let embedded_json = serde_json::to_string(artifact)?.replace("</script", "<\\/script");
	let template = r#"<!doctype html>
<html lang="en">
<head>
	<meta charset="utf-8">
	<meta name="viewport" content="width=device-width, initial-scale=1">
	<title>foch semantic graph - __FAMILY__</title>
	<style>
		:root {{
			color-scheme: light dark;
			font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
			background: #0f1115;
			color: #e6e9ef;
		}}
		body {{
			margin: 0;
			display: grid;
			grid-template-columns: 280px 1fr 360px;
			min-height: 100vh;
			background: #0f1115;
		}}
		aside, main {{
			padding: 16px;
			overflow: auto;
			border-right: 1px solid #232733;
		}}
		#details {{
			border-right: 0;
			background: #121724;
		}}
		h1, h2, h3 {{
			margin: 0 0 12px 0;
			font-size: 16px;
		}}
		.controls label {{
			display: flex;
			gap: 8px;
			align-items: center;
			margin: 8px 0;
			font-size: 13px;
		}}
		.tree {{
			font-size: 13px;
			line-height: 1.45;
		}}
		.tree ul {{
			list-style: none;
			margin: 0;
			padding-left: 20px;
			border-left: 1px solid #2b3344;
		}}
		.tree li {{
			margin: 6px 0;
		}}
		.node {{
			display: block;
			width: 100%;
			text-align: left;
			border: 1px solid #2d3547;
			background: #171d28;
			color: inherit;
			border-radius: 8px;
			padding: 8px 10px;
			cursor: pointer;
		}}
		.node.active {{
			border-color: #7aa2f7;
			box-shadow: 0 0 0 1px #7aa2f7 inset;
		}}
		.badges {{
			display: flex;
			flex-wrap: wrap;
			gap: 6px;
			margin-top: 6px;
		}}
		.badge {{
			display: inline-block;
			padding: 1px 6px;
			border-radius: 999px;
			font-size: 11px;
			background: #243047;
			color: #b9c7e3;
		}}
		.edge-list {{
			margin-top: 6px;
			display: flex;
			flex-direction: column;
			gap: 4px;
		}}
		.edge-item {{
			font-size: 12px;
			color: #a8b3cf;
		}}
		.section {{
			margin-top: 18px;
		}}
		.section ul {{
			padding-left: 18px;
		}}
		code {{
			font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		}}
	</style>
</head>
<body>
	<aside>
		<h1>Semantic Graph</h1>
		<p><code>__FAMILY__</code></p>
		<div class="controls" id="controls"></div>
	</aside>
	<main>
		<div id="tree" class="tree"></div>
	</main>
	<aside id="details">
		<h2>Details</h2>
		<div id="details-body">Select a node.</div>
	</aside>
	<script id="semantic-graph-data" type="application/json">__JSON__</script>
	<script>
	const graph = JSON.parse(document.getElementById("semantic-graph-data").textContent);
	const defaults = graph.defaults;
	const state = {{
		showContains: defaults.show_contains,
		showOverrides: defaults.show_overrides,
		showIntraFamilyRefs: defaults.show_intra_family_refs,
		showCrossFamilyRefs: defaults.show_cross_family_refs,
		showExternalRefs: defaults.show_external_refs,
		showNestedBlocks: defaults.show_nested_blocks,
		showEvidenceLeaves: defaults.show_evidence_leaves,
		showUnreferencedBlocks: defaults.show_unreferenced_blocks,
		showUnreferencedDefinitions: defaults.show_unreferenced_definitions,
		selectedNodeId: null,
	}};
	const controls = [
		["showContains", "Show contains"],
		["showOverrides", "Show overrides"],
		["showIntraFamilyRefs", "Show intra-family refs"],
		["showCrossFamilyRefs", "Show cross-family refs"],
		["showExternalRefs", "Show external refs"],
		["showNestedBlocks", "Show nested blocks"],
		["showEvidenceLeaves", "Show evidence leaves"],
		["showUnreferencedBlocks", "Show unreferenced blocks"],
		["showUnreferencedDefinitions", "Show unreferenced definitions"],
	];
	const nodesById = new Map(graph.nodes.map((node) => [node.id, node]));
	const containsChildren = new Map();
	const outgoingById = new Map();
	const incomingById = new Map();
	for (const edge of graph.edges) {{
		if (edge.kind === "contains") {{
			if (!containsChildren.has(edge.from)) containsChildren.set(edge.from, []);
			containsChildren.get(edge.from).push(edge.to);
		}} else {{
			if (!outgoingById.has(edge.from)) outgoingById.set(edge.from, []);
			outgoingById.get(edge.from).push(edge);
			if (!incomingById.has(edge.to)) incomingById.set(edge.to, []);
			incomingById.get(edge.to).push(edge);
		}}
	}}
	function visibleNode(node) {{
		if (node.kind === "semantic_block") {{
			return state.showNestedBlocks && (state.showUnreferencedBlocks || node.referenced);
		}}
		if (node.kind === "definition") {{
			return state.showUnreferencedDefinitions || node.referenced;
		}}
		return node.kind !== "external_target";
	}}
	function allowedRefKind(kind) {{
		if (kind === "references_intra_family") return state.showIntraFamilyRefs;
		if (kind === "references_cross_family") return state.showCrossFamilyRefs;
		if (kind === "references_external") return state.showExternalRefs;
		if (kind === "overrides") return state.showOverrides;
		return false;
	}}
	function renderControls() {{
		const el = document.getElementById("controls");
		el.innerHTML = controls.map(([key, label]) => `
			<label><input type="checkbox" data-key="${{key}}" ${{state[key] ? "checked" : ""}}> ${{label}}</label>
		`).join("");
		for (const input of el.querySelectorAll("input")) {{
			input.addEventListener("change", (event) => {{
				state[event.target.dataset.key] = event.target.checked;
				render();
			}});
		}}
	}}
	function renderBadges(node) {{
		const badges = [];
		badges.push(`<span class="badge">${{node.kind}}</span>`);
		if (node.mod_id) badges.push(`<span class="badge">${{node.mod_id}}</span>`);
		if (node.precedence !== null && node.precedence !== undefined) badges.push(`<span class="badge">precedence ${{node.precedence}}</span>`);
		if (node.scope_kind) badges.push(`<span class="badge">${{node.scope_kind}}</span>`);
		if (node.referenced) badges.push(`<span class="badge">referenced</span>`);
		if (node.resource_reference_count) badges.push(`<span class="badge">${{node.resource_reference_count}} refs</span>`);
		if (node.scalar_assignment_count) badges.push(`<span class="badge">${{node.scalar_assignment_count}} scalars</span>`);
		if (node.symbol_reference_count) badges.push(`<span class="badge">${{node.symbol_reference_count}} symbol refs</span>`);
		if (node.key_usage_count) badges.push(`<span class="badge">${{node.key_usage_count}} keys</span>`);
		return badges.length ? `<div class="badges">${{badges.join("")}}</div>` : "";
	}}
	function renderEdgeList(nodeId) {{
		const edges = (outgoingById.get(nodeId) || []).filter((edge) => allowedRefKind(edge.kind));
		if (!edges.length) return "";
		return `<div class="edge-list">${{edges.map((edge) => {{
			const target = nodesById.get(edge.to);
			const label = edge.label ? ` <code>${{edge.label}}</code>` : "";
			const count = edge.count ? ` ×${{edge.count}}` : "";
			return `<div class="edge-item">${{edge.kind}} → <button data-node="${{edge.to}}" class="link-button">${{target ? target.label : edge.to}}</button>${{count}}${{label}}</div>`;
		}}).join("")}}</div>`;
	}}
	function renderEvidencePreview(node) {{
		if (!state.showEvidenceLeaves) return "";
		const samples = [];
		for (const item of (node.evidence?.resource_references || []).slice(0, 3)) {{
			samples.push(`<span class="badge">${{item.label}}=${{item.value || ""}}</span>`);
		}}
		for (const item of (node.evidence?.scalar_assignments || []).slice(0, 3)) {{
			samples.push(`<span class="badge">${{item.label}}=${{item.value || ""}}</span>`);
		}}
		return samples.length ? `<div class="badges">${{samples.join("")}}</div>` : "";
	}}
	function renderTreeNode(nodeId) {{
		const node = nodesById.get(nodeId);
		if (!node || !visibleNode(node)) return "";
		const children = (containsChildren.get(nodeId) || []).map(renderTreeNode).filter(Boolean).join("");
		return `
			<li>
				<button class="node ${{state.selectedNodeId === nodeId ? "active" : ""}}" data-node="${{nodeId}}">
					<div><strong>${{node.label}}</strong></div>
					${{renderBadges(node)}}
					${{state.showOverrides ? renderEdgeList(nodeId).replaceAll("references_", "").replaceAll("overrides", "overrides") : renderEdgeList(nodeId)}}
					${{renderEvidencePreview(node)}}
				</button>
				${{children ? `<ul>${{children}}</ul>` : ""}}
			</li>
		`;
	}}
	function renderTree() {{
		const tree = document.getElementById("tree");
		const roots = graph.nodes.filter((node) => node.kind === "family");
		tree.innerHTML = `<ul>${{roots.map((node) => renderTreeNode(node.id)).join("")}}</ul>`;
		for (const button of tree.querySelectorAll("button[data-node]")) {{
			button.addEventListener("click", (event) => {{
				const id = event.currentTarget.dataset.node;
				state.selectedNodeId = id;
				renderDetails(id);
				renderTree();
			}});
		}}
	}
	function renderEvidenceSection(title, items) {{
		if (!items || !items.length) return "";
		return `<div class="section"><h3>${{title}}</h3><ul>${{items.map((item) => `<li><code>${{item.label}}</code>${{item.value ? ` = <code>${{item.value}}</code>` : ""}} <span>(${{item.line}}:${{item.column}})</span></li>`).join("")}}</ul></div>`;
	}}
	function renderDetails(nodeId) {{
		const node = nodesById.get(nodeId);
		const details = document.getElementById("details-body");
		if (!node) {{
			details.textContent = "Select a node.";
			return;
		}}
		const outgoing = (outgoingById.get(nodeId) || []).filter((edge) => allowedRefKind(edge.kind));
		const incoming = (incomingById.get(nodeId) || []).filter((edge) => allowedRefKind(edge.kind));
		details.innerHTML = `
			<div class="section"><strong>${{node.label}}</strong></div>
			<div class="section"><code>${{node.kind}}</code></div>
			${{node.mod_id ? `<div class="section">mod: <code>${{node.mod_id}}</code></div>` : ""}}
			${{node.path ? `<div class="section">path: <code>${{node.path}}</code></div>` : ""}}
			${{node.definition_key ? `<div class="section">definition: <code>${{node.definition_key}}</code> = <code>${{node.definition_value}}</code></div>` : ""}}
			${{node.scope_id !== null && node.scope_id !== undefined ? `<div class="section">scope: <code>${{node.scope_id}}</code></div>` : ""}}
			${{renderEvidenceSection("Resource references", node.evidence?.resource_references)}}
			${{renderEvidenceSection("Scalar assignments", node.evidence?.scalar_assignments)}}
			${{renderEvidenceSection("Symbol references", node.evidence?.symbol_references)}}
			${{renderEvidenceSection("Key usages", node.evidence?.key_usages)}}
			${{renderEvidenceSection("Alias usages", node.evidence?.alias_usages)}}
			${{outgoing.length ? `<div class="section"><h3>Outgoing edges</h3><ul>${{outgoing.map((edge) => {{
				const target = nodesById.get(edge.to);
				return `<li><code>${{edge.kind}}</code> → ${{target ? target.label : edge.to}}</li>`;
			}}).join("")}}</ul></div>` : ""}}
			${{incoming.length ? `<div class="section"><h3>Incoming edges</h3><ul>${{incoming.map((edge) => {{
				const source = nodesById.get(edge.from);
				return `<li><code>${{edge.kind}}</code> ← ${{source ? source.label : edge.from}}</li>`;
			}}).join("")}}</ul></div>` : ""}}
		`;
		for (const button of details.querySelectorAll("button[data-node]")) {{
			button.addEventListener("click", (event) => {{
				const id = event.currentTarget.dataset.node;
				state.selectedNodeId = id;
				render();
			}});
		}}
	}
	function render() {{
		renderControls();
		renderTree();
		if (state.selectedNodeId) {{
			renderDetails(state.selectedNodeId);
		}}
	}}
	render();
	</script>
</body>
</html>
	"#;
	let html = template
		.replace("{{", "{")
		.replace("}}", "}")
		.replace("__FAMILY__", &artifact.family_id)
		.replace("__JSON__", &embedded_json);
	Ok(html)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::graph::model::{GraphArtifactFormat, GraphModeSelection, GraphScopeSelection};
	use crate::request::CheckRequest;
	use crate::workspace::ResolvedFileContributor;
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::Playlist;
	use foch_core::model::{
		DocumentFamily, DocumentRecord, KeyUsage, ScalarAssignment, ScopeNode, ScopeType,
		SemanticIndex, SourceSpan,
	};
	use std::collections::{BTreeMap, HashMap};
	use std::path::PathBuf;

	#[test]
	fn classifier_keeps_same_family_definitions_separate_from_cross_family_refs() {
		assert_eq!(
			classify_reference_target("common/holy_orders", "holy_order_definition"),
			ReferenceTargetClass::SameFamily
		);
		assert_eq!(
			classify_reference_target("common/holy_orders", "region"),
			ReferenceTargetClass::OtherFamily("map/region")
		);
		assert_eq!(
			classify_reference_target("common/holy_orders", "custom_tooltip"),
			ReferenceTargetClass::Localisation
		);
		assert_eq!(
			classify_reference_target("common/holy_orders", "icon"),
			ReferenceTargetClass::AssetUi
		);
	}

	#[test]
	fn semantic_mode_requires_family_before_workspace_resolution() {
		let request = CheckRequest {
			playset_path: PathBuf::from("/definitely/missing.playset"),
			config: crate::config::Config::default(),
		};
		let result = run_semantic_graph_with_options(
			request,
			Path::new("/tmp/ignored"),
			GraphBuildOptions {
				include_game_base: false,
				mode: GraphModeSelection::Semantic,
				scope: GraphScopeSelection::Workspace,
				format: GraphArtifactFormat::Both,
				root: None,
				family: None,
			},
		);
		assert!(result.is_err());
		assert_eq!(
			result.expect_err("missing family").to_string(),
			"semantic graph mode requires --family"
		);
	}

	#[test]
	fn builder_keeps_definition_trunk_but_prunes_unreferenced_block_details() {
		let workspace = test_workspace();
		let state = test_runtime_state();
		let artifact = build_semantic_graph_artifact(&workspace, &state, "common/holy_orders")
			.expect("artifact");
		let family = artifact
			.nodes
			.iter()
			.find(|node| node.kind == SemanticGraphNodeKind::Family)
			.expect("family node");
		assert_eq!(family.label, "common/holy_orders");
		let definition = artifact
			.nodes
			.iter()
			.find(|node| {
				node.kind == SemanticGraphNodeKind::Definition
					&& node.definition_key.as_deref() == Some("holy_order_definition")
					&& node.definition_value.as_deref() == Some("order_alpha")
			})
			.expect("definition");
		assert!(definition.default_visible);
		let block = artifact
			.nodes
			.iter()
			.find(|node| node.kind == SemanticGraphNodeKind::SemanticBlock)
			.expect("block");
		assert!(!block.default_visible);
		assert!(artifact.edges.iter().any(|edge| {
			edge.kind == SemanticGraphEdgeKind::ReferencesCrossFamily
				&& edge.label.as_deref() == Some("region = europe_region")
		}));
		assert!(artifact.edges.iter().any(|edge| {
			edge.kind == SemanticGraphEdgeKind::ReferencesExternal
				&& edge.label.as_deref() == Some("custom_tooltip = HOLY_ORDER_TOOLTIP")
		}));
	}

	#[test]
	fn html_renderer_emits_valid_braces_for_css_and_js_templates() {
		let workspace = test_workspace();
		let state = test_runtime_state();
		let artifact = build_semantic_graph_artifact(&workspace, &state, "common/holy_orders")
			.expect("artifact");
		let html = render_semantic_graph_html(&artifact).expect("html");
		assert!(html.contains("const state = {"));
		assert!(html.contains(":root {"));
		assert!(html.contains("data-key=\"${key}\""));
		assert!(!html.contains("const state = {{"));
		assert!(!html.contains("${{"));
	}

	fn test_workspace() -> ResolvedWorkspace {
		let mut file_inventory = BTreeMap::new();
		file_inventory.insert(
			"common/holy_orders/orders.txt".to_string(),
			vec![
				ResolvedFileContributor {
					mod_id: "base:eu4".to_string(),
					root_path: PathBuf::from("/base"),
					absolute_path: PathBuf::from("/base/common/holy_orders/orders.txt"),
					precedence: 0,
					is_base_game: true,
					parse_ok_hint: Some(true),
				},
				ResolvedFileContributor {
					mod_id: "mod:test".to_string(),
					root_path: PathBuf::from("/mod"),
					absolute_path: PathBuf::from("/mod/common/holy_orders/orders.txt"),
					precedence: 1,
					is_base_game: false,
					parse_ok_hint: Some(true),
				},
			],
		);
		ResolvedWorkspace {
			playlist_path: PathBuf::from("/tmp/test.playset"),
			playlist: Playlist {
				game: Game::EuropaUniversalis4,
				name: "test".to_string(),
				mods: Vec::new(),
			},
			mods: Vec::new(),
			installed_base_snapshot: None,
			mod_snapshots: Vec::new(),
			file_inventory,
		}
	}

	fn test_runtime_state() -> RuntimeState {
		let scopes = vec![
			ScopeNode {
				id: 0,
				kind: ScopeKind::File,
				parent: None,
				this_type: ScopeType::Country,
				aliases: HashMap::new(),
				mod_id: "mod:test".to_string(),
				path: PathBuf::from("common/holy_orders/orders.txt"),
				span: SourceSpan { line: 1, column: 1 },
			},
			ScopeNode {
				id: 1,
				kind: ScopeKind::Block,
				parent: Some(0),
				this_type: ScopeType::Country,
				aliases: HashMap::new(),
				mod_id: "mod:test".to_string(),
				path: PathBuf::from("common/holy_orders/orders.txt"),
				span: SourceSpan { line: 2, column: 1 },
			},
		];
		RuntimeState {
			semantic_index: SemanticIndex {
				documents: vec![DocumentRecord {
					mod_id: "mod:test".to_string(),
					path: PathBuf::from("common/holy_orders/orders.txt"),
					family: DocumentFamily::Clausewitz,
					parse_ok: true,
				}],
				scopes,
				definitions: vec![],
				references: vec![],
				alias_usages: vec![],
				key_usages: vec![KeyUsage {
					key: "modifier".to_string(),
					mod_id: "mod:test".to_string(),
					path: PathBuf::from("common/holy_orders/orders.txt"),
					line: 3,
					column: 2,
					scope_id: 1,
					this_type: ScopeType::Country,
				}],
				scalar_assignments: vec![ScalarAssignment {
					key: "cost".to_string(),
					value: "50".to_string(),
					mod_id: "mod:test".to_string(),
					path: PathBuf::from("common/holy_orders/orders.txt"),
					line: 4,
					column: 2,
					scope_id: 1,
				}],
				localisation_definitions: vec![],
				localisation_duplicates: vec![],
				ui_definitions: vec![],
				resource_references: vec![
					ResourceReference {
						key: "holy_order_definition".to_string(),
						value: "order_alpha".to_string(),
						mod_id: "base:eu4".to_string(),
						path: PathBuf::from("common/holy_orders/orders.txt"),
						line: 1,
						column: 1,
					},
					ResourceReference {
						key: "holy_order_definition".to_string(),
						value: "order_alpha".to_string(),
						mod_id: "mod:test".to_string(),
						path: PathBuf::from("common/holy_orders/orders.txt"),
						line: 1,
						column: 1,
					},
					ResourceReference {
						key: "region".to_string(),
						value: "europe_region".to_string(),
						mod_id: "mod:test".to_string(),
						path: PathBuf::from("common/holy_orders/orders.txt"),
						line: 5,
						column: 2,
					},
					ResourceReference {
						key: "custom_tooltip".to_string(),
						value: "HOLY_ORDER_TOOLTIP".to_string(),
						mod_id: "mod:test".to_string(),
						path: PathBuf::from("common/holy_orders/orders.txt"),
						line: 6,
						column: 2,
					},
				],
				csv_rows: vec![],
				json_properties: vec![],
				parse_issues: vec![],
			},
			definitions: vec![],
			overlap_status_by_def: HashMap::new(),
			winner_by_symbol: HashMap::new(),
			dependency_hints: HashMap::new(),
			scope_definition_map: HashMap::new(),
			enabled_mod_ids: BTreeSet::from(["mod:test".to_string()])
				.into_iter()
				.collect(),
			base_game_mod_id: Some("base:eu4".to_string()),
		}
	}
}
