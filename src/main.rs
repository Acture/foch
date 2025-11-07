use clap::Parser;
use foch::{cli::{arg}, config::load_or_init_config};
use tracing_subscriber::FmtSubscriber;
fn main() {
	let cli = arg::ModManagerCli::parse();

	let subscriber = FmtSubscriber::builder()
		.with_max_level(cli.verbose.tracing_level_filter()) // 1. 从 -v flag 获取级别
		.with_target(false) // 2. (可选) uv 风格，不显示模块路径
		.without_time() // 3. (可选) uv 风格，不显示时间戳
		.finish();

	tracing::subscriber::set_global_default(subscriber).expect("设置 tracing 失败");

	tracing::info!("foch 已启动，日志级别: {}", cli.verbose.tracing_level_filter());
	tracing::debug!("这是一个 DEBUG 消息，只有 -vv 才能看到");
	tracing::warn!("这是一个 WARN 消息，默认就能看到");

	tracing::info!("当前命令行参数: {:?}", cli);

	let config = load_or_init_config().expect("无法加载或初始化配置");

	tracing::info!("当前配置: {:?}", config);



	match &cli.command {
		arg::ModManagerCliCommands::Check => {}
	}
}
