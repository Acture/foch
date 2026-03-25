use serde::Serialize;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct SimplifyOptions {
	pub include_game_base: bool,
	pub target_mod_id: String,
	pub out_dir: Option<PathBuf>,
	pub in_place: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SimplifySummary {
	pub report_path: PathBuf,
	pub removed_definition_count: usize,
	pub removed_file_count: usize,
	pub target_root: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SimplifyReport {
	pub target_mod_id: String,
	pub removed: Vec<SimplifyRemovedItem>,
	pub kept: Vec<SimplifyKeptItem>,
	pub merge_candidates: Vec<SimplifyKeptItem>,
	pub conflicts: Vec<SimplifyKeptItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimplifyRemovedItem {
	pub symbol_kind: String,
	pub name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimplifyKeptItem {
	pub symbol_kind: String,
	pub name: String,
	pub path: String,
	pub line: usize,
	pub column: usize,
	pub reason: String,
}
