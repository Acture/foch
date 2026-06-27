use super::model::{SymbolGraph, SymbolNodeId};
use foch_core::model::{SemanticIndex, SymbolDefinition, SymbolKind};
use std::collections::BTreeMap;
use std::path::Path;

fn node_id(kind: SymbolKind, name: &str) -> SymbolNodeId {
	SymbolNodeId::new(kind.as_str(), name)
}

/// Root family used as the clustering seed: for paths under common/, history/,
/// map/ use the first TWO segments (e.g. "common/religions"); otherwise the
/// first segment (e.g. "events"). Deterministic.
fn seed_for_path(path: &Path) -> String {
	let normalized = path.to_string_lossy().replace('\\', "/");
	let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
	match parts.first() {
		Some(&head @ ("common" | "history" | "map")) => match parts.get(1) {
			Some(group) => format!("{head}/{}", strip_ext(group)),
			None => head.to_string(),
		},
		Some(first) => strip_ext(first).to_string(),
		None => "misc".to_string(),
	}
}

fn strip_ext(value: &str) -> &str {
	value.rsplit_once('.').map_or(value, |(stem, _)| stem)
}

pub fn project_symbol_graph(index: &SemanticIndex) -> SymbolGraph {
	let mut graph = SymbolGraph::default();

	let mut scope_parent: BTreeMap<usize, Option<usize>> = BTreeMap::new();
	for scope in &index.scopes {
		scope_parent.insert(scope.id, scope.parent);
	}

	let mut scope_to_defs: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
	for (i, def) in index.definitions.iter().enumerate() {
		scope_to_defs.entry(def.scope_id).or_default().push(i);
	}

	for def in &index.definitions {
		let id = node_id(def.kind, &def.name);
		graph.add_node(&id);
		graph.set_seed(&id, &seed_for_path(&def.path));
		let mod_id = if def.mod_id.is_empty() {
			"__base__"
		} else {
			def.mod_id.as_str()
		};
		graph.add_mod(&id, mod_id);
	}

	for reference in &index.references {
		let Some(enclosing_idx) = nearest_enclosing_def(
			reference.scope_id,
			&scope_parent,
			&scope_to_defs,
			&index.definitions,
		) else {
			continue;
		};
		let enclosing = &index.definitions[enclosing_idx];
		let from = node_id(enclosing.kind, &enclosing.name);
		let to = node_id(reference.kind, &reference.name);
		graph.add_edge(&from, &to, 1);
	}

	graph
}

/// Walk up the scope parent chain from `scope_id` to the nearest scope that
/// owns a definition; return that definition's index. Deterministic: among
/// defs sharing one scope, pick the smallest by name. Returns None if the
/// chain reaches the root without an owning definition.
fn nearest_enclosing_def(
	mut scope_id: usize,
	scope_parent: &BTreeMap<usize, Option<usize>>,
	scope_to_defs: &BTreeMap<usize, Vec<usize>>,
	definitions: &[SymbolDefinition],
) -> Option<usize> {
	loop {
		if let Some(defs) = scope_to_defs.get(&scope_id) {
			return defs
				.iter()
				.copied()
				.min_by(|&a, &b| definitions[a].name.cmp(&definitions[b].name));
		}
		match scope_parent.get(&scope_id).copied().flatten() {
			Some(parent) => scope_id = parent,
			None => return None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::super::model::SymbolNodeId;
	use super::*;
	use foch_core::model::{
		MaybeScope, ScopeKind, ScopeNode, ScopeSet, SemanticIndex, SourceSpan, SymbolDefinition,
		SymbolKind, SymbolReference,
	};
	use std::path::PathBuf;

	fn def(name: &str, mod_id: &str, path: &str, scope_id: usize) -> SymbolDefinition {
		SymbolDefinition {
			kind: SymbolKind::ScriptedEffect,
			name: name.to_string(),
			module: String::new(),
			local_name: name.to_string(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from(path),
			line: 1,
			column: 1,
			scope_id,
			declared_this_type: MaybeScope::Unknown,
			inferred_this_type: MaybeScope::Unknown,
			inferred_this_mask: ScopeSet::EMPTY,
			inferred_from_mask: ScopeSet::EMPTY,
			inferred_root_mask: ScopeSet::EMPTY,
			required_params: Vec::new(),
			optional_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		}
	}

	fn reference(name: &str, mod_id: &str, path: &str, scope_id: usize) -> SymbolReference {
		SymbolReference {
			kind: SymbolKind::ScriptedEffect,
			name: name.to_string(),
			module: String::new(),
			mod_id: mod_id.to_string(),
			path: PathBuf::from(path),
			line: 2,
			column: 1,
			scope_id,
			provided_params: Vec::new(),
			param_bindings: Vec::new(),
		}
	}

	fn scope(id: usize, parent: Option<usize>, path: &str) -> ScopeNode {
		ScopeNode {
			id,
			kind: ScopeKind::Block,
			parent,
			this_type: MaybeScope::Unknown,
			aliases: Default::default(),
			mod_id: String::new(),
			path: PathBuf::from(path),
			span: SourceSpan { line: 1, column: 1 },
			key: String::new(),
		}
	}

	#[test]
	fn projects_definitions_and_reference_edges() {
		let mut index = SemanticIndex::default();
		index
			.definitions
			.push(def("caller", "modA", "common/scripted_effects/f1.txt", 10));
		index
			.definitions
			.push(def("callee", "", "common/scripted_effects/f2.txt", 20));
		index.references.push(reference(
			"callee",
			"modA",
			"common/scripted_effects/f1.txt",
			10,
		));

		let g = project_symbol_graph(&index);
		let kind = SymbolKind::ScriptedEffect.as_str();
		let caller = SymbolNodeId::new(kind, "caller");
		let callee = SymbolNodeId::new(kind, "callee");
		assert!(g.nodes.contains(&caller) && g.nodes.contains(&callee));
		assert_eq!(
			g.edges.get(&(caller.clone(), callee.clone())).copied(),
			Some(1)
		);
		assert!(g.node_mods[&caller].contains("modA"));
		assert!(g.node_mods[&callee].contains("__base__"));
		assert!(g.seeds.contains_key(&caller));
	}

	#[test]
	fn attributes_nested_reference_to_enclosing_definition() {
		let path = "common/scripted_effects/f1.txt";
		let mut index = SemanticIndex::default();
		index.definitions.push(def("caller", "modA", path, 5));
		index.definitions.push(def("aaa_other", "modA", path, 3));
		index.scopes.push(scope(5, None, path));
		index.scopes.push(scope(7, Some(5), path));
		index.references.push(reference("callee", "modA", path, 7));

		let g = project_symbol_graph(&index);
		let kind = SymbolKind::ScriptedEffect.as_str();
		let caller = SymbolNodeId::new(kind, "caller");
		let other = SymbolNodeId::new(kind, "aaa_other");
		let callee = SymbolNodeId::new(kind, "callee");
		assert_eq!(g.edges.get(&(caller, callee.clone())).copied(), Some(1));
		assert!(!g.edges.contains_key(&(other, callee)));
	}

	#[test]
	fn drops_reference_with_no_enclosing_definition() {
		let path = "common/scripted_effects/f1.txt";
		let mut index = SemanticIndex::default();
		index.definitions.push(def("caller", "modA", path, 5));
		index.scopes.push(scope(1, None, path));
		index.scopes.push(scope(2, Some(1), path));
		index.references.push(reference("callee", "modA", path, 2));

		let g = project_symbol_graph(&index);
		assert!(g.edges.is_empty());
	}

	#[test]
	fn seed_for_path_value() {
		let mut index = SemanticIndex::default();
		index
			.definitions
			.push(def("religion_def", "modA", "common/religions/x.txt", 1));
		index
			.definitions
			.push(def("event_def", "modA", "events/y.txt", 2));

		let g = project_symbol_graph(&index);
		let kind = SymbolKind::ScriptedEffect.as_str();
		let religion = SymbolNodeId::new(kind, "religion_def");
		let event = SymbolNodeId::new(kind, "event_def");
		assert_eq!(
			g.seeds.get(&religion).map(String::as_str),
			Some("common/religions")
		);
		assert_eq!(g.seeds.get(&event).map(String::as_str), Some("events"));
	}

	#[test]
	fn run_module_report_end_to_end_from_index() {
		let mut index = SemanticIndex::default();
		index
			.definitions
			.push(def("a", "modA", "common/scripted_effects/f.txt", 1));
		index
			.definitions
			.push(def("b", "modA", "common/scripted_effects/f.txt", 1));
		index
			.references
			.push(reference("b", "modA", "common/scripted_effects/f.txt", 1));
		let report = super::super::run_module_report(&index, 20);
		assert!(report.node_count >= 2);
		assert!(report.module_count >= 1);
		assert!(report.mods.iter().any(|m| m.mod_id == "modA"));
	}
}
