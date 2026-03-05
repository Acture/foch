use regex::Regex;
use std::collections::HashSet;
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

pub fn steam_library_paths(steam_root: &Path) -> Vec<PathBuf> {
	let mut candidates = Vec::new();
	candidates.push(steam_root.to_path_buf());

	let mut seen = HashSet::new();
	let mut resolved = Vec::new();
	for candidate in libraryfolders_files(steam_root) {
		let Ok(content) = std::fs::read_to_string(&candidate) else {
			continue;
		};
		for path in extract_library_paths_from_vdf(&content) {
			candidates.push(path);
		}
	}

	for path in candidates {
		let normalized = normalize_candidate(path);
		if !seen.insert(normalized.clone()) {
			continue;
		}
		resolved.push(PathBuf::from(normalized));
	}

	resolved
}

pub fn steam_workshop_mod_path(steam_root: &Path, app_id: u32, steam_id: &str) -> Option<PathBuf> {
	for library in steam_library_paths(steam_root) {
		let candidate = library
			.join("steamapps")
			.join("workshop")
			.join("content")
			.join(app_id.to_string())
			.join(steam_id);
		if candidate.is_dir() {
			return Some(candidate);
		}
	}
	None
}

pub fn steam_game_install_path(steam_root: &Path, app_id: u32) -> Option<PathBuf> {
	for library in steam_library_paths(steam_root) {
		let manifest = library
			.join("steamapps")
			.join(format!("appmanifest_{app_id}.acf"));
		if !manifest.is_file() {
			continue;
		}
		let Ok(content) = std::fs::read_to_string(&manifest) else {
			continue;
		};
		let Some(installdir) = extract_installdir_from_appmanifest(&content) else {
			continue;
		};
		let candidate = library.join("steamapps").join("common").join(installdir);
		if candidate.is_dir() {
			return Some(candidate);
		}
	}
	None
}

fn libraryfolders_files(steam_root: &Path) -> Vec<PathBuf> {
	vec![
		steam_root.join("steamapps").join("libraryfolders.vdf"),
		steam_root.join("libraryfolders.vdf"),
		steam_root.join("libraryfolder.vdf"),
	]
}

fn normalize_candidate(path: PathBuf) -> String {
	path.to_string_lossy().replace('\\', "/")
}

pub fn extract_library_paths_from_vdf(content: &str) -> Vec<PathBuf> {
	let path_re = Regex::new(r#""path"\s*"([^"]+)""#).expect("valid steam library path regex");
	let mut paths = Vec::new();
	for capture in path_re.captures_iter(content) {
		let Some(raw) = capture.get(1) else {
			continue;
		};
		let unescaped = raw.as_str().replace("\\\\", "\\");
		paths.push(PathBuf::from(unescaped));
	}
	paths
}

fn extract_installdir_from_appmanifest(content: &str) -> Option<String> {
	let install_re =
		Regex::new(r#""installdir"\s*"([^"]+)""#).expect("valid appmanifest installdir regex");
	let capture = install_re.captures(content)?;
	let raw = capture.get(1)?.as_str();
	Some(raw.replace("\\\\", "\\"))
}

#[cfg(test)]
mod tests {
	use super::{
		extract_library_paths_from_vdf, steam_game_install_path, steam_library_paths,
		steam_root_candidates,
	};
	use std::path::Path;
	use tempfile::TempDir;

	#[test]
	fn candidate_generation_is_stable() {
		let home = Path::new("/tmp/user");
		let xdg = Path::new("/tmp/xdg");
		let candidates = steam_root_candidates(home, Some(xdg));
		assert_eq!(candidates[0], xdg.join("Steam"));
		assert_eq!(candidates[1], home.join(".steam").join("steam"));
	}

	#[test]
	fn extract_library_paths_handles_windows_and_unix_styles() {
		let vdf = r#"
"libraryfolders"
{
	"0" { "path" "D:\\SteamLibrary" }
	"1" { "path" "/mnt/ssd/steam" }
}
"#;
		let paths = extract_library_paths_from_vdf(vdf);
		assert_eq!(paths.len(), 2);
		assert_eq!(paths[0], std::path::PathBuf::from(r"D:\SteamLibrary"));
		assert_eq!(paths[1], std::path::PathBuf::from("/mnt/ssd/steam"));
	}

	#[test]
	fn steam_library_paths_reads_libraryfolders() {
		let tmp = TempDir::new().expect("temp dir");
		let steam_root = tmp.path().join("Steam");
		std::fs::create_dir_all(steam_root.join("steamapps")).expect("create steamapps");
		std::fs::write(
			steam_root.join("steamapps").join("libraryfolders.vdf"),
			format!(
				r#""libraryfolders"
{{
	"0"
	{{
		"path"		"{}"
	}}
	"1"
	{{
		"path"		"{}"
	}}
}}"#,
				steam_root.display(),
				tmp.path().join("SteamLibrary2").display()
			),
		)
		.expect("write vdf");

		let paths = steam_library_paths(&steam_root);
		assert!(paths.iter().any(|item| item == &steam_root));
		assert!(
			paths
				.iter()
				.any(|item| item == &tmp.path().join("SteamLibrary2"))
		);
	}

	#[test]
	fn steam_game_install_path_uses_appmanifest() {
		let tmp = TempDir::new().expect("temp dir");
		let steam_root = tmp.path().join("Steam");
		let lib2 = tmp.path().join("SteamLibrary2");
		std::fs::create_dir_all(steam_root.join("steamapps")).expect("create steamapps");
		std::fs::create_dir_all(lib2.join("steamapps").join("common")).expect("create common");
		std::fs::write(
			steam_root.join("steamapps").join("libraryfolders.vdf"),
			format!(
				r#""libraryfolders"
{{
	"0" {{ "path" "{}" }}
	"1" {{ "path" "{}" }}
}}"#,
				steam_root.display(),
				lib2.display()
			),
		)
		.expect("write vdf");
		std::fs::write(
			lib2.join("steamapps").join("appmanifest_236850.acf"),
			r#""AppState"
{
	"appid" "236850"
	"installdir" "Europa Universalis IV"
}"#,
		)
		.expect("write manifest");
		let game_dir = lib2
			.join("steamapps")
			.join("common")
			.join("Europa Universalis IV");
		std::fs::create_dir_all(&game_dir).expect("create game dir");

		let resolved = steam_game_install_path(&steam_root, 236850);
		assert_eq!(resolved.as_deref(), Some(game_dir.as_path()));
	}
}
