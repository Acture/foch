pub mod dependency;
pub mod descriptor;
mod error;
pub mod steam;

pub use error::{ParseError, ParseErrorKind};

use crate::game::eu4::Eu4;
use crate::model::GamePath;
use descriptor::{ModDescriptor, load_descriptor};
use relative_path::RelativePath;
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
	///   `mod/ugc_<steamId>.mod` (positions = array index = precedence). The
	///   launcher writes them relative to the Paradox data directory in `/`
	///   syntax on every host; an entry in any other form, or one that leaves
	///   the data directory, is a format error.
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
			let rel = parse_enabled_mod_entry(path, rel)?;
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

/// Reads an `enabled_mods` entry. The launcher writes each entry relative to
/// the Paradox data directory in `/` syntax on every host. That syntax has the
/// constraints a [`GamePath`] validates (relative, no `.` or `..`, no host path
/// structure), so the same validator is reused and the entry cannot leave the
/// data directory. The entry stays a [`RelativePath`], though: it names a
/// launcher file under the data directory, not a file in the game's namespace.
/// A separator the syntax does not declare, such as `\`, is rejected rather
/// than guessed at.
fn parse_enabled_mod_entry<'a>(
	dlc_load: &Path,
	rel: &'a str,
) -> Result<&'a RelativePath, ParseError> {
	GamePath::new(rel)
		.map(GamePath::as_relative_path)
		.map_err(|error| {
			ParseError::format(
				dlc_load.to_path_buf(),
				format!(
					"enabled mod descriptor path `{rel}` must be normalized and relative: {}",
					error.kind
				),
			)
		})
}

/// The descriptor file an `enabled_mods` entry names, spelled as a host path.
fn enabled_mod_descriptor_path(paradox_data_dir: &Path, rel: &RelativePath) -> PathBuf {
	paradox_data_dir.join(rel.to_path(""))
}

fn read_dlc_load_entry(
	paradox_data_dir: &Path,
	position: usize,
	rel: &RelativePath,
) -> PlaysetEntry {
	let descriptor = load_descriptor(&enabled_mod_descriptor_path(paradox_data_dir, rel)).ok();
	playset_entry_from_descriptor(position, rel, descriptor)
}

fn read_dlc_load_entry_required(
	paradox_data_dir: &Path,
	position: usize,
	rel: &RelativePath,
) -> Result<PlaysetEntry, ParseError> {
	let descriptor = load_descriptor(&enabled_mod_descriptor_path(paradox_data_dir, rel))?;
	Ok(playset_entry_from_descriptor(
		position,
		rel,
		Some(descriptor),
	))
}

fn playset_entry_from_descriptor(
	position: usize,
	rel: &RelativePath,
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

fn extract_steam_id_from_descriptor_path(rel: &RelativePath) -> Option<String> {
	// Convention: dlc_load lists mods as `mod/ugc_<numeric steam id>.mod`;
	// strip the prefix/suffix and validate the inner segment.
	let filename = rel.file_stem()?;
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
	fn both_loaders_reject_entries_outside_the_launcher_slash_syntax() {
		let temp = TempDir::new().unwrap();
		let game_dir = temp.path().join("Europa Universalis IV");
		fs::create_dir_all(game_dir.join("mod")).unwrap();
		fs::write(
			temp.path().join("outside.mod"),
			"name=\"Outside\"\nremote_file_id=\"999\"\n",
		)
		.unwrap();
		// On a Unix host `mod\ugc_999.mod` is one file name, so reading it
		// with host syntax would silently look for a different file than the
		// launcher meant; the declared `/` syntax rejects it instead.
		fs::write(
			game_dir.join("mod").join("ugc_999.mod"),
			"name=\"Inside\"\nremote_file_id=\"999\"\n",
		)
		.unwrap();
		let dlc_load = game_dir.join("dlc_load.json");
		for entry in [
			"../outside.mod",
			"/mod/ugc_999.mod",
			r"mod\ugc_999.mod",
			"mod//ugc_999.mod",
			"./mod/ugc_999.mod",
			"",
		] {
			fs::write(
				&dlc_load,
				serde_json::json!({ "enabled_mods": [entry], "disabled_dlcs": [] }).to_string(),
			)
			.unwrap();
			for error in [
				Playset::from_dlc_load(&dlc_load).expect_err(entry),
				Playset::from_dlc_load_with_required_descriptors(&dlc_load).expect_err(entry),
			] {
				assert_eq!(error.kind, ParseErrorKind::Format, "{entry}");
				assert_eq!(error.path, dlc_load, "{entry}");
				assert!(
					error
						.message
						.contains(&format!("`{entry}` must be normalized and relative")),
					"{entry}: {error}"
				);
				assert!(
					!error.message.contains("game path"),
					"a launcher entry is not a game path: {error}"
				);
			}
		}
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
