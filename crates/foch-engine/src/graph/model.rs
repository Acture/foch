use foch_core::model::SymbolKind;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphModeSelection {
	Calls,
	Semantic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphScopeSelection {
	Workspace,
	Base,
	Mods,
	All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphArtifactFormat {
	Json,
	Dot,
	Both,
}

#[derive(Clone, Debug)]
pub struct GraphBuildOptions {
	pub include_game_base: bool,
	pub mode: GraphModeSelection,
	pub scope: GraphScopeSelection,
	pub format: GraphArtifactFormat,
	pub root: Option<GraphRootSelector>,
	pub family: Option<String>,
}

impl Default for GraphBuildOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
			mode: GraphModeSelection::Calls,
			scope: GraphScopeSelection::All,
			format: GraphArtifactFormat::Both,
			root: None,
			family: None,
		}
	}
}

#[derive(Clone, Debug)]
pub struct GraphRootSelector {
	pub kind: SymbolKind,
	pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct GraphBuildSummary {
	pub out_dir: PathBuf,
	pub workspace_written: bool,
	pub base_written: bool,
	pub mod_count: usize,
	pub tree_written: bool,
	pub semantic_written: bool,
}
