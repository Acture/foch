use crate::cli::arg;
use crate::cli::arg::ConfigArgs;
use crate::cli::config::Config;
use crate::cli::handler::HandlerResult;
use crate::cli::handler::config::set::handle_set;
use crate::cli::handler::config::show::handle_show;
use crate::cli::handler::config::validate::handle_validate;
use std::path::Path;

pub mod set;
pub mod show;
pub mod validate;

pub fn handle_config(
	config_args: &ConfigArgs,
	config: &mut Config,
	config_file: &Path,
) -> HandlerResult {
	match &config_args.command {
		arg::FochCliConfigCommands::Set(set_args) => handle_set(set_args, config, config_file),
		arg::FochCliConfigCommands::Show(show_args) => handle_show(config, show_args),
		arg::FochCliConfigCommands::Validate => handle_validate(config),
	}
}
