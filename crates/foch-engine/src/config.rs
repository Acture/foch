use foch_core::utils::steam::find_steam_root_path;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CONFIG_DIR_ENV: &str = "FOCH_CONFIG_DIR";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Config {
	#[serde(default)]
	pub steam_root_path: Option<PathBuf>,
	#[serde(default)]
	pub paradox_data_path: Option<PathBuf>,
	#[serde(default)]
	pub game_path: HashMap<String, PathBuf>,
	/// Additional glob patterns matched (case-insensitive) against the
	/// slash-normalized relative path of every file discovered while walking
	/// mod roots and the base game install. Files matching any pattern are
	/// dropped before parsing, semantic indexing, or conflict detection.
	#[serde(default)]
	pub extra_ignore_patterns: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ValidationStatus {
	Ok,
	Warning,
	Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidationItem {
	pub key: String,
	pub status: ValidationStatus,
	pub message: String,
}

impl TryFrom<&Path> for Config {
	type Error = std::io::Error;

	fn try_from(p: &Path) -> Result<Self, Self::Error> {
		let content = std::fs::read_to_string(p)?;
		toml::from_str(&content).map_err(std::io::Error::other)
	}
}

pub fn get_config_dir_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
	if let Ok(path) = std::env::var(CONFIG_DIR_ENV) {
		return Ok(PathBuf::from(path));
	}

	let home = dirs::home_dir().ok_or("无法获取 $HOME 目录")?;
	Ok(home.join(".config").join("foch"))
}

impl Config {
	pub fn load_config(path: &Path) -> Result<Self, std::io::Error> {
		let content = std::fs::read_to_string(path)?;
		if content.trim().is_empty() {
			return Ok(Self::default());
		}

		toml::from_str(&content).map_err(std::io::Error::other)
	}

	pub fn save_config(&self, path: &Path) -> Result<(), std::io::Error> {
		let content = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
		std::fs::write(path, content)
	}

	pub fn validate(&self) -> Vec<ValidationItem> {
		let mut items = Vec::new();

		match &self.steam_root_path {
			Some(path) if path.exists() => items.push(ValidationItem {
				key: "steam_root_path".to_string(),
				status: ValidationStatus::Ok,
				message: format!("Steam 路径可用: {}", path.display()),
			}),
			Some(path) => items.push(ValidationItem {
				key: "steam_root_path".to_string(),
				status: ValidationStatus::Error,
				message: format!("Steam 路径不存在: {}", path.display()),
			}),
			None => items.push(ValidationItem {
				key: "steam_root_path".to_string(),
				status: ValidationStatus::Warning,
				message: "Steam 路径未设置，工坊 Mod 自动定位可能失败".to_string(),
			}),
		}

		match &self.paradox_data_path {
			Some(path) if path.exists() => items.push(ValidationItem {
				key: "paradox_data_path".to_string(),
				status: ValidationStatus::Ok,
				message: format!("Paradox 数据目录可用: {}", path.display()),
			}),
			Some(path) => items.push(ValidationItem {
				key: "paradox_data_path".to_string(),
				status: ValidationStatus::Error,
				message: format!("Paradox 数据目录不存在: {}", path.display()),
			}),
			None => items.push(ValidationItem {
				key: "paradox_data_path".to_string(),
				status: ValidationStatus::Warning,
				message: "Paradox 数据目录未设置，本地 Mod 解析可能失败".to_string(),
			}),
		}

		if self.game_path.is_empty() {
			items.push(ValidationItem {
				key: "game_path".to_string(),
				status: ValidationStatus::Warning,
				message: "尚未配置任何游戏安装路径".to_string(),
			});
		} else {
			for (game, path) in &self.game_path {
				let status = if path.exists() {
					ValidationStatus::Ok
				} else {
					ValidationStatus::Error
				};
				let message = if path.exists() {
					format!("游戏 {game} 路径可用: {}", path.display())
				} else {
					format!("游戏 {game} 路径不存在: {}", path.display())
				};

				items.push(ValidationItem {
					key: format!("game_path.{game}"),
					status,
					message,
				});
			}
		}

		items
	}
}

pub fn load_or_init_config() -> Result<(Config, PathBuf), Box<dyn std::error::Error>> {
	let config_dir = get_config_dir_path()?;
	if !config_dir.exists() {
		std::fs::create_dir_all(&config_dir)?;
	}

	let config_file = config_dir.join("config.toml");
	if !config_file.exists() {
		std::fs::File::create(&config_file)?;
	}

	let mut config = Config::load_config(&config_file)?;
	if config.steam_root_path.is_none()
		&& let Some(path) = find_steam_root_path()
	{
		config.steam_root_path = Some(path);
		config.save_config(&config_file)?;
	}

	Ok((config, config_file))
}

#[cfg(test)]
mod tests {
	use super::{Config, ValidationStatus};

	#[test]
	fn validate_reports_missing_paths() {
		let config = Config::default();
		let statuses: Vec<ValidationStatus> =
			config.validate().into_iter().map(|x| x.status).collect();
		assert!(statuses.contains(&ValidationStatus::Warning));
	}
}
