use std::path::Path;
use crate::cli::arg::SetConfigArgs;
use crate::cli::config::Config;

pub fn handle_show(config: &mut Config) {
	tracing::info!("显示当前配置");
	println!("当前配置: {:?}", config);
}