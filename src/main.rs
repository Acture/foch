use clap::Parser;
use foch::{cli::arg, config::{self, load_or_init_config}};
use tracing_subscriber::FmtSubscriber;
fn main() {
	let cliargs = arg::ModManagerCli::parse();

	let subscriber = FmtSubscriber::builder()
		.with_max_level(cliargs.verbose.tracing_level_filter()) // 1. 从 -v flag 获取级别
		.with_target(false) // 2. (可选) uv 风格，不显示模块路径
		.without_time() // 3. (可选) uv 风格，不显示时间戳
		.finish();

	tracing::subscriber::set_global_default(subscriber).expect("设置 tracing 失败");

	tracing::info!(
		"foch 已启动，日志级别: {}",
		cliargs.verbose.tracing_level_filter()
	);
	tracing::debug!("这是一个 DEBUG 消息，只有 -vv 才能看到");
	tracing::info!("当前命令行参数: {:?}", cliargs);

	let (mut config, config_file) = load_or_init_config().expect("无法加载或初始化配置");

	tracing::info!("当前配置: {:?}", config);

	match &cliargs.command {
		arg::ModManagerCliCommands::Check(cliargs) => {
			tracing::info!("检查 Playset: {:?}", cliargs.playset_path);
		}
		arg::ModManagerCliCommands::Config(config_args) => match &config_args.command {
			arg::ModManagerCliConfigCommands::Set(set_args) => {
				tracing::info!("设置配置: {:?}", set_args);
				match &set_args.command {
					arg::ModManagerCliSetCommands::SteamPath(path_args) => {
						config.steam_root_path = Some(path_args.path.clone());
						println!("设置 Steam 路径为: {:?}", path_args.path);
					}
					arg::ModManagerCliSetCommands::ParadoxDataPath(path_args) => {
						config.paradox_data_path = Some(path_args.path.clone());
						println!("设置 Paradox 数据路径为: {:?}", path_args.path);
					}
					arg::ModManagerCliSetCommands::GamePath(game_path_args) => {
						config.game_path.insert(
							game_path_args.game_name.clone(),
							game_path_args.path.clone(),
						);
						println!(
							"设置游戏 '{}' 的路径为: {:?}",
							game_path_args.game_name, game_path_args.path
						);
					}

				}
				config.save_config(&config_file).expect("保存配置失败");
			}
			arg::ModManagerCliConfigCommands::Show => {
				tracing::info!("显示当前配置");
				println!("当前配置: {:?}", config);
			}
		},
	}
}
