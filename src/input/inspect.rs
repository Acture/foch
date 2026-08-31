use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::game::eu4::Eu4;
use crate::game::eu4::base::snapshot::{
	InstalledBaseSnapshotIdentity, installed_base_snapshot_identity, resolve_game_root_and_version,
};
use crate::input::config::{Config, get_config_dir_path};
use crate::input::request::InputRequest;
use crate::playset::descriptor::load_descriptor;
use crate::playset::steam::{
	SteamId, SteamWorkshopCatalog, SteamWorkshopError, find_steam_root_path,
};
use crate::playset::{Playset, default_dlc_load_path};

const CURRENT_PLAYSET_NAME: &str = "Current EU4 playset";
const GAME_NAME: &str = "Europa Universalis IV";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputReadiness {
	Ready,
	ReadyWithOmissions,
	Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseDataState {
	Ready,
	Missing,
	Stale,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledGameInspection {
	pub name: String,
	pub version: Option<String>,
	pub install_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseDataInspection {
	pub state: BaseDataState,
	pub version: Option<String>,
	pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedPlaysetMod {
	pub id: String,
	pub name: String,
	pub position: usize,
	pub enabled: bool,
	pub workshop_id: Option<String>,
	pub workshop_manifest_id: Option<String>,
	pub version: Option<String>,
	pub declared_dependencies: Vec<String>,
	pub descriptor_path: Option<PathBuf>,
	pub source_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedPlayset {
	pub name: String,
	pub source_path: PathBuf,
	pub mods: Vec<DetectedPlaysetMod>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputReadinessIssue {
	pub id: String,
	pub title: String,
	pub detail: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub action: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedPlaysetMod {
	pub id: String,
	pub name: String,
	pub position: usize,
	pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableInputRecovery {
	pub source_mod_count: usize,
	pub omitted_mods: Vec<OmittedPlaysetMod>,
	pub included_mod_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputPreparationMode {
	Complete,
	AvailableOnly,
}

#[derive(Clone, Debug)]
pub struct PreparedAnalysisInput {
	pub request: InputRequest,
	pub source_mod_count: usize,
	pub recovery: Option<AvailableInputRecovery>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentEu4Input {
	pub readiness: InputReadiness,
	pub game: InstalledGameInspection,
	pub base_data: BaseDataInspection,
	pub playset: Option<DetectedPlayset>,
	pub issues: Vec<InputReadinessIssue>,
	pub recovery: Option<AvailableInputRecovery>,
	#[serde(skip)]
	prepared: Option<PreparedCurrentEu4Input>,
}

#[derive(Clone, Debug)]
struct PreparedCurrentEu4Input {
	config: Config,
	game_root: PathBuf,
	playset_path: PathBuf,
	playset: Playset,
	source_mod_count: usize,
	recovery: Option<AvailableInputRecovery>,
	base_snapshot_identity: InstalledBaseSnapshotIdentity,
}

#[derive(Clone, Debug)]
pub(crate) struct CurrentEu4InputEnvironment {
	pub config_dir: Result<PathBuf, String>,
	pub discovered_steam_root: Option<PathBuf>,
	pub default_paradox_data_path: Option<PathBuf>,
}

impl CurrentEu4InputEnvironment {
	fn discover() -> Self {
		Self {
			config_dir: get_config_dir_path().map_err(|error| error.to_string()),
			discovered_steam_root: find_steam_root_path(),
			default_paradox_data_path: dirs::document_dir().map(|documents| {
				documents
					.join("Paradox Interactive")
					.join(Eu4::PARADOX_DATA_DIR_NAME)
			}),
		}
	}
}

impl CurrentEu4Input {
	pub fn into_request(self) -> Option<InputRequest> {
		self.prepare(InputPreparationMode::Complete)
			.map(|prepared| prepared.request)
	}

	pub fn prepare(self, mode: InputPreparationMode) -> Option<PreparedAnalysisInput> {
		let prepared = self.prepared?;
		let prepared_mode = if prepared.recovery.is_some() {
			InputPreparationMode::AvailableOnly
		} else {
			InputPreparationMode::Complete
		};
		if mode != prepared_mode {
			return None;
		}
		let identity_label = prepared.base_snapshot_identity.as_label();
		Some(PreparedAnalysisInput {
			request: InputRequest::from_playset_path(prepared.playset_path, prepared.config)
				.with_expected_base_snapshot_identity(identity_label)
				.with_base_snapshot_lease(Some(prepared.base_snapshot_identity))
				.with_expected_game_root(prepared.game_root)
				.with_preloaded_playset(prepared.playset),
			source_mod_count: prepared.source_mod_count,
			recovery: prepared.recovery,
		})
	}
}

pub fn inspect_current_eu4_input() -> CurrentEu4Input {
	inspect_current_eu4_input_with_environment(CurrentEu4InputEnvironment::discover())
}

pub(crate) fn inspect_current_eu4_input_with_environment(
	environment: CurrentEu4InputEnvironment,
) -> CurrentEu4Input {
	let mut issues = Vec::new();
	let mut config = load_existing_config(&environment.config_dir, &mut issues);
	if config.steam_root_path.is_none() {
		config.steam_root_path = environment.discovered_steam_root;
	}
	if config.paradox_data_path.is_none() {
		config.paradox_data_path = environment.default_paradox_data_path;
	}

	let (game_root, game_version) = inspect_game(&config, &mut issues);
	if let Some(game_root) = game_root.as_ref() {
		config
			.game_path
			.insert(Eu4::KEY.to_string(), game_root.clone());
	}
	let (playset_view, prepared_playset, playset_recovery) = inspect_playset(&config, &mut issues);
	let source_mod_count = playset_view
		.as_ref()
		.map_or(0, |playset| playset.mods.len());
	let (base_data, base_snapshot_identity) =
		inspect_base_data(game_version.as_deref(), &mut issues);

	let prepared = match (
		prepared_playset,
		base_snapshot_identity,
		game_root.as_ref(),
		game_version.as_ref(),
	) {
		(Some((playset_path, playset)), Some(base_snapshot_identity), Some(game_root), Some(_)) => {
			Some(PreparedCurrentEu4Input {
				config,
				game_root: game_root.clone(),
				playset_path,
				playset,
				source_mod_count,
				recovery: playset_recovery.clone(),
				base_snapshot_identity,
			})
		}
		_ => None,
	};
	let recovery = prepared
		.as_ref()
		.and_then(|prepared| prepared.recovery.clone());

	CurrentEu4Input {
		readiness: match (prepared.is_some(), recovery.is_some()) {
			(false, _) => InputReadiness::Blocked,
			(true, false) => InputReadiness::Ready,
			(true, true) => InputReadiness::ReadyWithOmissions,
		},
		game: InstalledGameInspection {
			name: GAME_NAME.to_string(),
			version: game_version,
			install_path: game_root,
		},
		base_data,
		playset: playset_view,
		issues,
		recovery,
		prepared,
	}
}

fn load_existing_config(
	config_dir: &Result<PathBuf, String>,
	issues: &mut Vec<InputReadinessIssue>,
) -> Config {
	let config_dir = match config_dir {
		Ok(path) => path,
		Err(detail) => {
			issues.push(issue(
				"config_directory_unavailable",
				"Configuration directory is unavailable",
				detail.clone(),
				Some("Select the Steam, EU4, and Paradox data paths in Foch."),
			));
			return Config::default();
		}
	};
	let path = config_dir.join("config.toml");
	if !path.is_file() {
		return Config::default();
	}
	match Config::load_config(&path) {
		Ok(config) => config,
		Err(error) => {
			issues.push(issue(
				"config_invalid",
				"Foch configuration could not be read",
				format!("{}: {error}", path.display()),
				Some("Correct config.toml or select the paths again in Foch."),
			));
			Config::default()
		}
	}
}

fn inspect_game(
	config: &Config,
	issues: &mut Vec<InputReadinessIssue>,
) -> (Option<PathBuf>, Option<String>) {
	match resolve_game_root_and_version(config, &Eu4) {
		Ok((root, version)) => (Some(root), Some(version)),
		Err(detail) => {
			issues.push(issue(
				"eu4_install_unavailable",
				"Europa Universalis IV is not ready",
				detail,
				Some("Select the EU4 installation directory."),
			));
			(None, None)
		}
	}
}

fn inspect_playset(
	config: &Config,
	issues: &mut Vec<InputReadinessIssue>,
) -> (
	Option<DetectedPlayset>,
	Option<(PathBuf, Playset)>,
	Option<AvailableInputRecovery>,
) {
	let candidates = paradox_data_candidates(config.paradox_data_path.as_deref());
	let playset_path = candidates
		.iter()
		.map(|path| default_dlc_load_path(path))
		.find(|path| path.is_file())
		.or_else(|| candidates.first().map(|path| default_dlc_load_path(path)));
	let Some(playset_path) = playset_path else {
		issues.push(issue(
			"paradox_data_unavailable",
			"EU4 user data directory is not configured",
			"Foch could not determine where the current EU4 dlc_load.json lives.",
			Some("Select the EU4 user data directory."),
		));
		return (None, None, None);
	};
	let mut playset = match Playset::from_dlc_load_with_required_descriptors(&playset_path) {
		Ok(playset) => playset,
		Err(error) => {
			issues.push(issue(
				"current_playset_unavailable",
				"Current EU4 playset could not be read",
				error.to_string(),
				Some("Start the EU4 Launcher and select a playset, then retry."),
			));
			return (None, None, None);
		}
	};
	playset.name = CURRENT_PLAYSET_NAME.to_string();

	let catalog = config
		.steam_root_path
		.as_deref()
		.ok_or_else(|| "Steam is not configured or discoverable".to_string())
		.and_then(|root| {
			SteamWorkshopCatalog::discover_from_steam_root(root, Eu4::STEAM_APP_ID)
				.map_err(|error| error.to_string())
		});
	if let Err(detail) = &catalog {
		issues.push(issue(
			"workshop_catalog_unavailable",
			"Steam Workshop installation data is unavailable",
			detail.clone(),
			Some("Select the Steam installation directory and retry."),
		));
	}

	let catalog_ready = catalog.is_ok();
	let mut has_blocking_mod_error = false;
	let mut omittable_mods = Vec::with_capacity(playset.mods.len());
	let mut detected_mods = Vec::with_capacity(playset.mods.len());
	for (index, entry) in playset.mods.iter_mut().enumerate() {
		let workshop_id = entry.steam_id.clone();
		let mut detected = DetectedPlaysetMod {
			id: workshop_id
				.clone()
				.unwrap_or_else(|| format!("playset-entry-{}", index + 1)),
			name: entry
				.display_name
				.clone()
				.unwrap_or_else(|| "Unknown mod".to_string()),
			position: index + 1,
			enabled: entry.enabled,
			workshop_id: workshop_id.clone(),
			workshop_manifest_id: None,
			version: None,
			declared_dependencies: Vec::new(),
			descriptor_path: None,
			source_error: None,
		};
		let enrichment = workshop_id
			.as_deref()
			.ok_or_else(|| ("playset entry has no Workshop id".to_string(), false))
			.and_then(|id| {
				id.parse::<SteamId>()
					.map_err(|error| (error.to_string(), false))
			})
			.and_then(|id| {
				let catalog = catalog.as_ref().map_err(|error| (error.clone(), false))?;
				let item = catalog.require_item(&id).map_err(|error| {
					let omittable = matches!(
						&error,
						SteamWorkshopError::MissingItem { .. }
							| SteamWorkshopError::UnavailableManifest { .. }
							| SteamWorkshopError::MissingItemContent { .. }
					);
					(error.to_string(), omittable)
				})?;
				let identity = item
					.identity()
					.map_err(|error| (error.to_string(), false))?;
				let descriptor_path = item.content_path.join("descriptor.mod");
				detected.descriptor_path = Some(descriptor_path.clone());
				let descriptor = load_descriptor(&descriptor_path)
					.map_err(|error| (error.to_string(), false))?;
				if descriptor.name.trim().is_empty() {
					return Err((
						format!(
							"Workshop descriptor {} has an empty name",
							descriptor_path.display()
						),
						false,
					));
				}
				Ok((
					item.content_path.clone(),
					identity,
					descriptor_path,
					descriptor,
				))
			});
		match enrichment {
			Ok((root_path, identity, descriptor_path, descriptor)) => {
				omittable_mods.push(false);
				detected.name = descriptor.name.clone();
				detected.workshop_manifest_id = Some(identity.manifest_id.to_string());
				detected.version = descriptor.version.clone();
				detected.declared_dependencies = descriptor.dependencies.clone();
				detected.descriptor_path = Some(descriptor_path);
				entry.display_name = Some(descriptor.name);
				entry.root_path = Some(root_path);
				entry.workshop_identity = Some(identity);
			}
			Err((error, omittable)) => {
				omittable_mods.push(omittable);
				if !omittable {
					has_blocking_mod_error = true;
				}
				detected.source_error = Some(error.clone());
				issues.push(issue(
					format!("workshop_mod_unavailable_{}", index + 1),
					format!("Workshop mod {} is not ready", detected.id),
					error,
					Some(if omittable {
						"Analyze the remaining installed mods, or remove/reinstall this item in the EU4 Launcher."
					} else {
						"Repair or redownload this Workshop item in Steam."
					}),
				));
			}
		}
		detected_mods.push(detected);
	}

	let detected = DetectedPlayset {
		name: CURRENT_PLAYSET_NAME.to_string(),
		source_path: playset_path.clone(),
		mods: detected_mods,
	};
	let omitted_mods = detected
		.mods
		.iter()
		.zip(&omittable_mods)
		.filter(|(_, omittable)| **omittable)
		.filter_map(|(playset_mod, _)| {
			playset_mod
				.source_error
				.as_ref()
				.map(|reason| OmittedPlaysetMod {
					id: playset_mod.id.clone(),
					name: playset_mod.name.clone(),
					position: playset_mod.position,
					reason: reason.clone(),
				})
		})
		.collect::<Vec<_>>();
	let prepared = if catalog_ready && !has_blocking_mod_error {
		playset
			.mods
			.retain(|entry| entry.root_path.is_some() && entry.workshop_identity.is_some());
		if playset.mods.is_empty() && !omitted_mods.is_empty() {
			issues.push(issue(
				"no_available_workshop_mods",
				"No installed Workshop mods remain to analyze",
				"Every enabled Workshop item in the current playset is unavailable.",
				Some("Repair the missing items or select another playset in the EU4 Launcher."),
			));
			None
		} else {
			Some((playset_path, playset))
		}
	} else {
		None
	};
	let recovery = prepared.as_ref().and_then(|(_, playset)| {
		(!omitted_mods.is_empty()).then_some(AvailableInputRecovery {
			source_mod_count: detected.mods.len(),
			omitted_mods,
			included_mod_count: playset.mods.len(),
		})
	});
	(Some(detected), prepared, recovery)
}

fn paradox_data_candidates(path: Option<&Path>) -> Vec<PathBuf> {
	let Some(path) = path else {
		return Vec::new();
	};
	let mut candidates = vec![path.to_path_buf()];
	if path.file_name().and_then(|name| name.to_str()) != Some(Eu4::PARADOX_DATA_DIR_NAME) {
		candidates.push(path.join(Eu4::PARADOX_DATA_DIR_NAME));
	}
	candidates
}

fn inspect_base_data(
	game_version: Option<&str>,
	issues: &mut Vec<InputReadinessIssue>,
) -> (BaseDataInspection, Option<InstalledBaseSnapshotIdentity>) {
	let Some(game_version) = game_version else {
		return (
			BaseDataInspection {
				state: BaseDataState::Missing,
				version: None,
				detail: "EU4 must be located before matching base data can be selected."
					.to_string(),
			},
			None,
		);
	};
	match installed_base_snapshot_identity(Eu4::KEY, game_version) {
		Ok(Some(identity)) => (
			BaseDataInspection {
				state: BaseDataState::Ready,
				version: Some(game_version.to_string()),
				detail: format!("Installed base data identity: {}", identity.as_label()),
			},
			Some(identity),
		),
		Ok(None) => {
			let detail = format!("Base data for EU4 {game_version} is not installed.");
			issues.push(issue(
				"base_data_missing",
				"Matching EU4 base data is missing",
				detail.clone(),
				Some("Install base data for the detected EU4 version."),
			));
			(
				BaseDataInspection {
					state: BaseDataState::Missing,
					version: Some(game_version.to_string()),
					detail,
				},
				None,
			)
		}
		Err(detail) => {
			issues.push(issue(
				"base_data_stale",
				"Installed EU4 base data is stale or invalid",
				detail.clone(),
				Some("Reinstall base data for the detected EU4 version."),
			));
			(
				BaseDataInspection {
					state: BaseDataState::Stale,
					version: Some(game_version.to_string()),
					detail,
				},
				None,
			)
		}
	}
}

fn issue(
	id: impl Into<String>,
	title: impl Into<String>,
	detail: impl Into<String>,
	action: Option<&str>,
) -> InputReadinessIssue {
	InputReadinessIssue {
		id: id.into(),
		title: title.into(),
		detail: detail.into(),
		action: action.map(str::to_string),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::game::eu4::base::snapshot::{
		BASE_DATA_DIR_ENV, BASE_DATA_ENV_LOCK, BaseDataSource, build_base_snapshot,
		install_built_snapshot,
	};
	use crate::input::resolve::build_input_inventory;
	use crate::input::{FileFilter, resolve_product_input_manifest};
	use std::ffi::OsString;
	use std::fs;
	use std::time::SystemTime;
	use tempfile::TempDir;
	use walkdir::WalkDir;

	const GAME_VERSION: &str = "1.37.5";
	const FIRST_ID: &str = "42";
	const SECOND_ID: &str = "43";
	const LARGE_MANIFEST_ID: &str = "9007199254740993";

	struct EnvVarGuard {
		key: &'static str,
		previous: Option<OsString>,
	}

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			// SAFETY: tests changing the process-wide base-data override hold
			// BASE_DATA_ENV_LOCK for the entire lifetime of this guard.
			unsafe { std::env::set_var(key, value) };
			Self { key, previous }
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			// SAFETY: the caller still holds BASE_DATA_ENV_LOCK while guards drop.
			unsafe {
				match &self.previous {
					Some(value) => std::env::set_var(self.key, value),
					None => std::env::remove_var(self.key),
				}
			}
		}
	}

	fn fixture_environment(root: &Path) -> CurrentEu4InputEnvironment {
		CurrentEu4InputEnvironment {
			config_dir: Ok(root.join("config")),
			discovered_steam_root: Some(root.join("Steam")),
			default_paradox_data_path: Some(root.join("Paradox EU4")),
		}
	}

	fn setup_input_fixture(root: &Path) -> (PathBuf, PathBuf) {
		let steam_root = root.join("Steam");
		let steamapps = steam_root.join("steamapps");
		let game_root = steamapps.join("common").join(Eu4::PARADOX_DATA_DIR_NAME);
		fs::create_dir_all(&game_root).expect("create game root");
		fs::write(game_root.join("version.txt"), format!("{GAME_VERSION}\n"))
			.expect("write game version");
		fs::write(
			steamapps.join("libraryfolders.vdf"),
			format!(
				r#""libraryfolders"
{{
	"0" {{ "path" "{}" }}
}}"#,
				steam_root.to_string_lossy().replace('\\', "\\\\")
			),
		)
		.expect("write Steam library list");
		fs::write(
			steamapps.join("appmanifest_236850.acf"),
			r#""AppState"
{
	"appid" "236850"
	"installdir" "Europa Universalis IV"
	"buildid" "123456"
}"#,
		)
		.expect("write Steam app manifest");

		let workshop_root = steamapps.join("workshop");
		let content_root = workshop_root
			.join("content")
			.join(Eu4::STEAM_APP_ID.to_string());
		fs::create_dir_all(content_root.join(FIRST_ID)).expect("create first Workshop item");
		fs::create_dir_all(content_root.join(SECOND_ID)).expect("create second Workshop item");
		write_two_item_workshop_manifest(&workshop_root.join("appworkshop_236850.acf"), "44");
		fs::write(
			content_root.join(FIRST_ID).join("descriptor.mod"),
			r#"name="First Workshop Mod"
version="2.4.1"
dependencies={
	"Foundation"
	"Shared Assets"
}
remote_file_id="42"
"#,
		)
		.expect("write first descriptor");
		fs::write(
			content_root.join(SECOND_ID).join("descriptor.mod"),
			r#"name="Second Workshop Mod"
version="3.0"
dependencies={ "First Workshop Mod" }
remote_file_id="43"
"#,
		)
		.expect("write second descriptor");

		let paradox_root = root.join("Paradox EU4");
		fs::create_dir_all(paradox_root.join("mod")).expect("create Launcher mod dir");
		fs::write(
			paradox_root.join("dlc_load.json"),
			format!(
				r#"{{"enabled_mods":["mod/ugc_{FIRST_ID}.mod","mod/ugc_{SECOND_ID}.mod"],"disabled_dlcs":[]}}"#
			),
		)
		.expect("write playset");
		for (id, name) in [(FIRST_ID, "Launcher First"), (SECOND_ID, "Launcher Second")] {
			fs::write(
				paradox_root.join("mod").join(format!("ugc_{id}.mod")),
				format!("name=\"{name}\"\nremote_file_id=\"{id}\"\n"),
			)
			.expect("write Launcher descriptor");
		}
		(game_root, content_root)
	}

	fn write_two_item_workshop_manifest(path: &Path, second_manifest_id: &str) {
		fs::write(
			path,
			format!(
				r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
		"{FIRST_ID}"
		{{
			"size" "10"
			"timeupdated" "1"
			"manifest" "{LARGE_MANIFEST_ID}"
		}}
		"{SECOND_ID}"
		{{
			"size" "20"
			"timeupdated" "2"
			"manifest" "{second_manifest_id}"
		}}
	}}
}}"#
			),
		)
		.expect("write Workshop ACF");
	}

	fn install_matching_base_data(game_root: &Path) {
		let built = build_base_snapshot(
			&Eu4,
			game_root,
			Some(GAME_VERSION),
			&FileFilter::for_game(Eu4),
		)
		.expect("build base fixture");
		install_built_snapshot(
			&built.encoded_snapshot,
			BaseDataSource::Build,
			Some(built.snapshot_asset_name),
			Some(built.snapshot_sha256),
		)
		.expect("install base fixture");
	}

	#[derive(Debug, Eq, PartialEq)]
	struct FileSystemEntrySnapshot {
		path: String,
		kind: &'static str,
		permissions: u32,
		modified: Option<SystemTime>,
		bytes: Option<Vec<u8>>,
		link_target: Option<PathBuf>,
	}

	fn permission_bits(metadata: &fs::Metadata) -> u32 {
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			metadata.permissions().mode()
		}
		#[cfg(not(unix))]
		{
			u32::from(metadata.permissions().readonly())
		}
	}

	fn file_system_snapshot(root: &Path) -> Vec<FileSystemEntrySnapshot> {
		let mut entries = WalkDir::new(root)
			.into_iter()
			.filter_map(Result::ok)
			.filter(|entry| entry.path() != root)
			.map(|entry| {
				let path = entry.path();
				let metadata = fs::symlink_metadata(path).expect("snapshot metadata");
				let file_type = metadata.file_type();
				FileSystemEntrySnapshot {
					path: path
						.strip_prefix(root)
						.expect("fixture-relative path")
						.to_string_lossy()
						.replace('\\', "/"),
					kind: if file_type.is_file() {
						"file"
					} else if file_type.is_dir() {
						"directory"
					} else if file_type.is_symlink() {
						"symlink"
					} else {
						"other"
					},
					permissions: permission_bits(&metadata),
					modified: file_type
						.is_file()
						.then(|| metadata.modified().expect("file modified time")),
					bytes: file_type
						.is_file()
						.then(|| fs::read(path).expect("snapshot file bytes")),
					link_target: file_type
						.is_symlink()
						.then(|| fs::read_link(path).expect("snapshot link target")),
				}
			})
			.collect::<Vec<_>>();
		entries.sort_by(|left, right| left.path.cmp(&right.path));
		entries
	}

	#[test]
	fn inspection_is_read_only_and_freezes_exact_product_input() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let data_root = temp.path().join("base-data");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &data_root);
		let (game_root, _) = setup_input_fixture(temp.path());
		install_matching_base_data(&game_root);
		let lock_path = data_root.join("eu4/.snapshot-1.37.5.lock");
		fs::remove_file(&lock_path).expect("remove fixture installation lock");
		let config_path = temp.path().join("config/config.toml");
		assert!(!lock_path.exists());
		assert!(!config_path.exists());
		let before = file_system_snapshot(temp.path());

		let inspection =
			inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));

		assert_eq!(inspection.readiness, InputReadiness::Ready);
		assert_eq!(inspection.game.version.as_deref(), Some(GAME_VERSION));
		assert_eq!(
			inspection.game.install_path.as_deref(),
			Some(game_root.as_path())
		);
		assert_eq!(inspection.base_data.state, BaseDataState::Ready);
		assert!(inspection.issues.is_empty());
		let playset = inspection.playset.as_ref().expect("detected playset");
		assert_eq!(playset.name, CURRENT_PLAYSET_NAME);
		assert_eq!(playset.mods.len(), 2);
		assert_eq!(playset.mods[0].position, 1);
		assert_eq!(playset.mods[0].name, "First Workshop Mod");
		assert_eq!(playset.mods[0].workshop_id.as_deref(), Some(FIRST_ID));
		assert_eq!(
			playset.mods[0].workshop_manifest_id.as_deref(),
			Some(LARGE_MANIFEST_ID)
		);
		assert_eq!(playset.mods[0].version.as_deref(), Some("2.4.1"));
		assert_eq!(
			playset.mods[0].declared_dependencies,
			["Foundation", "Shared Assets"]
		);
		assert_eq!(playset.mods[1].position, 2);
		assert_eq!(playset.mods[1].name, "Second Workshop Mod");
		assert_eq!(
			serde_json::to_value(&inspection).expect("serialize inspection")["playset"]["mods"][0]
				["workshopManifestId"],
			LARGE_MANIFEST_ID
		);
		assert_eq!(
			before,
			file_system_snapshot(temp.path()),
			"inspection must preserve paths, bytes, permissions, and file modification times"
		);
		assert!(!lock_path.exists());
		assert!(!config_path.exists());

		fs::write(
			temp.path().join("Paradox EU4/dlc_load.json"),
			r#"{"enabled_mods":[],"disabled_dlcs":[]}"#,
		)
		.expect("mutate live playset after inspection");
		let request = inspection.into_request().expect("prepared request");
		assert_eq!(
			request.config.game_path.get(Eu4::KEY),
			Some(&game_root),
			"prepared analysis must retain the exact inspected game root"
		);
		assert_eq!(request.expected_game_root.as_ref(), Some(&game_root));
		assert!(request.base_snapshot_lease.is_some());
		assert!(request.expected_base_snapshot_identity.is_some());
		let manifest = resolve_product_input_manifest(&request, None).expect("frozen manifest");
		assert_eq!(manifest.mods.len(), 2);
		assert_eq!(manifest.mods[0].mod_id, FIRST_ID);
		assert_eq!(manifest.mods[0].precedence, 1);
		assert_eq!(
			manifest.mods[0].workshop_identity.manifest_id.as_str(),
			LARGE_MANIFEST_ID
		);
		assert_eq!(manifest.mods[1].mod_id, SECOND_ID);
		assert_eq!(manifest.mods[1].precedence, 2);

		let replacement_game_root = temp.path().join("Steam/steamapps/common/Relocated EU4");
		fs::rename(&game_root, &replacement_game_root).expect("relocate game after inspection");
		fs::write(
			temp.path().join("Steam/steamapps/appmanifest_236850.acf"),
			r#""AppState"
{
	"appid" "236850"
	"installdir" "Relocated EU4"
	"buildid" "654321"
}"#,
		)
		.expect("redirect Steam game install");
		let error = build_input_inventory(&request, true)
			.expect_err("analysis must reject a different post-inspection game root");
		assert!(
			error.message.contains("inspected EU4 game root changed"),
			"{}",
			error.message
		);
	}

	#[test]
	fn missing_and_invalid_workshop_descriptors_block_preparation() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &temp.path().join("base-data"));
		let (_, content_root) = setup_input_fixture(temp.path());
		let second_descriptor = content_root.join(SECOND_ID).join("descriptor.mod");
		let canonical_second_descriptor =
			fs::canonicalize(&second_descriptor).expect("canonical descriptor path");
		fs::remove_file(&second_descriptor).expect("remove fixture descriptor");

		let missing = inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		assert_eq!(missing.readiness, InputReadiness::Blocked);
		let missing_mod = &missing.playset.as_ref().expect("missing playset").mods[1];
		assert!(missing_mod.source_error.is_some());
		assert_eq!(
			missing_mod.descriptor_path.as_deref(),
			Some(canonical_second_descriptor.as_path())
		);
		assert!(missing.into_request().is_none());

		fs::write(&second_descriptor, b"name={ invalid").expect("write invalid descriptor");
		let invalid = inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		let second = &invalid.playset.as_ref().expect("playset").mods[1];
		assert!(second.source_error.is_some());
		assert_eq!(
			second.descriptor_path.as_deref(),
			Some(canonical_second_descriptor.as_path())
		);
		assert!(invalid.into_request().is_none());

		fs::write(&second_descriptor, b"version=\"1\"\n").expect("write unnamed descriptor");
		let unnamed = inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		let second = &unnamed.playset.as_ref().expect("playset").mods[1];
		assert!(
			second
				.source_error
				.as_deref()
				.is_some_and(|error| error.contains("empty name"))
		);
		assert!(unnamed.into_request().is_none());
	}

	#[test]
	fn missing_workshop_item_can_be_explicitly_omitted_from_the_frozen_input() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let data_root = temp.path().join("base-data");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &data_root);
		let (game_root, _) = setup_input_fixture(temp.path());
		install_matching_base_data(&game_root);
		let manifest_path = temp
			.path()
			.join("Steam/steamapps/workshop/appworkshop_236850.acf");
		fs::write(
			&manifest_path,
			format!(
				r#""AppWorkshop"
{{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{{
		"{FIRST_ID}"
		{{
			"size" "10"
			"timeupdated" "1"
			"manifest" "{LARGE_MANIFEST_ID}"
		}}
	}}
}}"#
			),
		)
		.expect("remove second item from Workshop ACF");
		let before = file_system_snapshot(temp.path());

		let inspection =
			inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));

		assert_eq!(inspection.readiness, InputReadiness::ReadyWithOmissions);
		let recovery = inspection
			.recovery
			.clone()
			.expect("available-only recovery");
		assert_eq!(recovery.source_mod_count, 2);
		assert_eq!(recovery.included_mod_count, 1);
		assert_eq!(recovery.omitted_mods.len(), 1);
		assert_eq!(recovery.omitted_mods[0].id, SECOND_ID);
		assert_eq!(recovery.omitted_mods[0].name, "Launcher Second");
		assert_eq!(recovery.omitted_mods[0].position, 2);
		assert!(
			recovery.omitted_mods[0]
				.reason
				.contains("absent from appworkshop_236850.acf")
		);
		assert_eq!(
			before,
			file_system_snapshot(temp.path()),
			"available-only inspection must not modify Launcher, Workshop, game, or base data"
		);
		assert!(inspection.clone().into_request().is_none());
		let mut tampered = inspection.clone();
		tampered.readiness = InputReadiness::Ready;
		tampered.recovery = None;
		if let Some(playset) = tampered.playset.as_mut() {
			playset.mods.clear();
		}
		assert!(tampered.clone().into_request().is_none());

		fs::write(
			temp.path().join("Paradox EU4/dlc_load.json"),
			r#"{"enabled_mods":[],"disabled_dlcs":[]}"#,
		)
		.expect("mutate live playset after inspection");
		let prepared = tampered
			.prepare(InputPreparationMode::AvailableOnly)
			.expect("prepare available subset");
		assert_eq!(prepared.source_mod_count, 2);
		assert_eq!(prepared.recovery.as_ref(), Some(&recovery));
		let manifest =
			resolve_product_input_manifest(&prepared.request, None).expect("selected manifest");
		assert_eq!(manifest.mods.len(), 1);
		assert_eq!(manifest.mods[0].mod_id, FIRST_ID);
		assert_eq!(manifest.mods[0].precedence, 1);
	}

	#[test]
	fn unavailable_manifest_and_missing_content_are_recoverable_absences() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &temp.path().join("base-data"));
		let (game_root, content_root) = setup_input_fixture(temp.path());
		install_matching_base_data(&game_root);
		let manifest_path = temp
			.path()
			.join("Steam/steamapps/workshop/appworkshop_236850.acf");

		write_two_item_workshop_manifest(&manifest_path, "-1");
		let unavailable_manifest =
			inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		assert_eq!(
			unavailable_manifest.readiness,
			InputReadiness::ReadyWithOmissions
		);
		assert!(
			unavailable_manifest
				.recovery
				.as_ref()
				.is_some_and(|recovery| {
					recovery.omitted_mods.len() == 1
						&& recovery.omitted_mods[0].id == SECOND_ID
						&& recovery.omitted_mods[0].reason.contains("manifest = -1")
				})
		);

		write_two_item_workshop_manifest(&manifest_path, "44");
		fs::remove_dir_all(content_root.join(SECOND_ID)).expect("remove second item content");
		let missing_content =
			inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		assert_eq!(
			missing_content.readiness,
			InputReadiness::ReadyWithOmissions
		);
		assert!(missing_content.recovery.as_ref().is_some_and(|recovery| {
			recovery.omitted_mods.len() == 1
				&& recovery.omitted_mods[0].id == SECOND_ID
				&& recovery.omitted_mods[0]
					.reason
					.contains("has no content directory")
		}));
	}

	#[test]
	fn all_missing_workshop_items_do_not_offer_an_empty_analysis() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &temp.path().join("base-data"));
		let (game_root, _) = setup_input_fixture(temp.path());
		install_matching_base_data(&game_root);
		fs::write(
			temp.path()
				.join("Steam/steamapps/workshop/appworkshop_236850.acf"),
			r#""AppWorkshop"
{
	"appid" "236850"
	"WorkshopItemsInstalled" {}
}"#,
		)
		.expect("empty Workshop ACF");

		let inspection =
			inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));

		assert_eq!(inspection.readiness, InputReadiness::Blocked);
		assert!(inspection.recovery.is_none());
		assert!(inspection.issues.iter().any(|issue| {
			issue.id == "no_available_workshop_mods"
				&& issue.title == "No installed Workshop mods remain to analyze"
		}));
		assert!(
			inspection
				.prepare(InputPreparationMode::AvailableOnly)
				.is_none()
		);
	}

	#[test]
	fn missing_and_invalid_launcher_descriptors_block_preparation() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &temp.path().join("base-data"));
		setup_input_fixture(temp.path());
		let descriptor = temp
			.path()
			.join("Paradox EU4/mod")
			.join(format!("ugc_{SECOND_ID}.mod"));
		fs::remove_file(&descriptor).expect("remove Launcher descriptor");

		let missing = inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		assert_eq!(missing.readiness, InputReadiness::Blocked);
		assert!(missing.playset.is_none());
		assert!(missing.issues.iter().any(|issue| {
			issue.id == "current_playset_unavailable" && issue.detail.contains("ugc_43.mod")
		}));
		assert!(missing.into_request().is_none());

		fs::write(&descriptor, b"name={ invalid").expect("write invalid Launcher descriptor");
		let invalid = inspect_current_eu4_input_with_environment(fixture_environment(temp.path()));
		assert_eq!(invalid.readiness, InputReadiness::Blocked);
		assert!(invalid.playset.is_none());
		assert!(invalid.issues.iter().any(|issue| {
			issue.id == "current_playset_unavailable" && issue.detail.contains("ugc_43.mod")
		}));
		assert!(invalid.into_request().is_none());
	}

	#[test]
	fn missing_and_stale_base_data_are_distinct_without_creating_locks() {
		let _lock = BASE_DATA_ENV_LOCK
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		let temp = TempDir::new().expect("fixture root");
		let data_root = temp.path().join("base-data");
		let _data_guard = EnvVarGuard::set(BASE_DATA_DIR_ENV, &data_root);
		let lock_path = data_root.join("eu4/.snapshot-1.37.5.lock");

		let (missing, identity) = inspect_base_data(Some(GAME_VERSION), &mut Vec::new());
		assert_eq!(missing.state, BaseDataState::Missing);
		assert!(identity.is_none());
		assert!(!lock_path.exists());

		let installed_dir = data_root.join("eu4").join(GAME_VERSION);
		fs::create_dir_all(&installed_dir).expect("create stale install");
		fs::write(installed_dir.join("metadata.json"), b"not json").expect("write stale metadata");
		let before = file_system_snapshot(&data_root);
		let (stale, identity) = inspect_base_data(Some(GAME_VERSION), &mut Vec::new());
		assert_eq!(stale.state, BaseDataState::Stale);
		assert!(identity.is_none());
		assert_eq!(before, file_system_snapshot(&data_root));
		assert!(!lock_path.exists());
	}
}
