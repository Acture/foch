use clap::Parser;
use foch::{cli, cli::arg, cli::config::load_or_init_config};
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
		arg::ModManagerCliCommands::Check(check_args) => {
			cli::handler::check::handle_check(check_args, config);
		}
		arg::ModManagerCliCommands::Config(config_args) => {
			cli::handler::config::handle_config(config_args, &mut config, &config_file)
		}
	}
}
