use foch_core::model::{AnalysisMode, ChannelMode};
use std::path::PathBuf;

use crate::config::Config;

#[derive(Clone, Debug)]
pub struct CheckRequest {
	pub playset_path: PathBuf,
	pub config: Config,
}

#[derive(Clone, Debug)]
pub struct RunOptions {
	pub analysis_mode: AnalysisMode,
	pub channel_mode: ChannelMode,
	pub include_game_base: bool,
}

impl Default for RunOptions {
	fn default() -> Self {
		Self {
			analysis_mode: AnalysisMode::default(),
			channel_mode: ChannelMode::default(),
			include_game_base: true,
		}
	}
}

#[derive(Clone, Debug)]
pub struct MergePlanOptions {
	pub include_game_base: bool,
}

impl Default for MergePlanOptions {
	fn default() -> Self {
		Self {
			include_game_base: true,
		}
	}
}
