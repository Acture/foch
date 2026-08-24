//! Shared EU4 and Workshop discovery for corpus collection and scoring.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use foch::playset::steam::{
	InstalledWorkshopItem, SteamId, SteamWorkshopCatalog, WorkshopInstallIdentity,
	find_steam_root_path, locate_steam_app, locate_steam_app_from_root,
};
use foch_engine::Config;

/// Europa Universalis IV Steam application id.
pub const EU4_APPID: u32 = 236850;
pub const EU4_ROOT_ENV: &str = "EU4_ROOT";
pub const STEAM_WORKSHOP_DIR_ENV: &str = "STEAM_WORKSHOP_DIR";
pub const STEAM_WORKSHOP_ACF_ENV: &str = "STEAM_WORKSHOP_ACF";

#[derive(Clone, Debug, Default)]
pub struct DiscoveryOverrides {
	pub game_root: Option<PathBuf>,
	pub workshop_dir: Option<PathBuf>,
	pub workshop_acf: Option<PathBuf>,
	pub steam_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopCatalog {
	inner: SteamWorkshopCatalog,
	library_paths: Vec<(PathBuf, PathBuf)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopItemVersion {
	pub identity: WorkshopInstallIdentity,
	pub size_bytes: u64,
	pub time_updated: u64,
	pub ugc_handle: Option<String>,
	pub content_path: PathBuf,
	pub manifest_path: PathBuf,
}

impl WorkshopCatalog {
	pub fn new(inner: SteamWorkshopCatalog) -> Self {
		let library_paths = inner
			.libraries()
			.iter()
			.map(|library| (library.content_root.clone(), library.manifest_path.clone()))
			.collect();
		Self {
			inner,
			library_paths,
		}
	}

	pub fn from_override(
		app_id: u32,
		content_root: impl Into<PathBuf>,
		manifest_path: impl Into<PathBuf>,
	) -> Result<Self, String> {
		SteamWorkshopCatalog::from_override(app_id, content_root, manifest_path)
			.map(Self::new)
			.map_err(|error| error.to_string())
	}

	pub fn app_id(&self) -> u32 {
		self.inner.app_id
	}

	pub fn content_roots(&self) -> Vec<&Path> {
		self.library_paths
			.iter()
			.map(|(content_root, _)| content_root.as_path())
			.collect()
	}

	pub fn resolve(&self, workshop_id: &str) -> Option<PathBuf> {
		self.require_item(workshop_id)
			.ok()
			.map(|item| item.content_path)
	}

	pub fn contains(&self, workshop_id: &str) -> bool {
		self.resolve(workshop_id).is_some()
	}

	pub fn require_item(&self, workshop_id: &str) -> Result<WorkshopItemVersion, String> {
		let workshop_id = workshop_id.parse::<SteamId>()?;
		let item = self
			.inner
			.require_item(&workshop_id)
			.map_err(|error| error.to_string())?;
		workshop_item_version(item)
	}

	/// Re-read the exact same content-root/ACF pairs. This never rediscovers a
	/// different Steam library and never opens either location for writing.
	pub fn reload(&self) -> Result<Self, String> {
		SteamWorkshopCatalog::from_library_paths(self.inner.app_id, self.library_paths.clone())
			.map(Self::new)
			.map_err(|error| error.to_string())
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
	let workshop_acf_override = overrides
		.workshop_acf
		.clone()
		.or_else(|| std::env::var_os(STEAM_WORKSHOP_ACF_ENV).map(PathBuf::from));
	resolve_eu4(
		game_override,
		workshop_override,
		workshop_acf_override,
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
	workshop_acf_override: Option<PathBuf>,
	steam_override: Option<PathBuf>,
	config: &Config,
) -> Result<Eu4Discovery, String> {
	let game = resolve_eu4_game(game_override, steam_override, config)?;
	let workshop = match workshop_override {
		Some(content_root) => {
			let manifest_path = workshop_acf_override
				.or_else(|| infer_workshop_manifest_path(&content_root))
				.ok_or_else(|| {
					format!(
						"an explicit Workshop directory requires its read-only ACF; set {STEAM_WORKSHOP_ACF_ENV}"
					)
				})?;
			SteamWorkshopCatalog::from_override(EU4_APPID, content_root, manifest_path)
				.map_err(|error| error.to_string())?
		}
		None => {
			if workshop_acf_override.is_some() {
				return Err(format!(
					"{STEAM_WORKSHOP_ACF_ENV} requires {STEAM_WORKSHOP_DIR_ENV}"
				));
			}
			let root = game.steam_root.as_deref().ok_or_else(|| {
				format!(
					"could not locate Steam for EU4 Workshop discovery; pass --steam-root or set {STEAM_WORKSHOP_DIR_ENV} and {STEAM_WORKSHOP_ACF_ENV}"
				)
			})?;
			SteamWorkshopCatalog::discover_from_steam_root(root, EU4_APPID)
				.map_err(|error| error.to_string())?
		}
	};
	Ok(Eu4Discovery {
		game_root: game.game_root,
		game_version: game.game_version,
		steam_build_id: game.steam_build_id,
		steam_root: game.steam_root,
		workshop: WorkshopCatalog::new(workshop),
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

fn workshop_item_version(item: &InstalledWorkshopItem) -> Result<WorkshopItemVersion, String> {
	Ok(WorkshopItemVersion {
		identity: item.identity().map_err(|error| error.to_string())?,
		size_bytes: item.size_bytes,
		time_updated: item.time_updated,
		ugc_handle: item.ugc_handle.as_ref().map(ToString::to_string),
		content_path: item.content_path.clone(),
		manifest_path: item.manifest_path.clone(),
	})
}

fn infer_workshop_manifest_path(content_root: &Path) -> Option<PathBuf> {
	if content_root.file_name()? != OsStr::new(&EU4_APPID.to_string()) {
		return None;
	}
	let content = content_root.parent()?;
	if content.file_name()? != OsStr::new("content") {
		return None;
	}
	let workshop = content.parent()?;
	if workshop.file_name()? != OsStr::new("workshop")
		|| workshop.parent()?.file_name()? != OsStr::new("steamapps")
	{
		return None;
	}
	Some(workshop.join(format!("appworkshop_{EU4_APPID}.acf")))
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

	fn write_workshop_manifest(path: &Path, workshop_id: &str, manifest_id: &str) {
		fs::write(
			path,
			format!(
				r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
		"{workshop_id}"
		{{
			"size" "12"
			"timeupdated" "1780000000"
			"manifest" "{manifest_id}"
		}}
	}}
}}"#
			),
		)
		.unwrap();
	}

	fn write_library_folders(steam: &Path, libraries: &[&Path]) {
		let entries = libraries
			.iter()
			.enumerate()
			.map(|(index, path)| {
				format!("\t\"{index}\" {{ \"path\" \"{}\" }}", vdf_path_value(path))
			})
			.collect::<Vec<_>>()
			.join("\n");
		fs::write(
			steam.join("steamapps/libraryfolders.vdf"),
			format!("\"libraryfolders\"\n{{\n{entries}\n}}"),
		)
		.unwrap();
	}

	fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
		let temp = tempfile::tempdir().unwrap();
		let steam = temp.path().join("Steam");
		let library = temp.path().join("Library");
		let game = library.join("steamapps/common/Europa Universalis IV");
		let workshop = library.join("steamapps/workshop/content/236850");
		let workshop_acf = library.join("steamapps/workshop/appworkshop_236850.acf");
		fs::create_dir_all(steam.join("steamapps")).unwrap();
		fs::create_dir_all(&game).unwrap();
		fs::create_dir_all(&workshop).unwrap();
		fs::write(game.join("version.txt"), "1.37.5\n").unwrap();
		write_library_folders(&steam, &[&steam, &library]);
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
		write_workshop_manifest(&workshop_acf, "42", "18446744073709551614");
		fs::create_dir_all(workshop.join("42")).unwrap();
		(temp, steam, library, game, workshop)
	}

	#[test]
	fn steam_discovery_finds_game_build_and_secondary_workshop() {
		let (_temp, steam, _library, game, workshop) = fixture();
		let resolved =
			resolve_eu4(None, None, None, Some(steam.clone()), &Config::default()).unwrap();
		assert_eq!(resolved.game_root, game);
		assert_eq!(resolved.game_version, "1.37.5");
		assert_eq!(resolved.steam_build_id, Some(4242));
		let canonical_workshop = fs::canonicalize(&workshop).unwrap();
		assert_eq!(
			resolved.workshop.content_roots(),
			vec![canonical_workshop.as_path()]
		);
		assert_eq!(
			resolved
				.workshop
				.require_item("42")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"18446744073709551614"
		);
		assert_eq!(resolved.steam_root, Some(steam));
	}

	#[test]
	fn explicit_paths_take_precedence_over_config() {
		let (temp, _steam, _library, _game, _workshop) = fixture();
		let explicit_game = temp.path().join("explicit-game");
		let explicit_library = temp.path().join("ExplicitLibrary");
		let explicit_workshop = explicit_library.join("steamapps/workshop/content/236850");
		let explicit_acf = explicit_library.join("steamapps/workshop/appworkshop_236850.acf");
		fs::create_dir_all(&explicit_game).unwrap();
		fs::create_dir_all(&explicit_workshop).unwrap();
		fs::create_dir_all(explicit_workshop.join("42")).unwrap();
		write_workshop_manifest(&explicit_acf, "42", "18446744073709551614");
		fs::write(explicit_game.join("version.txt"), "9.9.9\n").unwrap();
		let mut config = Config::default();
		config
			.game_path
			.insert("eu4".to_string(), temp.path().join("wrong-game"));
		let resolved = resolve_eu4(
			Some(explicit_game.clone()),
			Some(explicit_workshop.clone()),
			Some(explicit_acf),
			None,
			&config,
		)
		.unwrap();
		assert_eq!(resolved.game_root, explicit_game);
		let canonical_workshop = fs::canonicalize(&explicit_workshop).unwrap();
		assert_eq!(
			resolved.workshop.content_roots(),
			vec![canonical_workshop.as_path()]
		);
		assert_eq!(resolved.game_version, "9.9.9");
	}

	#[test]
	fn workshop_catalog_resolves_across_roots() {
		let temp = tempfile::tempdir().unwrap();
		let content = temp
			.path()
			.join("Library/steamapps/workshop/content/236850");
		let acf = temp
			.path()
			.join("Library/steamapps/workshop/appworkshop_236850.acf");
		fs::create_dir_all(content.join("42")).unwrap();
		write_workshop_manifest(&acf, "42", "18446744073709551614");
		let catalog = WorkshopCatalog::new(
			SteamWorkshopCatalog::from_override(EU4_APPID, &content, &acf).unwrap(),
		);
		assert_eq!(
			catalog.resolve("42"),
			Some(fs::canonicalize(&content).unwrap().join("42"))
		);
		write_workshop_manifest(&acf, "42", "7");
		let reloaded = catalog.reload().unwrap();
		assert_eq!(
			catalog
				.require_item("42")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"18446744073709551614"
		);
		assert_eq!(
			reloaded
				.require_item("42")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"7"
		);
		assert_eq!(reloaded.content_roots(), catalog.content_roots());
	}

	#[test]
	fn explicit_workshop_override_rejects_cross_library_acf() {
		let (temp, _steam, _library, game, workshop) = fixture();
		let other_library = temp.path().join("OtherLibrary");
		let other_content = other_library.join("steamapps/workshop/content/236850");
		let other_acf = other_library.join("steamapps/workshop/appworkshop_236850.acf");
		fs::create_dir_all(&other_content).unwrap();
		write_workshop_manifest(&other_acf, "42", "99");

		let error = resolve_eu4(
			Some(game),
			Some(workshop),
			Some(other_acf),
			None,
			&Config::default(),
		)
		.unwrap_err();
		assert!(error.contains("same-library"), "{error}");
	}

	#[test]
	fn standard_override_infers_acf_but_nonstandard_path_does_not() {
		let (temp, _steam, _library, game, workshop) = fixture();
		let resolved = resolve_eu4(
			Some(game.clone()),
			Some(workshop),
			None,
			None,
			&Config::default(),
		)
		.unwrap();
		assert_eq!(
			resolved
				.workshop
				.require_item("42")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"18446744073709551614"
		);

		let nonstandard = temp.path().join("custom/content");
		fs::create_dir_all(nonstandard.join("42")).unwrap();
		let accidentally_adjacent = temp.path().join("appworkshop_236850.acf");
		write_workshop_manifest(&accidentally_adjacent, "42", "9");
		let error = resolve_eu4(
			Some(game.clone()),
			Some(nonstandard),
			None,
			None,
			&Config::default(),
		)
		.unwrap_err();
		assert!(error.contains("requires its read-only ACF"));

		let error = resolve_eu4(
			Some(game),
			None,
			Some(accidentally_adjacent),
			None,
			&Config::default(),
		)
		.unwrap_err();
		assert!(error.contains("STEAM_WORKSHOP_ACF requires STEAM_WORKSHOP_DIR"));
	}

	#[test]
	fn steam_discovery_combines_multiple_paired_workshop_libraries() {
		let (temp, steam, library, _game, first_workshop) = fixture();
		let second_library = temp.path().join("SecondLibrary");
		let second_workshop = second_library.join("steamapps/workshop/content/236850");
		let second_acf = second_library.join("steamapps/workshop/appworkshop_236850.acf");
		fs::create_dir_all(second_workshop.join("99")).unwrap();
		write_workshop_manifest(&second_acf, "99", "10");
		write_library_folders(&steam, &[&steam, &library, &second_library]);

		let resolved = resolve_eu4(None, None, None, Some(steam), &Config::default()).unwrap();
		let canonical_first = fs::canonicalize(&first_workshop).unwrap();
		let canonical_second = fs::canonicalize(&second_workshop).unwrap();
		assert_eq!(
			resolved.workshop.content_roots(),
			vec![canonical_first.as_path(), canonical_second.as_path()]
		);
		assert_eq!(
			resolved
				.workshop
				.require_item("42")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"18446744073709551614"
		);
		assert_eq!(
			resolved
				.workshop
				.require_item("99")
				.unwrap()
				.identity
				.manifest_id
				.as_str(),
			"10"
		);
	}
}
