pub mod dependency;
pub mod descriptor;
mod error;
pub mod steam;

pub use error::{ParseError, ParseErrorKind};

use crate::game::eu4::Eu4;
use descriptor::{ModDescriptor, load_descriptor};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use steam::WorkshopInstallIdentity;

/// In-memory representation of an EU4 playset.
///
/// The on-disk format is the launcher's `dlc_load.json` plus the `mod/`
/// directory of `.mod` descriptors next to it; this struct is the parsed
/// projection used everywhere downstream. It is **not** itself a serializable
/// JSON shape — the launcher owns the canonical format and `foch` consumes it
/// via [`Playset::from_dlc_load`].
#[derive(Debug, Clone, Default)]
pub struct Playset {
	pub game: Eu4,
	pub name: String,
	pub mods: Vec<PlaysetEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaysetEntry {
	pub id: Option<String>,
	pub display_name: Option<String>,
	pub enabled: bool,
	pub position: Option<usize>,
	pub steam_id: Option<String>,
	pub workshop_identity: Option<WorkshopInstallIdentity>,
	pub root_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct DlcLoad {
	#[serde(default)]
	enabled_mods: Vec<String>,
	#[serde(default)]
	#[allow(dead_code)] // surfaced for completeness; foch ignores DLC selection.
	disabled_dlcs: Vec<String>,
}

impl Playset {
	/// Parse the launcher's `dlc_load.json` plus the sibling `mod/` directory
	/// of `.mod` descriptors into an in-memory [`Playset`].
	///
	/// Conventions:
	/// - `dlc_load.json`'s `enabled_mods` is an ordered list of paths like
	///   `mod/ugc_<steamId>.mod` (positions = array index = precedence).
	/// - Each referenced descriptor is read for its `name` (→ display_name)
	///   and `remote_file_id` (→ steam_id, falling back to the filename's
	///   numeric tail when the descriptor omits it).
	/// - EU4 is the only supported game, so the parsed playset carries the
	///   concrete [`Eu4`] identity.
	pub fn from_dlc_load(path: &Path) -> Result<Self, ParseError> {
		Self::from_dlc_load_impl(path, false)
	}

	pub(crate) fn from_dlc_load_with_required_descriptors(path: &Path) -> Result<Self, ParseError> {
		Self::from_dlc_load_impl(path, true)
	}

	fn from_dlc_load_impl(path: &Path, require_descriptors: bool) -> Result<Self, ParseError> {
		let bytes = std::fs::read(path).map_err(|err| ParseError::io(path.to_path_buf(), err))?;
		let dlc: DlcLoad = serde_json::from_slice(&bytes)
			.map_err(|err| ParseError::format(path.to_path_buf(), err.to_string()))?;
		let parent = path
			.parent()
			.ok_or_else(|| {
				ParseError::format(
					path.to_path_buf(),
					"dlc_load.json must live inside a paradox game data directory".to_string(),
				)
			})?
			.to_path_buf();
		let game = Eu4;
		let mut mods = Vec::with_capacity(dlc.enabled_mods.len());
		for (position, rel) in dlc.enabled_mods.iter().enumerate() {
			let entry = if require_descriptors {
				read_dlc_load_entry_required(&parent, position, rel)?
			} else {
				read_dlc_load_entry(&parent, position, rel)
			};
			mods.push(entry);
		}
		let name = match path.file_stem().and_then(|s| s.to_str()) {
			Some(stem) if !stem.is_empty() => format!("{stem} (active)"),
			_ => "active".to_string(),
		};
		Ok(Playset { game, name, mods })
	}
}

fn read_dlc_load_entry(paradox_data_dir: &Path, position: usize, rel: &str) -> PlaysetEntry {
	let descriptor_path = paradox_data_dir.join(rel);
	let descriptor = load_descriptor(&descriptor_path).ok();
	playset_entry_from_descriptor(position, rel, descriptor)
}

fn read_dlc_load_entry_required(
	paradox_data_dir: &Path,
	position: usize,
	rel: &str,
) -> Result<PlaysetEntry, ParseError> {
	let relative_path = Path::new(rel);
	if relative_path.is_absolute()
		|| relative_path
			.components()
			.any(|component| !matches!(component, std::path::Component::Normal(_)))
	{
		return Err(ParseError::format(
			paradox_data_dir.join(relative_path),
			"enabled mod descriptor path must be normalized and relative".to_string(),
		));
	}
	let descriptor_path = paradox_data_dir.join(relative_path);
	let descriptor = load_descriptor(&descriptor_path)?;
	Ok(playset_entry_from_descriptor(
		position,
		rel,
		Some(descriptor),
	))
}

fn playset_entry_from_descriptor(
	position: usize,
	rel: &str,
	descriptor: Option<ModDescriptor>,
) -> PlaysetEntry {
	let steam_id = descriptor
		.as_ref()
		.and_then(|d| d.remote_file_id.clone())
		.or_else(|| extract_steam_id_from_descriptor_path(rel));
	let display_name = descriptor
		.as_ref()
		.and_then(|d| (!d.name.trim().is_empty()).then(|| d.name.clone()))
		.or_else(|| steam_id.as_ref().map(|id| format!("ugc_{id}")));
	PlaysetEntry {
		id: None,
		display_name,
		enabled: true,
		position: Some(position),
		steam_id,
		workshop_identity: None,
		root_path: None,
	}
}

fn extract_steam_id_from_descriptor_path(rel: &str) -> Option<String> {
	// Convention: dlc_load lists mods as `mod/ugc_<numeric steam id>.mod`;
	// strip the prefix/suffix and validate the inner segment.
	let filename = Path::new(rel).file_stem().and_then(|s| s.to_str())?;
	let stripped = filename.strip_prefix("ugc_")?;
	if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
		Some(stripped.to_string())
	} else {
		None
	}
}

/// Default location of a launcher `dlc_load.json` for a paradox data
/// directory configured via `Config::paradox_data_path`.
pub fn default_dlc_load_path(paradox_data_dir: &Path) -> PathBuf {
	paradox_data_dir.join("dlc_load.json")
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	fn write_dlc_load(dir: &Path, mods: &[(&str, &str)]) {
		fs::create_dir_all(dir.join("mod")).unwrap();
		let entries: Vec<String> = mods
			.iter()
			.map(|(steam_id, _)| format!("mod/ugc_{steam_id}.mod"))
			.collect();
		let payload =
			serde_json::json!({ "enabled_mods": entries, "disabled_dlcs": Vec::<String>::new() });
		fs::write(
			dir.join("dlc_load.json"),
			serde_json::to_string_pretty(&payload).unwrap(),
		)
		.unwrap();
		for (steam_id, name) in mods {
			let body = format!(
				"name=\"{name}\"\npath=\"/tmp/mod_{steam_id}\"\nremote_file_id=\"{steam_id}\"\n"
			);
			fs::write(dir.join("mod").join(format!("ugc_{steam_id}.mod")), body).unwrap();
		}
	}

	#[test]
	fn parses_dlc_load_with_descriptors() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(&game_dir).unwrap();
		write_dlc_load(
			&game_dir,
			&[("2164202838", "Europa Expanded"), ("1999055990", "汉化")],
		);

		let playlist = Playset::from_dlc_load(&game_dir.join("dlc_load.json")).unwrap();
		assert_eq!(playlist.game, Eu4);
		assert_eq!(playlist.mods.len(), 2);
		assert_eq!(playlist.mods[0].steam_id.as_deref(), Some("2164202838"));
		assert_eq!(
			playlist.mods[0].display_name.as_deref(),
			Some("Europa Expanded")
		);
		assert_eq!(playlist.mods[0].position, Some(0));
		assert!(playlist.mods[0].enabled);
		assert_eq!(playlist.mods[1].steam_id.as_deref(), Some("1999055990"));
		assert_eq!(playlist.mods[1].display_name.as_deref(), Some("汉化"));
		assert_eq!(playlist.mods[1].position, Some(1));
	}

	#[test]
	fn falls_back_to_filename_when_descriptor_missing() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(&game_dir).unwrap();
		fs::write(
			game_dir.join("dlc_load.json"),
			r#"{"enabled_mods":["mod/ugc_999.mod"],"disabled_dlcs":[]}"#,
		)
		.unwrap();

		let playlist = Playset::from_dlc_load(&game_dir.join("dlc_load.json")).unwrap();
		assert_eq!(playlist.mods.len(), 1);
		assert_eq!(playlist.mods[0].steam_id.as_deref(), Some("999"));
		assert_eq!(playlist.mods[0].display_name.as_deref(), Some("ugc_999"));
		let strict_error =
			Playset::from_dlc_load_with_required_descriptors(&game_dir.join("dlc_load.json"))
				.expect_err("current-input inspection must require the sibling descriptor");
		assert!(strict_error.path.ends_with("mod/ugc_999.mod"));
	}

	#[test]
	fn strict_loader_rejects_descriptor_path_escape() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(&game_dir).unwrap();
		fs::write(
			temp.path().join("outside.mod"),
			"name=\"Outside\"\nremote_file_id=\"999\"\n",
		)
		.unwrap();
		fs::write(
			game_dir.join("dlc_load.json"),
			r#"{"enabled_mods":["../outside.mod"],"disabled_dlcs":[]}"#,
		)
		.unwrap();

		let error =
			Playset::from_dlc_load_with_required_descriptors(&game_dir.join("dlc_load.json"))
				.expect_err("strict loader must reject paths escaping the game data directory");
		assert!(
			error
				.to_string()
				.contains("must be normalized and relative")
		);
	}

	#[test]
	fn unknown_paradox_data_dir_defaults_to_eu4() {
		let temp = TempDir::new().unwrap();
		let path = temp.path().join("dlc_load.json");
		fs::write(&path, r#"{"enabled_mods":[],"disabled_dlcs":[]}"#).unwrap();
		let playlist = Playset::from_dlc_load(&path).unwrap();
		assert_eq!(playlist.game, Eu4);
	}
}
