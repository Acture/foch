use super::model::{ModulePartition, SymbolGraph, SymbolNodeId};
use std::collections::BTreeMap;

/// Deterministic seeded label propagation.
///
/// Initial label = the node's seed if present, else its own id. Each sweep
/// (over nodes in sorted order) reassigns a node to the highest-weighted label
/// among its neighbours; ties break to the lexicographically smallest label so
/// the result is byte-reproducible. Stops at convergence or `max_iters`.
pub fn cluster_modules(graph: &SymbolGraph, max_iters: usize) -> ModulePartition {
	let adj = graph.undirected_adjacency();
	let mut label: BTreeMap<SymbolNodeId, String> = graph
		.nodes
		.iter()
		.map(|n| {
			let l = graph.seeds.get(n).cloned().unwrap_or_else(|| n.0.clone());
			(n.clone(), l)
		})
		.collect();

	for _ in 0..max_iters {
		let mut changed = false;
		for node in &graph.nodes {
			let Some(neighbours) = adj.get(node) else {
				continue;
			};
			if neighbours.is_empty() {
				continue;
			}
			let mut tally: BTreeMap<String, u64> = BTreeMap::new();
			for (nbr, w) in neighbours {
				*tally.entry(label[nbr].clone()).or_insert(0) += *w;
			}
			// max weight; tie-break = smallest label. BTreeMap iterates sorted,
			// so strict `>` keeps the first/smallest label among equal weights.
			let mut best_label: Option<&String> = None;
			let mut best_weight = 0u64;
			for (cand, w) in &tally {
				if *w > best_weight {
					best_weight = *w;
					best_label = Some(cand);
				}
			}
			if let Some(best) = best_label
				&& label[node] != *best
			{
				label.insert(node.clone(), best.clone());
				changed = true;
			}
		}
		if !changed {
			break;
		}
	}

	ModulePartition { module_of: label }
}

#[cfg(test)]
mod tests {
	use super::super::model::{SymbolGraph, SymbolNodeId};
	use super::*;

	#[test]
	fn two_dense_clusters_separate_thin_bridge() {
		let mut g = SymbolGraph::default();
		let ids = ["a", "b", "c", "d", "e", "f"].map(|n| SymbolNodeId::new("k", n));
		for (i, j, w) in [
			(0, 1, 5),
			(1, 2, 5),
			(0, 2, 5),
			(3, 4, 5),
			(4, 5, 5),
			(3, 5, 5),
			(2, 3, 1),
		] {
			g.add_edge(&ids[i], &ids[j], w);
		}
		let part = cluster_modules(&g, 20);
		let m = |n: usize| part.module_of[&ids[n]].clone();
		assert_eq!(m(0), m(1));
		assert_eq!(m(1), m(2));
		assert_eq!(m(3), m(4));
		assert_eq!(m(4), m(5));
		assert_ne!(
			m(2),
			m(3),
			"thin bridge must not merge the two dense clusters"
		);
	}

	#[test]
	fn clustering_is_deterministic() {
		let mut g = SymbolGraph::default();
		let ids = ["a", "b", "c", "d"].map(|n| SymbolNodeId::new("k", n));
		for (i, j, w) in [(0, 1, 3), (1, 2, 3), (2, 3, 3)] {
			g.add_edge(&ids[i], &ids[j], w);
		}
		assert_eq!(
			cluster_modules(&g, 20).module_of,
			cluster_modules(&g, 20).module_of
		);
	}

	#[test]
	fn seeds_bias_initial_labels() {
		let mut g = SymbolGraph::default();
		let a = SymbolNodeId::new("k", "a");
		let b = SymbolNodeId::new("k", "b");
		g.add_edge(&a, &b, 1);
		g.set_seed(&a, "religion");
		g.set_seed(&b, "religion");
		let part = cluster_modules(&g, 20);
		assert_eq!(part.module_of[&a], "religion");
		assert_eq!(part.module_of[&b], "religion");
	}
}
