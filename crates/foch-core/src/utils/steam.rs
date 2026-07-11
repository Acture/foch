use std::collections::HashSet;
use std::path::{Path, PathBuf};

use steamlocate::SteamDir;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatedSteamApp {
	pub app_id: u32,
	pub steam_root: PathBuf,
	pub library_root: PathBuf,
	pub game_root: PathBuf,
	pub build_id: Option<u64>,
}

pub fn find_steam_root_path() -> Option<PathBuf> {
	SteamDir::locate()
		.ok()
		.map(|steam| steam.path().to_path_buf())
}

pub fn locate_steam_app(app_id: u32) -> Result<LocatedSteamApp, String> {
	let steam = SteamDir::locate().map_err(|err| format!("failed to locate Steam: {err}"))?;
	locate_steam_app_from(&steam, app_id)
}

pub fn locate_steam_app_from_root(
	steam_root: &Path,
	app_id: u32,
) -> Result<LocatedSteamApp, String> {
	let steam = SteamDir::from_dir(steam_root)
		.map_err(|err| format!("invalid Steam root {}: {err}", steam_root.display()))?;
	locate_steam_app_from(&steam, app_id)
}

fn locate_steam_app_from(steam: &SteamDir, app_id: u32) -> Result<LocatedSteamApp, String> {
	let (app, library) = steam
		.find_app(app_id)
		.map_err(|err| format!("failed to inspect Steam app {app_id}: {err}"))?
		.ok_or_else(|| format!("Steam app {app_id} is not installed"))?;
	let game_root = library.resolve_app_dir(&app);
	if !game_root.is_dir() {
		return Err(format!(
			"Steam app {app_id} manifest points to missing directory {}",
			game_root.display()
		));
	}
	Ok(LocatedSteamApp {
		app_id,
		steam_root: steam.path().to_path_buf(),
		library_root: library.path().to_path_buf(),
		game_root,
		build_id: app.build_id,
	})
}

pub fn steam_library_paths(steam_root: &Path) -> Vec<PathBuf> {
	let mut paths = SteamDir::from_dir(steam_root)
		.and_then(|steam| steam.library_paths())
		.unwrap_or_default();
	paths.push(steam_root.to_path_buf());

	let mut seen = HashSet::new();
	paths.retain(|path| seen.insert(normalize_candidate(path)));
	paths
}

pub fn steam_workshop_mod_path(steam_root: &Path, app_id: u32, steam_id: &str) -> Option<PathBuf> {
	steam_library_paths(steam_root)
		.into_iter()
		.map(|library| {
			library
				.join("steamapps")
				.join("workshop")
				.join("content")
				.join(app_id.to_string())
				.join(steam_id)
		})
		.find(|candidate| candidate.is_dir())
}

pub fn steam_game_install_path(steam_root: &Path, app_id: u32) -> Option<PathBuf> {
	locate_steam_app_from_root(steam_root, app_id)
		.ok()
		.map(|app| app.game_root)
}

fn normalize_candidate(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::{
		locate_steam_app_from_root, steam_game_install_path, steam_library_paths,
		steam_workshop_mod_path,
	};
	use tempfile::TempDir;

	fn write_steam_fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
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
	"buildid" "123456"
}"#,
		)
		.expect("write manifest");
		let game_dir = lib2
			.join("steamapps")
			.join("common")
			.join("Europa Universalis IV");
		std::fs::create_dir_all(&game_dir).expect("create game dir");
		(tmp, steam_root, game_dir)
	}

	#[test]
	fn library_paths_include_alternate_library() {
		let (tmp, steam_root, _) = write_steam_fixture();
		let paths = steam_library_paths(&steam_root);
		assert!(paths.iter().any(|item| item == &steam_root));
		assert!(
			paths
				.iter()
				.any(|item| item == &tmp.path().join("SteamLibrary2"))
		);
	}

	#[test]
	fn located_app_carries_build_and_library_identity() {
		let (tmp, steam_root, game_dir) = write_steam_fixture();
		let app = locate_steam_app_from_root(&steam_root, 236850).expect("locate app");
		assert_eq!(app.game_root, game_dir);
		assert_eq!(app.library_root, tmp.path().join("SteamLibrary2"));
		assert_eq!(app.build_id, Some(123456));
		assert_eq!(
			steam_game_install_path(&steam_root, 236850).as_deref(),
			Some(app.game_root.as_path())
		);
	}

	#[test]
	fn workshop_item_searches_all_libraries() {
		let (tmp, steam_root, _) = write_steam_fixture();
		let workshop_item = tmp
			.path()
			.join("SteamLibrary2/steamapps/workshop/content/236850/42");
		std::fs::create_dir_all(&workshop_item).expect("create workshop item");
		assert_eq!(
			steam_workshop_mod_path(&steam_root, 236850, "42").as_deref(),
			Some(workshop_item.as_path())
		);
	}
}
