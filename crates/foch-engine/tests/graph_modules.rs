use foch_engine::graph::modules::{SymbolGraph, SymbolNodeId};

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
