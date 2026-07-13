//! Shared EU4 and Workshop discovery for corpus collection and scoring.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use foch_core::utils::steam::{
	find_steam_root_path, locate_steam_app, locate_steam_app_from_root, steam_library_paths,
};
use foch_engine::Config;

/// Europa Universalis IV Steam application id.
pub const EU4_APPID: u32 = 236850;
pub const EU4_ROOT_ENV: &str = "EU4_ROOT";
pub const STEAM_WORKSHOP_DIR_ENV: &str = "STEAM_WORKSHOP_DIR";

#[derive(Clone, Debug, Default)]
pub struct DiscoveryOverrides {
	pub game_root: Option<PathBuf>,
	pub workshop_dir: Option<PathBuf>,
	pub steam_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopCatalog {
	pub roots: Vec<PathBuf>,
}

impl WorkshopCatalog {
	pub fn resolve(&self, workshop_id: &str) -> Option<PathBuf> {
		self.roots
			.iter()
			.map(|root| root.join(workshop_id))
			.find(|path| path.is_dir())
	}

	pub fn contains(&self, workshop_id: &str) -> bool {
		self.resolve(workshop_id).is_some()
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Eu4GameDiscovery {
	pub game_root: PathBuf,
	pub game_version: String,
	pub steam_build_id: Option<u64>,
	pub steam_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Eu4Discovery {
	pub game_root: PathBuf,
	pub game_version: String,
	pub steam_build_id: Option<u64>,
	pub steam_root: Option<PathBuf>,
	pub workshop: WorkshopCatalog,
}

pub fn discover_eu4(overrides: &DiscoveryOverrides) -> Result<Eu4Discovery, String> {
	let config = load_existing_config().unwrap_or_default();
	let game_override = overrides
		.game_root
		.clone()
		.or_else(|| std::env::var_os(EU4_ROOT_ENV).map(PathBuf::from));
	let workshop_override = overrides
		.workshop_dir
		.clone()
		.or_else(|| std::env::var_os(STEAM_WORKSHOP_DIR_ENV).map(PathBuf::from));
	resolve_eu4(
		game_override,
		workshop_override,
		overrides.steam_root.clone(),
		&config,
	)
}

pub fn discover_eu4_game(overrides: &DiscoveryOverrides) -> Result<Eu4GameDiscovery, String> {
	let config = load_existing_config().unwrap_or_default();
	let game_override = overrides
		.game_root
		.clone()
		.or_else(|| std::env::var_os(EU4_ROOT_ENV).map(PathBuf::from));
	resolve_eu4_game(game_override, overrides.steam_root.clone(), &config)
}

pub fn resolve_eu4(
	game_override: Option<PathBuf>,
	workshop_override: Option<PathBuf>,
	steam_override: Option<PathBuf>,
	config: &Config,
) -> Result<Eu4Discovery, String> {
	let game = resolve_eu4_game(game_override, steam_override, config)?;
	let roots = if let Some(workshop) = workshop_override {
		vec![workshop]
	} else if let Some(root) = game.steam_root.as_deref() {
		steam_library_paths(root)
			.into_iter()
			.map(|library| workshop_root(&library))
			.filter(|candidate| candidate.is_dir())
			.collect()
	} else {
		Vec::new()
	};
	let roots = dedup_paths(roots)
		.into_iter()
		.filter(|root| root.is_dir())
		.collect::<Vec<_>>();
	if roots.is_empty() {
		return Err(format!(
			"could not locate an EU4 Workshop directory; pass --workshop-dir or set {STEAM_WORKSHOP_DIR_ENV}"
		));
	}
	Ok(Eu4Discovery {
		game_root: game.game_root,
		game_version: game.game_version,
		steam_build_id: game.steam_build_id,
		steam_root: game.steam_root,
		workshop: WorkshopCatalog { roots },
	})
}

pub fn resolve_eu4_game(
	game_override: Option<PathBuf>,
	steam_override: Option<PathBuf>,
	config: &Config,
) -> Result<Eu4GameDiscovery, String> {
	let steam_root = steam_override
		.or_else(|| config.steam_root_path.clone())
		.or_else(find_steam_root_path);
	let located = match steam_root.as_deref() {
		Some(root) => locate_steam_app_from_root(root, EU4_APPID).ok(),
		None => locate_steam_app(EU4_APPID).ok(),
	};

	let game_root = game_override
		.or_else(|| config.game_path.get("eu4").cloned())
		.or_else(|| located.as_ref().map(|app| app.game_root.clone()))
		.ok_or_else(|| {
			format!(
				"could not locate EU4; pass --game-root, set {EU4_ROOT_ENV}, or configure game_path.eu4"
			)
		})?;
	if !game_root.is_dir() {
		return Err(format!("EU4 root does not exist: {}", game_root.display()));
	}
	let game_version = detect_game_version(&game_root)
		.ok_or_else(|| format!("could not detect EU4 version under {}", game_root.display()))?;

	let steam_build_id = located.as_ref().and_then(|app| {
		paths_equal(&app.game_root, &game_root)
			.then_some(app.build_id)
			.flatten()
	});
	Ok(Eu4GameDiscovery {
		game_root,
		game_version,
		steam_build_id,
		steam_root,
	})
}

pub fn detect_game_version(game_root: &Path) -> Option<String> {
	for candidate in [
		game_root.join("launcher-settings.json"),
		game_root.join("launcher").join("launcher-settings.json"),
		game_root.join("version.txt"),
	] {
		if !candidate.is_file() {
			continue;
		}
		if candidate.file_name() == Some(OsStr::new("version.txt")) {
			let version = fs::read_to_string(&candidate).ok()?;
			let version = version.lines().next()?.trim();
			if !version.is_empty() {
				return Some(version.to_string());
			}
			continue;
		}
		let raw = fs::read_to_string(&candidate).ok()?;
		let json = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
		for key in ["rawVersion", "version", "gameVersion"] {
			if let Some(version) = json.get(key).and_then(serde_json::Value::as_str)
				&& !version.trim().is_empty()
			{
				return Some(version.trim().to_string());
			}
		}
	}
	None
}

/// Short git SHA of the current checkout, recorded into `corpus.json` for
/// provenance. `None` if git is unavailable or this isn't a repo.
pub fn tool_commit() -> Option<String> {
	let out = Command::new("git")
		.args(["rev-parse", "--short", "HEAD"])
		.output()
		.ok()?;
	if !out.status.success() {
		return None;
	}
	let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
	if sha.is_empty() { None } else { Some(sha) }
}

fn workshop_root(library: &Path) -> PathBuf {
	library
		.join("steamapps")
		.join("workshop")
		.join("content")
		.join(EU4_APPID.to_string())
}

fn load_existing_config() -> Result<Config, String> {
	let config_dir = foch_engine::get_config_dir_path().map_err(|err| err.to_string())?;
	let path = config_dir.join("config.toml");
	if !path.is_file() {
		return Ok(Config::default());
	}
	Config::load_config(&path)
		.map_err(|err| format!("failed to load foch config {}: {err}", path.display()))
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut seen = HashSet::new();
	paths
		.into_iter()
		.filter(|path| seen.insert(path.to_string_lossy().replace('\\', "/")))
		.collect()
}

fn paths_equal(left: &Path, right: &Path) -> bool {
	match (left.canonicalize(), right.canonicalize()) {
		(Ok(left), Ok(right)) => left == right,
		_ => left == right,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn vdf_path_value(path: &Path) -> String {
		path.to_string_lossy().replace('\\', "\\\\")
	}

	fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
		let temp = tempfile::tempdir().unwrap();
		let steam = temp.path().join("Steam");
		let library = temp.path().join("Library");
		let game = library.join("steamapps/common/Europa Universalis IV");
		let workshop = library.join("steamapps/workshop/content/236850");
		fs::create_dir_all(steam.join("steamapps")).unwrap();
		fs::create_dir_all(&game).unwrap();
		fs::create_dir_all(&workshop).unwrap();
		fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
		fs::write(
			steam.join("steamapps/libraryfolders.vdf"),
			format!(
				r#""libraryfolders"
{{
	"0" {{ "path" "{}" }}
	"1" {{ "path" "{}" }}
}}"#,
				vdf_path_value(&steam),
				vdf_path_value(&library)
			),
		)
		.unwrap();
		fs::write(
			library.join("steamapps/appmanifest_236850.acf"),
			r#""AppState"
{
	"appid" "236850"
	"installdir" "Europa Universalis IV"
	"buildid" "4242"
}"#,
		)
		.unwrap();
		(temp, steam, game, workshop)
	}

	#[test]
	fn steam_discovery_finds_game_build_and_secondary_workshop() {
		let (_temp, steam, game, workshop) = fixture();
		let resolved = resolve_eu4(None, None, Some(steam.clone()), &Config::default()).unwrap();
		assert_eq!(resolved.game_root, game);
		assert_eq!(resolved.game_version, "1.37.5");
		assert_eq!(resolved.steam_build_id, Some(4242));
		assert_eq!(resolved.workshop.roots, vec![workshop]);
		assert_eq!(resolved.steam_root, Some(steam));
	}

	#[test]
	fn explicit_paths_take_precedence_over_config() {
		let (temp, _steam, _game, _workshop) = fixture();
		let explicit_game = temp.path().join("explicit-game");
		let explicit_workshop = temp.path().join("explicit-workshop");
		fs::create_dir_all(&explicit_game).unwrap();
		fs::create_dir_all(&explicit_workshop).unwrap();
		fs::write(explicit_game.join("version.txt"), "9.9.9\n").unwrap();
		let mut config = Config::default();
		config
			.game_path
			.insert("eu4".to_string(), temp.path().join("wrong-game"));
		let resolved = resolve_eu4(
			Some(explicit_game.clone()),
			Some(explicit_workshop.clone()),
			None,
			&config,
		)
		.unwrap();
		assert_eq!(resolved.game_root, explicit_game);
		assert_eq!(resolved.workshop.roots, vec![explicit_workshop]);
		assert_eq!(resolved.game_version, "9.9.9");
	}

	#[test]
	fn workshop_catalog_resolves_across_roots() {
		let temp = tempfile::tempdir().unwrap();
		let first = temp.path().join("first");
		let second = temp.path().join("second");
		fs::create_dir_all(second.join("42")).unwrap();
		let catalog = WorkshopCatalog {
			roots: vec![first, second.clone()],
		};
		assert_eq!(catalog.resolve("42"), Some(second.join("42")));
	}
}
