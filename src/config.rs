use crate::steam::find_steam_root_path;
use clap_verbosity_flag::{InfoLevel, Verbosity};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// 1. 定义我们的配置结构
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Config {
	pub steam_root_path: Option<PathBuf>,
}

fn get_config_file_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
	Ok(dirs::config_dir().ok_or("无法获取配置目录")?.join("foch"))
}

fn load_config(path: &Path) -> Result<Config, std::io::Error> {
	let content = std::fs::read_to_string(path)?;
	toml::from_str(&content).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

fn save_config(path: &Path, config: &Config) -> Result<(), std::io::Error> {
	let content = toml::to_string_pretty(config)
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
	std::fs::write(path, content)
}

pub fn load_or_init_config() -> Result<Config, Box<dyn std::error::Error>> {
	let config_dir = get_config_file_path()?;

	if !config_dir.exists() {
		std::fs::create_dir_all(&config_dir)?;
		tracing::info!("创建配置目录: {:?}", &config_dir);
	}

	let config_file = config_dir.join("config.toml");

	if !config_file.exists() {
		std::fs::File::create(&config_file)?;
		tracing::info!("创建配置文件: {:?}", &config_file);
	}

	// --- 步骤 A: 尝试加载 ---
	if let Ok(config) = load_config(&config_file) {
		if config.steam_root_path.is_some() {
			// 缓存命中！我们啥也不用干，直接返回
			return Ok(config);
		}
	}

	// --- 步骤 B: 缓存未命中 (或文件不存在) ---
	// 这是“第一次启动”的逻辑

	// 1. 设置“漂亮”的加载条
	let spinner = ProgressBar::new_spinner();
	spinner.enable_steady_tick(Duration::from_millis(120));
	spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}") // ".cyan" 设置颜色
			.unwrap()
			// 这里可以自定义你喜欢的旋转动画
			.tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
	);
	spinner.set_message("首次运行，正在自动侦测 Steam 路径...");

	let steam_path = find_steam_root_path();

	spinner.finish_and_clear();

	let mut config = Config::default();

	if let Some(path) = steam_path {
		// 4. 侦测成功
		println!("✅ 侦测成功！Steam 根目录: {:?}", path);
		println!("   配置已缓存至: {:?}", config_file);
		config.steam_root_path = Some(path);
	} else {
		// 5. 侦测失败
		eprintln!("⚠️ 自动侦测 Steam 路径失败。");
		eprintln!("   您仍可继续使用，但某些功能(如读取工坊Mod)可能受限。");
		eprintln!("   请尝试使用 --steam-path <路径> 手动配置。");
		// config.steam_root_path 保持为 None
	}

	// 6. 无论成功与否，都将结果(哪怕是 None)写入缓存
	// 这样下次就不会再“首次运行”了
	save_config(&config_file, &config)?;

	Ok(config)
}
