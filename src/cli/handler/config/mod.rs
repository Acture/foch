use crate::cli::arg;
use crate::cli::arg::ConfigArgs;
use std::path::Path;
use crate::cli::config::Config;
use crate::cli::handler::config::set::handle_set;
use crate::cli::handler::config::show::handle_show;

pub mod set;
pub mod show;

pub fn handle_config(config_args: &ConfigArgs, config: &mut Config, config_file: &Path) {
	match &config_args.command {
		arg::ModManagerCliConfigCommands::Set(set_args) => handle_set(set_args, config, config_file),
		arg::ModManagerCliConfigCommands::Show => handle_show(config),
	}
}
