use crate::cli::arg;
use crate::cli::arg::SetConfigArgs;
use crate::cli::handler::HandlerResult;
use foch_engine::Config;
use std::path::Path;

pub fn handle_set(
	set_args: &SetConfigArgs,
	config: &mut Config,
	config_file: &Path,
) -> HandlerResult {
	tracing::info!("设置配置: {:?}", set_args);

	match &set_args.command {
		arg::FochCliSetCommands::SteamPath(path_args) => {
			let path = path_args
				.path
				.canonicalize()
				.unwrap_or_else(|_| path_args.path.clone());
			println!("设置 Steam 路径: {}", path.display());
			config.steam_root_path = Some(path);
		}
		arg::FochCliSetCommands::ParadoxDataPath(path_args) => {
			let path = path_args
				.path
				.canonicalize()
				.unwrap_or_else(|_| path_args.path.clone());
			println!("设置 Paradox 数据路径: {}", path.display());
			config.paradox_data_path = Some(path);
		}
		arg::FochCliSetCommands::GamePath(game_path_args) => {
			let path = game_path_args
				.path
				.canonicalize()
				.unwrap_or_else(|_| game_path_args.path.clone());
			println!(
				"设置游戏 '{}' 路径: {}",
				game_path_args.game_name,
				path.display()
			);
			config
				.game_path
				.insert(game_path_args.game_name.clone(), path);
		}
	}

	config.save_config(config_file)?;
	Ok(0)
}
