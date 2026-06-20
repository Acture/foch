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

	let mut defs_by_path: BTreeMap<String, Vec<&SymbolDefinition>> = BTreeMap::new();
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
		defs_by_path
			.entry(def.path.to_string_lossy().to_string())
			.or_default()
			.push(def);
	}

	for reference in &index.references {
		let path_key = reference.path.to_string_lossy().to_string();
		let Some(candidates) = defs_by_path.get(&path_key) else {
			continue;
		};
		let enclosing = candidates
			.iter()
			.find(|d| d.scope_id == reference.scope_id)
			.or_else(|| candidates.iter().min_by(|a, b| a.name.cmp(&b.name)));
		let Some(enclosing) = enclosing else {
			continue;
		};
		let from = node_id(enclosing.kind, &enclosing.name);
		let to = node_id(reference.kind, &reference.name);
		graph.add_edge(&from, &to, 1);
	}

	graph
}

#[cfg(test)]
mod tests {
	use super::super::model::SymbolNodeId;
	use super::*;
	use foch_core::model::{
		ScopeType, SemanticIndex, SymbolDefinition, SymbolKind, SymbolReference,
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
}
