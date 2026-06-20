#![allow(dead_code, unused_imports)]

mod cluster;
mod model;
mod project;
mod report;

pub use cluster::cluster_modules;
pub use model::{
	CollisionHotspot, ModSummary, ModulePartition, ModuleReport, SymbolGraph, SymbolNodeId,
};
pub use project::project_symbol_graph;
pub use report::build_module_report;

use foch_core::model::SemanticIndex;

pub fn run_module_report(index: &SemanticIndex, max_iters: usize) -> ModuleReport {
	let graph = project_symbol_graph(index);
	let partition = cluster_modules(&graph, max_iters);
	build_module_report(&graph, &partition)
}

pub use report::write_module_report;
