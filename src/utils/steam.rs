use std::path::{Path, PathBuf};

/// 尝试自动侦测 Steam 根目录。
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
	path.exists().then_some(path)
}

#[cfg(target_os = "macos")]
fn find_steam_root_impl() -> Option<PathBuf> {
	let home = home::home_dir()?;
	let candidates = steam_root_candidates(
		&home,
		std::env::var_os("XDG_DATA_HOME").as_ref().map(Path::new),
	);
	candidates.into_iter().find(|path| path.exists())
}

#[cfg(target_os = "linux")]
fn find_steam_root_impl() -> Option<PathBuf> {
	let home = home::home_dir()?;
	let candidates = steam_root_candidates(
		&home,
		std::env::var_os("XDG_DATA_HOME").as_ref().map(Path::new),
	);
	candidates.into_iter().find(|path| path.exists())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn find_steam_root_impl() -> Option<PathBuf> {
	None
}

pub fn steam_root_candidates(home: &Path, xdg_data_home: Option<&Path>) -> Vec<PathBuf> {
	let xdg_root = xdg_data_home
		.map(PathBuf::from)
		.unwrap_or_else(|| home.join(".local").join("share"));

	vec![
		xdg_root.join("Steam"),
		home.join(".steam").join("steam"),
		home.join("Library")
			.join("Application Support")
			.join("Steam"),
	]
}

#[cfg(test)]
mod tests {
	use super::steam_root_candidates;
	use std::path::Path;

	#[test]
	fn candidate_generation_is_stable() {
		let home = Path::new("/tmp/user");
		let xdg = Path::new("/tmp/xdg");
		let candidates = steam_root_candidates(home, Some(xdg));
		assert_eq!(candidates[0], xdg.join("Steam"));
		assert_eq!(candidates[1], home.join(".steam").join("steam"));
	}
}
