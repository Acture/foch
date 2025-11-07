use crate::cli::arg;
use crate::cli::arg::ConfigArgs;
use crate::config::Config;
use std::path::{Path, PathBuf};
use crate::cli::config::set::handle_set;
use crate::cli::config::show::handle_show;

mod set;
mod show;

pub fn handle_config(config_args: &ConfigArgs, config: &mut Config, config_file: &Path) {
	match &config_args.command {
		arg::ModManagerCliConfigCommands::Set(set_args) => handle_set(set_args, config, config_file),
		arg::ModManagerCliConfigCommands::Show => handle_show(config),
	}
}
