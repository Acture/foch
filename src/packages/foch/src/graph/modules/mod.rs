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
pub use report::{build_module_report, merge_trace_edges_from_trace};

use crate::check::runtime::build_runtime_state_for_request;
use crate::input::InputRequest;
use crate::model::SemanticIndex;

pub fn run_module_report(index: &SemanticIndex, max_iters: usize) -> ModuleReport {
	let graph = project_symbol_graph(index);
	let partition = cluster_modules(&graph, max_iters);
	build_module_report(&graph, &partition)
}

pub fn run_module_report_for_request(
	request: &InputRequest,
	include_game_base: bool,
	max_iters: usize,
) -> Result<ModuleReport, String> {
	let state = build_runtime_state_for_request(request, include_game_base)?;
	Ok(run_module_report(&state.semantic_index, max_iters))
}

pub use report::write_module_report;
