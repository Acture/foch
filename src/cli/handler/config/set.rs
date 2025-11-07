use crate::cli::arg;
use crate::cli::arg::{ConfigArgs, SetConfigArgs};
use crate::cli::config::Config;
use std::path::Path;

pub fn handle_set(set_args: &SetConfigArgs, config: &mut Config, config_file: &Path) {
	tracing::info!("设置配置: {:?}", set_args);
	match &set_args.command {
		arg::ModManagerCliSetCommands::SteamPath(path_args) => {
			let t_path = path_args
				.path
				.canonicalize()
				.unwrap_or(path_args.path.clone());
			println!("设置 Steam 路径为: {:?}", t_path);
			config.steam_root_path = Some(t_path);
		}
		arg::ModManagerCliSetCommands::ParadoxDataPath(path_args) => {
			let t_path = path_args
				.path
				.canonicalize()
				.unwrap_or(path_args.path.clone());
			println!("设置 Paradox 数据路径为: {:?}", t_path);
			config.paradox_data_path = Some(t_path);
		}
		arg::ModManagerCliSetCommands::GamePath(game_path_args) => {
			let t_path = game_path_args
				.path
				.canonicalize()
				.unwrap_or(game_path_args.path.clone());
			println!(
				"设置游戏 '{}' 的路径为: {:?}",
				game_path_args.game_name, t_path
			);
			config
				.game_path
				.insert(game_path_args.game_name.clone(), t_path);
		}
	}
	config.save_config(config_file).expect("保存配置失败");
}
