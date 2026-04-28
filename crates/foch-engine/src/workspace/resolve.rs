use super::file_filter::FileFilter;
use super::{LoadedModSnapshot, cache::load_or_build_mod_snapshot};
use crate::base_data::{
	InstalledBaseSnapshot, base_game_mod_id, detect_game_version, load_installed_base_snapshot,
	resolve_game_root, resolve_game_root_and_version,
};
use crate::request::CheckRequest;
use foch_core::domain::ParseErrorKind;
use foch_core::domain::descriptor::load_descriptor;
use foch_core::domain::game::Game;
use foch_core::domain::playlist::{Playlist, PlaylistEntry, load_playlist};
use foch_core::model::ModCandidate;
use foch_core::utils::steam::steam_workshop_mod_path;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceResolveErrorKind {
	PlaylistFormat,
	Io,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceResolveError {
	pub kind: WorkspaceResolveErrorKind,
	pub path: PathBuf,
	pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedFileContributor {
	pub mod_id: String,
	pub root_path: PathBuf,
	pub absolute_path: PathBuf,
	pub precedence: usize,
	pub is_base_game: bool,
	pub is_synthetic_base: bool,
	pub parse_ok_hint: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedWorkspace {
	pub playlist_path: PathBuf,
	pub playlist: Playlist,
	pub mods: Vec<ModCandidate>,
	pub installed_base_snapshot: Option<InstalledBaseSnapshot>,
	pub mod_snapshots: Vec<Option<LoadedModSnapshot>>,
	pub file_inventory: BTreeMap<String, Vec<ResolvedFileContributor>>,
}

pub(crate) fn resolve_workspace(
	request: &CheckRequest,
	include_game_base: bool,
) -> Result<ResolvedWorkspace, WorkspaceResolveError> {
	let playlist = load_playlist(&request.playset_path).map_err(|err| WorkspaceResolveError {
		kind: if matches!(err.kind, ParseErrorKind::Format) {
			WorkspaceResolveErrorKind::PlaylistFormat
		} else {
			WorkspaceResolveErrorKind::Io
		},
		path: err.path.clone(),
		message: match err.kind {
			ParseErrorKind::Format => err.message,
			ParseErrorKind::Io => format!("无法读取 Playset: {err}"),
		},
	})?;

	let mods = build_mod_candidates(request, &playlist);
	let optional_game_root = resolve_game_root(&request.config, &playlist.game);
	let (base_game_root, installed_base_snapshot, mod_cache_game_version) = if include_game_base {
		let (game_root, game_version) =
			resolve_game_root_and_version(&request.config, &playlist.game).map_err(|message| {
				WorkspaceResolveError {
					kind: WorkspaceResolveErrorKind::Io,
					path: request.playset_path.clone(),
					message,
				}
			})?;
		let installed = load_installed_base_snapshot(playlist.game.key(), &game_version)
			.map_err(|message| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: request.playset_path.clone(),
				message,
			})?
			.ok_or_else(|| WorkspaceResolveError {
				kind: WorkspaceResolveErrorKind::Io,
				path: request.playset_path.clone(),
				message: missing_base_data_message(&playlist.game, &game_version, &game_root),
			})?;
		(Some(game_root), Some(installed), Some(game_version))
	} else {
		(
			None,
			None,
			optional_game_root
				.as_ref()
				.and_then(|game_root| detect_game_version(game_root)),
		)
	};
	let snapshot_filter =
		match FileFilter::new(playlist.game.clone(), &request.config.extra_ignore_patterns) {
			Ok(filter) => filter,
			Err(message) => {
				tracing::warn!(target: "foch::workspace::resolve", message, "回退到无额外忽略 glob 的过滤器");
				FileFilter::for_game(playlist.game.clone())
			}
		};
	let mod_snapshots: Vec<Option<LoadedModSnapshot>> = mods
		.iter()
		.map(|mod_item| {
			load_or_build_mod_snapshot(
				playlist.game.key(),
				mod_cache_game_version.as_deref(),
				mod_item,
				&snapshot_filter,
			)
		})
		.collect();
	let mut file_inventory = build_file_inventory(
		&playlist,
		&mods,
		&mod_snapshots,
		base_game_root.as_ref(),
		installed_base_snapshot.as_ref(),
	);
	inject_synthetic_bases(&mut file_inventory);

	Ok(ResolvedWorkspace {
		playlist_path: request.playset_path.clone(),
		playlist,
		mods,
		installed_base_snapshot,
		mod_snapshots,
		file_inventory,
	})
}

fn missing_base_data_message(game: &Game, game_version: &str, game_root: &Path) -> String {
	format!(
		"缺少 {} {} 的已安装基础数据；请运行 `foch data install {} --game-version auto` 或 `foch data build {} --from-game-path {} --game-version auto --install`，或使用 --no-game-base",
		game.key(),
		game_version,
		game.key(),
		game.key(),
		game_root.display()
	)
}

pub(crate) fn build_mod_candidates(
	request: &CheckRequest,
	playlist: &Playlist,
) -> Vec<ModCandidate> {
	let playset_dir = request
		.playset_path
		.parent()
		.map_or_else(|| PathBuf::from("."), PathBuf::from);

	let filter = match FileFilter::new(playlist.game.clone(), &request.config.extra_ignore_patterns)
	{
		Ok(filter) => filter,
		Err(message) => {
			tracing::warn!(target: "foch::workspace::resolve", message, "回退到无额外忽略 glob 的过滤器");
			FileFilter::for_game(playlist.game.clone())
		}
	};

	let mut entries = playlist.mods.clone();
	entries.sort_by_key(|entry| entry.position.unwrap_or(usize::MAX));

	entries
		.into_iter()
		.map(|entry| {
			let mod_id = entry
				.steam_id
				.clone()
				.filter(|x| !x.trim().is_empty())
				.unwrap_or_else(|| "<missing-steam-id>".to_string());

			let root_path = resolve_mod_root(&playset_dir, request, playlist, &entry);
			let descriptor_path = root_path.as_ref().map(|path| path.join("descriptor.mod"));

			let (descriptor, descriptor_error) = match descriptor_path.as_ref() {
				Some(path) if path.exists() => match load_descriptor(path) {
					Ok(descriptor) => (Some(descriptor), None),
					Err(err) => (None, Some(err.to_string())),
				},
				Some(path) => (None, Some(format!("{} 不存在", path.display()))),
				None => (None, None),
			};

			let files = root_path
				.as_ref()
				.map_or_else(Vec::new, |root| collect_relative_files(root, &filter));

			ModCandidate {
				entry,
				mod_id,
				root_path,
				descriptor_path,
				descriptor,
				descriptor_error,
				files,
			}
		})
		.collect()
}

fn resolve_mod_root(
	playset_dir: &Path,
	request: &CheckRequest,
	playlist: &Playlist,
	entry: &PlaylistEntry,
) -> Option<PathBuf> {
	let mut candidates = Vec::new();

	if let Some(steam_id) = entry.steam_id.as_ref() {
		candidates.push(playset_dir.join(steam_id));
		candidates.push(playset_dir.join(format!("mod_{steam_id}")));

		if let Some(path) = request.config.paradox_data_path.as_ref() {
			for game_data_dir in paradox_game_data_dirs(path, &playlist.game) {
				if let Some(root) = resolve_mod_from_ugc_descriptor(&game_data_dir, steam_id) {
					candidates.push(root);
				}
				candidates.push(game_data_dir.join("mod").join(steam_id));
				candidates.push(game_data_dir.join("mod").join(format!("ugc_{steam_id}")));
			}
		}

		if let Some(steam_root) = request.config.steam_root_path.as_ref() {
			for app_id in playlist.game.steam_app_ids() {
				if let Some(path) = steam_workshop_mod_path(steam_root, *app_id, steam_id) {
					candidates.push(path);
				}
				candidates.push(
					steam_root
						.join("steamapps")
						.join("workshop")
						.join("content")
						.join(app_id.to_string())
						.join(steam_id),
				);
			}
		}
	}

	if let Some(name) = entry.display_name.as_ref() {
		candidates.push(playset_dir.join(name));
		candidates.push(playset_dir.join(name.replace(' ', "_")));
	}

	dedup_candidates(candidates)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

fn dedup_candidates(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut seen = HashSet::new();
	let mut result = Vec::new();
	for candidate in candidates {
		let key = candidate.to_string_lossy().replace('\\', "/");
		if !seen.insert(key) {
			continue;
		}
		result.push(candidate);
	}
	result
}

fn paradox_game_data_dirs(base: &Path, game: &Game) -> Vec<PathBuf> {
	let mut dirs = vec![base.to_path_buf()];
	if let Some(game_dir_name) = game.paradox_data_dir_name() {
		dirs.push(base.join(game_dir_name));
	}
	dedup_candidates(dirs)
}

fn resolve_mod_from_ugc_descriptor(game_data_dir: &Path, steam_id: &str) -> Option<PathBuf> {
	let metadata = game_data_dir
		.join("mod")
		.join(format!("ugc_{steam_id}.mod"));
	if !metadata.is_file() {
		return None;
	}

	let descriptor = load_descriptor(&metadata).ok()?;
	let raw_path = descriptor.path?;
	descriptor_path_candidates(game_data_dir, &raw_path)
		.into_iter()
		.find(|candidate| candidate.is_dir())
}

fn descriptor_path_candidates(game_data_dir: &Path, raw: &str) -> Vec<PathBuf> {
	let mut fragments = vec![raw.to_string()];
	if raw.contains('\\') {
		fragments.push(raw.replace('\\', "/"));
	}
	if raw.contains('/') {
		fragments.push(raw.replace('/', "\\"));
	}

	let mut candidates = Vec::new();
	for fragment in fragments {
		let path = PathBuf::from(&fragment);
		if path.is_absolute() {
			candidates.push(path.clone());
		}
		candidates.push(game_data_dir.join(&path));
		candidates.push(game_data_dir.join("mod").join(&path));
	}

	dedup_candidates(candidates)
}

pub(crate) fn collect_relative_files(root: &Path, filter: &FileFilter) -> Vec<PathBuf> {
	let mut files = Vec::new();

	for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
		if !entry.file_type().is_file() {
			continue;
		}

		let path = entry.path();
		if path.file_name() == Some(OsStr::new("descriptor.mod")) {
			continue;
		}

		if let Ok(relative) = path.strip_prefix(root) {
			if !filter.accepts(relative) {
				continue;
			}
			files.push(relative.to_path_buf());
		}
	}

	files.sort();
	files
}

pub(crate) fn build_file_inventory(
	playlist: &Playlist,
	mods: &[ModCandidate],
	mod_snapshots: &[Option<LoadedModSnapshot>],
	base_game_root: Option<&PathBuf>,
	installed_base_snapshot: Option<&InstalledBaseSnapshot>,
) -> BTreeMap<String, Vec<ResolvedFileContributor>> {
	let mut inventory = BTreeMap::new();
	let mut precedence = 0;

	if let (Some(root), Some(snapshot)) = (base_game_root, installed_base_snapshot) {
		let mod_id = base_game_mod_id(playlist.game.key());
		let document_lookup = snapshot.snapshot.document_lookup();
		for relative in &snapshot.snapshot.inventory_paths {
			let document = document_lookup.get(relative.as_str());
			inventory
				.entry(relative.clone())
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(relative),
					precedence,
					is_base_game: true,
					is_synthetic_base: false,
					parse_ok_hint: document.map(|(_family, parse_ok)| *parse_ok),
				});
		}
		precedence += 1;
	}

	for (idx, mod_item) in mods.iter().enumerate() {
		let Some(root) = mod_item.root_path.as_ref() else {
			continue;
		};
		let parse_hints = mod_snapshots
			.get(idx)
			.and_then(|snapshot| snapshot.as_ref())
			.map(|snapshot| &snapshot.document_parse_hints);
		for relative in &mod_item.files {
			let key = normalize_relative_path(relative);
			let parse_ok_hint = parse_hints.and_then(|hints| hints.get(&key).copied());
			inventory
				.entry(key)
				.or_insert_with(Vec::new)
				.push(ResolvedFileContributor {
					mod_id: mod_item.mod_id.clone(),
					root_path: root.clone(),
					absolute_path: root.join(relative),
					precedence,
					is_base_game: false,
					is_synthetic_base: false,
					parse_ok_hint,
				});
		}
		precedence += 1;
	}

	inventory
}

pub(crate) fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

/// 当 file_inventory 中某个文件没有 base game 贡献者，但有 ≥2 个 mod 贡献者时，
/// 选取 precedence 最小的 mod（tie 用 mod_id 字典序）clone 一份作为合成 base，
/// 插入到 contributors 最前面。这样下游的 patch 引擎可以把所有 mod 视为对该 base 的 patch。
///
/// 合成 base 的特征：`is_synthetic_base = true`，`is_base_game = false`，`precedence = 0`。
/// 原贡献者保持不动。
pub(crate) fn inject_synthetic_bases(
	file_inventory: &mut BTreeMap<String, Vec<ResolvedFileContributor>>,
) {
	for contributors in file_inventory.values_mut() {
		if contributors.iter().any(|c| c.is_base_game) {
			continue;
		}
		let non_base_count = contributors.iter().filter(|c| !c.is_base_game).count();
		if non_base_count < 2 {
			continue;
		}
		let Some(seed) = contributors
			.iter()
			.filter(|c| !c.is_base_game)
			.min_by(|a, b| {
				a.precedence
					.cmp(&b.precedence)
					.then_with(|| a.mod_id.cmp(&b.mod_id))
			})
		else {
			continue;
		};
		let mut synthetic = seed.clone();
		synthetic.is_synthetic_base = true;
		synthetic.is_base_game = false;
		synthetic.precedence = 0;
		contributors.insert(0, synthetic);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn make_contributor(
		mod_id: &str,
		precedence: usize,
		is_base_game: bool,
	) -> ResolvedFileContributor {
		ResolvedFileContributor {
			mod_id: mod_id.to_string(),
			root_path: PathBuf::from(format!("/mods/{mod_id}")),
			absolute_path: PathBuf::from(format!("/mods/{mod_id}/file.txt")),
			precedence,
			is_base_game,
			is_synthetic_base: false,
			parse_ok_hint: None,
		}
	}

	#[test]
	fn inject_synthetic_bases_no_base_two_mods_creates_synthetic() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("mod_b", 5, false),
				make_contributor("mod_a", 3, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3, "synthetic base should be added");
		let synth = &contribs[0];
		assert!(synth.is_synthetic_base);
		assert!(!synth.is_base_game);
		assert_eq!(synth.precedence, 0);
		assert_eq!(synth.mod_id, "mod_a", "lowest-precedence mod is mod_a (3)");
		// originals preserved
		assert!(!contribs[1].is_synthetic_base);
		assert!(!contribs[2].is_synthetic_base);
		assert_eq!(contribs[1].mod_id, "mod_b");
		assert_eq!(contribs[2].mod_id, "mod_a");
		assert_eq!(contribs[2].precedence, 3);
	}

	#[test]
	fn inject_synthetic_bases_with_real_base_skipped() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("base:eu4", 0, true),
				make_contributor("mod_a", 1, false),
				make_contributor("mod_b", 2, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3, "no synthetic base should be added");
		assert!(!contribs.iter().any(|c| c.is_synthetic_base));
	}

	#[test]
	fn inject_synthetic_bases_single_mod_skipped() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![make_contributor("mod_a", 1, false)],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 1, "no synthetic base for single mod");
		assert!(!contribs[0].is_synthetic_base);
	}

	#[test]
	fn inject_synthetic_bases_tie_breaks_on_mod_id() {
		let mut inventory: BTreeMap<String, Vec<ResolvedFileContributor>> = BTreeMap::new();
		inventory.insert(
			"common/file.txt".to_string(),
			vec![
				make_contributor("mod_z", 2, false),
				make_contributor("mod_a", 2, false),
			],
		);

		inject_synthetic_bases(&mut inventory);

		let contribs = &inventory["common/file.txt"];
		assert_eq!(contribs.len(), 3);
		assert!(contribs[0].is_synthetic_base);
		assert_eq!(
			contribs[0].mod_id, "mod_a",
			"tie on precedence resolved by mod_id lex"
		);
	}
}
