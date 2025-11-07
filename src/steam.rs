use std::path::PathBuf;

/// 尝试自动侦测 Steam 的根安装目录
pub fn find_steam_root_path() -> Option<PathBuf> {
	find_steam_root_impl()
}

#[cfg(windows)]
fn find_steam_root_impl() -> Option<PathBuf> {
	use winreg::RegKey;
	use winreg::enums::HKEY_CURRENT_USER;

	let hkey_current_user = RegKey::predef(HKEY_CURRENT_USER);

	let steam_key = hkey_current_user
		.open_subkey("SOFTWARE\\Valve\\Steam")
		.ok()?;

	let steam_path_str: String = steam_key.get_value("SteamPath").ok()?;

	let path = PathBuf::from(steam_path_str);

	// 再次确认一下，万一注册表骗人
	if path.exists() { Some(path) } else { None }
}

#[cfg(target_os = "macos")]
fn find_steam_root_impl() -> Option<PathBuf> {
	// macOS 简单粗暴，路径是固定的
	let path = home::home_dir()?.join("Library/Application Support/Steam");

	if path.exists() { Some(path) } else { None }
}

#[cfg(target_os = "linux")]
fn find_steam_root_impl() -> Option<PathBuf> {
	let home = home::home_dir()?;

	// 路径 1: 现代 XDG/Flatpak 标准
	// 先尝试从 $XDG_DATA_HOME 环境变量找, 找不到就默认 ~/.local/share
	let xdg_data_dir = std::env::var("XDG_DATA_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|_| home.join(".local/share"));

	let xdg_path = xdg_data_dir.join("Steam");
	if xdg_path.exists() {
		return Some(xdg_path);
	}

	// 路径 2: 经典的 ~/.steam/steam 路径
	let legacy_path = home.join(".steam/steam");
	if legacy_path.exists() {
		return Some(legacy_path);
	}

	// 都没找到，那就算了
	None
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn find_steam_root_impl() -> Option<PathBuf> {
	None
}


mod tests {
	use super::*;

	#[test]
	fn test_find_steam_root() {
		let path = find_steam_root_path();
		println!("{:?}", path);
		assert!(path.is_some(), "无法找到 Steam 根目录");
	}

}
