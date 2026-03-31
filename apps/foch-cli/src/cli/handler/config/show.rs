use crate::cli::arg::ShowConfigArgs;
use crate::cli::handler::HandlerResult;
use foch_engine::Config;

pub fn handle_show(config: &Config, show_args: &ShowConfigArgs) -> HandlerResult {
	if show_args.json {
		println!("{}", serde_json::to_string_pretty(config)?);
	} else {
		println!("当前配置:");
		println!(
			"  steam_root_path: {}",
			display_opt_path(config.steam_root_path.as_deref())
		);
		println!(
			"  paradox_data_path: {}",
			display_opt_path(config.paradox_data_path.as_deref())
		);
		if config.game_path.is_empty() {
			println!("  game_path: <empty>");
		} else {
			println!("  game_path:");
			for (game, path) in &config.game_path {
				println!("    {game}: {}", path.display());
			}
		}
	}

	Ok(0)
}

fn display_opt_path(path: Option<&std::path::Path>) -> String {
	path.map(|p| p.display().to_string())
		.unwrap_or_else(|| "<unset>".to_string())
}
