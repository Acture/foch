use super::model::{CollisionHotspot, ModSummary, ModulePartition, ModuleReport, SymbolGraph};
use std::collections::{BTreeMap, BTreeSet};

pub fn build_module_report(graph: &SymbolGraph, partition: &ModulePartition) -> ModuleReport {
	let module_of = &partition.module_of;

	let mut module_sizes: BTreeMap<String, usize> = BTreeMap::new();
	for module in module_of.values() {
		*module_sizes.entry(module.clone()).or_insert(0) += 1;
	}

	let mut per_mod: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
	for (node, mods) in &graph.node_mods {
		let Some(module) = module_of.get(node) else {
			continue;
		};
		for mod_id in mods {
			*per_mod
				.entry(mod_id.clone())
				.or_default()
				.entry(module.clone())
				.or_insert(0) += 1;
		}
	}

	let mods = per_mod
		.into_iter()
		.map(|(mod_id, nodes_per_module)| {
			let touched_nodes: usize = nodes_per_module.values().sum();
			let top = nodes_per_module.values().copied().max().unwrap_or(0);
			let top_module_share = if touched_nodes == 0 {
				0.0
			} else {
				top as f64 / touched_nodes as f64
			};
			ModSummary {
				mod_id,
				touched_nodes,
				nodes_per_module,
				top_module_share,
			}
		})
		.collect();

	let mut collision_nodes_by_module: BTreeMap<String, usize> = BTreeMap::new();
	let mut mods_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
	for (node, mods) in &graph.node_mods {
		if mods.len() < 2 {
			continue;
		}
		let Some(module) = module_of.get(node) else {
			continue;
		};
		*collision_nodes_by_module.entry(module.clone()).or_insert(0) += 1;
		mods_by_module
			.entry(module.clone())
			.or_default()
			.extend(mods.iter().cloned());
	}

	let collision_hotspots = collision_nodes_by_module
		.into_iter()
		.map(|(module, collision_nodes)| CollisionHotspot {
			mods_involved: mods_by_module
				.get(&module)
				.map(|s| s.iter().cloned().collect())
				.unwrap_or_default(),
			module,
			collision_nodes,
		})
		.collect();

	ModuleReport {
		module_count: module_sizes.len(),
		node_count: graph.nodes.len(),
		module_sizes,
		mods,
		collision_hotspots,
	}
}

#[cfg(test)]
mod tests {
	use super::super::cluster::cluster_modules;
	use super::super::model::{SymbolGraph, SymbolNodeId};
	use super::*;

	#[test]
	fn report_localizes_mods_and_flags_collisions() {
		let mut g = SymbolGraph::default();
		let r1 = SymbolNodeId::new("k", "r1");
		let r2 = SymbolNodeId::new("k", "r2");
		let t1 = SymbolNodeId::new("k", "t1");
		for (n, fam) in [(&r1, "religion"), (&r2, "religion"), (&t1, "trade")] {
			g.set_seed(n, fam);
		}
		g.add_edge(&r1, &r2, 5);
		g.add_mod(&r1, "modA");
		g.add_mod(&r2, "modA");
		g.add_mod(&r2, "modB");
		g.add_mod(&t1, "modB");

		let part = cluster_modules(&g, 20);
		let report = build_module_report(&g, &part);

		let mod_a = report.mods.iter().find(|m| m.mod_id == "modA").unwrap();
		assert_eq!(mod_a.touched_nodes, 2);
		assert!(
			(mod_a.top_module_share - 1.0).abs() < 1e-9,
			"modA is fully localized"
		);

		let hotspot = report
			.collision_hotspots
			.iter()
			.find(|h| h.collision_nodes > 0)
			.expect("expected a collision hotspot");
		assert_eq!(hotspot.collision_nodes, 1);
		assert_eq!(
			hotspot.mods_involved,
			vec!["modA".to_string(), "modB".to_string()]
		);
	}
}
