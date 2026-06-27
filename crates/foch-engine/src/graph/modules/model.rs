use foch_core::model::MergeTraceEdge;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A logical symbol identity, stable across mods: `kind:name`. Two mods that
/// define/override the same logical symbol map to the SAME node, which is what
/// makes the partition meaningful for merge (a collision is a shared node).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct SymbolNodeId(pub String);

impl SymbolNodeId {
	pub fn new(kind: &str, name: &str) -> Self {
		SymbolNodeId(format!("{kind}:{name}"))
	}
}

#[derive(Clone, Debug, Default)]
pub struct SymbolGraph {
	pub nodes: BTreeSet<SymbolNodeId>,
	/// Directed reference edges (caller -> callee) with multiplicity weight.
	pub edges: BTreeMap<(SymbolNodeId, SymbolNodeId), u32>,
	/// Seed label per node (the content family of its defining file).
	pub seeds: BTreeMap<SymbolNodeId, String>,
	/// Which mod ids define or override each node (base game uses "__base__").
	pub node_mods: BTreeMap<SymbolNodeId, BTreeSet<String>>,
}

impl SymbolGraph {
	pub fn add_node(&mut self, id: &SymbolNodeId) {
		self.nodes.insert(id.clone());
	}

	pub fn add_edge(&mut self, from: &SymbolNodeId, to: &SymbolNodeId, weight: u32) {
		if from == to {
			return;
		}
		self.add_node(from);
		self.add_node(to);
		*self.edges.entry((from.clone(), to.clone())).or_insert(0) += weight;
	}

	pub fn set_seed(&mut self, id: &SymbolNodeId, label: &str) {
		self.add_node(id);
		self.seeds.insert(id.clone(), label.to_string());
	}

	pub fn add_mod(&mut self, id: &SymbolNodeId, mod_id: &str) {
		self.add_node(id);
		self.node_mods
			.entry(id.clone())
			.or_default()
			.insert(mod_id.to_string());
	}

	/// Collapse directed edges into a symmetric weighted adjacency map.
	pub fn undirected_adjacency(&self) -> BTreeMap<SymbolNodeId, BTreeMap<SymbolNodeId, u64>> {
		let mut adj: BTreeMap<SymbolNodeId, BTreeMap<SymbolNodeId, u64>> = BTreeMap::new();
		for ((from, to), w) in &self.edges {
			*adj.entry(from.clone())
				.or_default()
				.entry(to.clone())
				.or_insert(0) += *w as u64;
			*adj.entry(to.clone())
				.or_default()
				.entry(from.clone())
				.or_insert(0) += *w as u64;
		}
		adj
	}
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ModulePartition {
	/// Module label assigned to each node (deterministic).
	pub module_of: BTreeMap<SymbolNodeId, String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ModSummary {
	pub mod_id: String,
	pub touched_nodes: usize,
	pub nodes_per_module: BTreeMap<String, usize>,
	/// share of touched nodes in this mod's single largest module (0.0..=1.0).
	pub top_module_share: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct CollisionHotspot {
	pub module: String,
	pub collision_nodes: usize,
	pub mods_involved: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ModuleReport {
	pub module_count: usize,
	pub node_count: usize,
	pub module_sizes: BTreeMap<String, usize>,
	pub mods: Vec<ModSummary>,
	pub collision_hotspots: Vec<CollisionHotspot>,
	#[serde(default)]
	pub merge_trace_edges: Vec<MergeTraceEdge>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn symbol_graph_add_edge_is_undirected_weight_sum() {
		let mut g = SymbolGraph::default();
		let a = SymbolNodeId::new("scripted_effect", "add_happiness");
		let b = SymbolNodeId::new("scripted_effect", "give_money");
		g.add_edge(&a, &b, 2);
		g.add_edge(&b, &a, 1);
		let adj = g.undirected_adjacency();
		assert_eq!(adj.get(&a).and_then(|m| m.get(&b)).copied(), Some(3));
		assert_eq!(adj.get(&b).and_then(|m| m.get(&a)).copied(), Some(3));
		assert!(g.nodes.contains(&a) && g.nodes.contains(&b));
	}

	#[test]
	fn add_edge_ignores_self_loops() {
		let mut g = SymbolGraph::default();
		let a = SymbolNodeId::new("k", "a");
		g.add_edge(&a, &a, 5);
		assert!(g.edges.is_empty());
	}
}
