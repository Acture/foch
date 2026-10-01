use crate::game::eu4::base::snapshot::InstalledBaseSnapshot;
use crate::game::eu4::script::{ParsedScriptFile, parse_script_bytes_cached};
use crate::model::{GamePath, GamePathBuf, ModCandidate};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::LoadedModSnapshot;

type ScriptCacheKey = (String, GamePathBuf);

#[derive(Clone, Debug)]
struct ScriptOverlay {
	parsed: Arc<ParsedScriptFile>,
	bytes: Arc<[u8]>,
}

#[derive(Debug)]
struct LazyScriptFile {
	mod_id: String,
	root_path: PathBuf,
	absolute_path: PathBuf,
	relative_path: GamePathBuf,
	expected_size_bytes: u64,
	expected_content_digest: String,
	expected_parse_ok: Option<bool>,
	parsed: OnceLock<Result<Arc<ParsedScriptFile>, String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InputScriptCache {
	/// Analysis-local adaptations. Persisted source snapshots remain unchanged.
	overlays: Arc<HashMap<ScriptCacheKey, ScriptOverlay>>,
	loaded: Arc<HashMap<ScriptCacheKey, Arc<ParsedScriptFile>>>,
	lazy: Arc<HashMap<ScriptCacheKey, Arc<LazyScriptFile>>>,
	noop_hints: Arc<HashMap<ScriptCacheKey, bool>>,
}

impl InputScriptCache {
	pub(crate) fn from_parts(
		mods: &[ModCandidate],
		mod_snapshots: &[Option<LoadedModSnapshot>],
		installed_base_snapshot: Option<&InstalledBaseSnapshot>,
		base_game_root: Option<&Path>,
	) -> Result<Self, String> {
		let mut loaded = HashMap::new();
		if let (Some(installed), Some(root)) = (installed_base_snapshot, base_game_root) {
			match installed
				.snapshot
				.parsed_script_files(root)
				.and_then(index_base_scripts)
			{
				Ok(files) => loaded = files,
				Err(err) => {
					tracing::warn!(
						target: "crate::input::scripts",
						error = %err,
						"failed to decode base parsed script cache; merge planning requires a rebuilt base snapshot"
					);
				}
			}
		}

		let mut noop_hints = HashMap::new();
		for (mod_item, snapshot) in mods.iter().zip(mod_snapshots.iter()) {
			if let Some(snapshot) = snapshot {
				for (path, is_noop) in &snapshot.document_noop_hints {
					noop_hints.insert((mod_item.mod_id.clone(), path.clone()), *is_noop);
				}
			} else {
				tracing::debug!(
					target: "crate::input::scripts",
					mod_id = %mod_item.mod_id,
					"no parsed script snapshot for mod"
				);
			}
		}

		let mut lazy = HashMap::new();
		for (mod_item, snapshot) in mods.iter().zip(mod_snapshots.iter()) {
			let Some(snapshot) = snapshot.as_ref() else {
				continue;
			};
			let Some(root_path) = mod_item.root_path.as_ref() else {
				continue;
			};
			for (relative_path, identity) in &snapshot.document_input_identities {
				let key = (mod_item.mod_id.clone(), relative_path.clone());
				let expected_parse_ok = snapshot.document_parse_hints.get(relative_path).copied();
				let entry = Arc::new(LazyScriptFile {
					mod_id: mod_item.mod_id.clone(),
					root_path: root_path.clone(),
					absolute_path: relative_path.to_path(root_path),
					relative_path: relative_path.clone(),
					expected_size_bytes: identity.size_bytes,
					expected_content_digest: identity.content_digest.clone(),
					expected_parse_ok,
					parsed: OnceLock::new(),
				});
				if lazy.insert(key.clone(), entry).is_some() {
					return Err(format!(
						"duplicate semantic snapshot input for {}:{}",
						key.0, key.1
					));
				}
			}
		}

		Ok(Self {
			overlays: Arc::default(),
			loaded: Arc::new(loaded),
			lazy: Arc::new(lazy),
			noop_hints: Arc::new(noop_hints),
		})
	}

	pub(crate) fn get(
		&self,
		mod_id: &str,
		relative_path: &GamePath,
	) -> Result<Option<Arc<ParsedScriptFile>>, String> {
		let key = (mod_id.to_string(), relative_path.to_owned());
		if let Some(overlay) = self.overlays.get(&key) {
			return Ok(Some(overlay.parsed.clone()));
		}
		if let Some(parsed) = self.loaded.get(&key) {
			return Ok(Some(parsed.clone()));
		}
		let Some(entry) = self.lazy.get(&key) else {
			return Ok(None);
		};
		match entry.parsed.get() {
			Some(Ok(parsed)) => Ok(Some(parsed.clone())),
			Some(Err(error)) => Err(error.clone()),
			None => Ok(None),
		}
	}

	#[cfg(test)]
	pub(crate) fn is_loaded(&self, mod_id: &str, relative_path: &GamePath) -> bool {
		matches!(self.get(mod_id, relative_path), Ok(Some(_)))
	}

	pub(crate) fn is_noop_hint(&self, mod_id: &str, relative_path: &GamePath) -> Option<bool> {
		if self.overlay_bytes(mod_id, relative_path).is_some() {
			return Some(false);
		}
		self.noop_hints
			.get(&(mod_id.to_string(), relative_path.to_owned()))
			.copied()
	}

	pub(crate) fn insert_overlay(&mut self, mut parsed: ParsedScriptFile, bytes: Vec<u8>) {
		let key = (parsed.mod_id.clone(), parsed.relative_path.clone());
		parsed.source.clear();
		Arc::make_mut(&mut self.overlays).insert(
			key,
			ScriptOverlay {
				parsed: Arc::new(parsed),
				bytes: bytes.into(),
			},
		);
	}

	pub(crate) fn overlay_bytes(&self, mod_id: &str, relative_path: &GamePath) -> Option<&[u8]> {
		self.overlays
			.get(&(mod_id.to_owned(), relative_path.to_owned()))
			.map(|overlay| overlay.bytes.as_ref())
	}

	pub(crate) fn overlay_parse_ok(&self, mod_id: &str, relative_path: &GamePath) -> Option<bool> {
		self.overlays
			.get(&(mod_id.to_owned(), relative_path.to_owned()))
			.map(|overlay| overlay.parsed.parse_issues.is_empty())
	}

	pub(crate) fn has_overlay_for_path(&self, relative_path: &GamePath) -> bool {
		self.overlays
			.keys()
			.any(|(_, candidate)| candidate == relative_path)
	}

	pub(crate) fn has_overlay_in_directory(&self, directory: &GamePath) -> bool {
		self.overlays
			.keys()
			.any(|(_, path)| path.parent() == Some(directory))
	}

	pub(crate) fn documents_for_mods(
		&self,
		enabled_mod_ids: &HashSet<String>,
		base_mod_id: Option<&str>,
	) -> Result<Vec<Arc<ParsedScriptFile>>, String> {
		let mut documents = self
			.loaded
			.values()
			.filter(|document| {
				enabled_mod_ids.contains(&document.mod_id)
					|| base_mod_id.is_some_and(|base| document.mod_id == base)
			})
			.cloned()
			.collect::<Vec<_>>();
		for (key, entry) in self.lazy.iter() {
			if self.overlays.contains_key(key) {
				continue;
			}
			if !(enabled_mod_ids.contains(&key.0) || base_mod_id.is_some_and(|base| key.0 == base))
			{
				continue;
			}
			match entry.parsed.get() {
				Some(Ok(parsed)) => documents.push(parsed.clone()),
				Some(Err(error)) => return Err(error.clone()),
				None => {}
			}
		}
		documents.retain(|document| {
			!self
				.overlays
				.contains_key(&(document.mod_id.clone(), document.relative_path.clone()))
		});
		documents.extend(
			self.overlays
				.values()
				.filter(|overlay| {
					enabled_mod_ids.contains(&overlay.parsed.mod_id)
						|| base_mod_id.is_some_and(|base| overlay.parsed.mod_id == base)
				})
				.map(|overlay| overlay.parsed.clone()),
		);
		documents.sort_by(|lhs, rhs| {
			(lhs.mod_id.as_str(), &lhs.relative_path)
				.cmp(&(rhs.mod_id.as_str(), &rhs.relative_path))
		});
		Ok(documents)
	}

	pub(crate) fn load(
		&self,
		contributor: &super::ResolvedInputContributor,
	) -> Result<Arc<ParsedScriptFile>, String> {
		if let Some(parsed) = self.get(&contributor.mod_id, &contributor.relative_path)? {
			return Ok(parsed);
		}
		let key = (
			contributor.mod_id.clone(),
			contributor.relative_path.clone(),
		);
		let entry = self
			.lazy
			.get(&key)
			.ok_or_else(|| format!("no semantic-snapshot input for {}:{}", key.0, key.1))?;
		entry.validate_contributor(contributor)?;
		entry.parsed.get_or_init(|| entry.load_verified()).clone()
	}
}

impl LazyScriptFile {
	fn validate_contributor(
		&self,
		contributor: &super::ResolvedInputContributor,
	) -> Result<(), String> {
		if contributor.mod_id != self.mod_id
			|| contributor.root_path != self.root_path
			|| contributor.relative_path != self.relative_path
		{
			return Err(format!(
				"lazy AST contributor identity does not match semantic snapshot for {}:{}",
				self.mod_id, self.relative_path
			));
		}
		if contributor.parse_ok_hint != self.expected_parse_ok {
			return Err(format!(
				"lazy AST parse-status hint changed for {}:{}: snapshot={:?}, contributor={:?}",
				self.mod_id, self.relative_path, self.expected_parse_ok, contributor.parse_ok_hint
			));
		}
		if self.expected_parse_ok.is_none() {
			return Err(format!(
				"lazy AST has no semantic parse-status hint for {}:{}",
				self.mod_id, self.relative_path
			));
		}
		Ok(())
	}

	fn load_verified(&self) -> Result<Arc<ParsedScriptFile>, String> {
		let bytes = std::fs::read(&self.absolute_path).map_err(|error| {
			format!(
				"failed to read snapshot-bound script {}:{}: {error}",
				self.mod_id, self.relative_path
			)
		})?;
		if bytes.len() as u64 != self.expected_size_bytes {
			return Err(format!(
				"snapshot-bound script size changed for {}:{}: expected {}, observed {}",
				self.mod_id,
				self.relative_path,
				self.expected_size_bytes,
				bytes.len()
			));
		}
		let observed_digest = blake3::hash(&bytes).to_hex().to_string();
		if observed_digest != self.expected_content_digest {
			return Err(format!(
				"snapshot-bound script digest changed for {}:{}: expected {}, observed {}",
				self.mod_id, self.relative_path, self.expected_content_digest, observed_digest
			));
		}
		let parsed =
			parse_script_bytes_cached(&self.mod_id, &self.root_path, &self.relative_path, &bytes);
		let observed_parse_ok = parsed.parse_issues.is_empty();
		if Some(observed_parse_ok) != self.expected_parse_ok {
			return Err(format!(
				"snapshot-bound script parse status changed for {}:{}: expected {:?}, observed {observed_parse_ok}",
				self.mod_id, self.relative_path, self.expected_parse_ok
			));
		}
		Ok(Arc::new(parsed))
	}
}

/// Keys decoded base scripts by game path. Two documents sharing one can only
/// come from a corrupt section, so that is reported like a section that fails
/// to decode rather than letting one document stand in for another.
fn index_base_scripts(
	documents: Vec<ParsedScriptFile>,
) -> Result<HashMap<ScriptCacheKey, Arc<ParsedScriptFile>>, String> {
	let mut files = HashMap::with_capacity(documents.len());
	for mut document in documents {
		document.source.clear();
		match files.entry((document.mod_id.clone(), document.relative_path.clone())) {
			Entry::Occupied(entry) => {
				let (mod_id, relative_path) = entry.key();
				return Err(format!(
					"base parsed scripts list {mod_id}:{relative_path} more than once"
				));
			}
			Entry::Vacant(entry) => {
				entry.insert(Arc::new(document));
			}
		}
	}
	Ok(files)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::input::mod_snapshot::CachedDocumentInputIdentity;
	use crate::model::{ParseFamilyStats, SemanticIndex};
	use crate::playset::PlaysetEntry;
	use std::fs;
	use tempfile::TempDir;

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn contributor(root: &Path, relative: &str) -> super::super::ResolvedInputContributor {
		super::super::ResolvedInputContributor {
			mod_id: "mod-a".to_string(),
			root_path: root.to_path_buf(),
			relative_path: game_path(relative),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some("hash-a".to_string()),
		}
	}

	fn cache_for_files(
		root: &Path,
		files: &[&str],
		parse_ok: bool,
		noop_hints: &[(&str, bool)],
	) -> InputScriptCache {
		let document_input_identities = files
			.iter()
			.map(|relative| {
				let bytes = fs::read(root.join(relative)).expect("read semantic input");
				(
					game_path(relative),
					CachedDocumentInputIdentity {
						size_bytes: bytes.len() as u64,
						content_digest: blake3::hash(&bytes).to_hex().to_string(),
					},
				)
			})
			.collect();
		let mod_item = ModCandidate {
			entry: PlaysetEntry {
				enabled: true,
				position: Some(0),
				steam_id: Some("1".to_string()),
				..PlaysetEntry::default()
			},
			mod_id: "mod-a".to_string(),
			root_path: Some(root.to_path_buf()),
			descriptor_path: None,
			descriptor: None,
			workshop_identity: None,
			descriptor_error: None,
			files: files.iter().copied().map(game_path).collect(),
		};
		let snapshot = LoadedModSnapshot {
			semantic_index: SemanticIndex::default(),
			inventory_paths: files.iter().copied().map(game_path).collect(),
			mod_hash: Some("hash-a".to_string()),
			parsed_files: files.len(),
			parse_error_count: 0,
			parse_stats: ParseFamilyStats::default(),
			clausewitz_parse_cache_hits: 0,
			clausewitz_parse_cache_misses: 0,
			document_parse_hints: files
				.iter()
				.map(|relative| (game_path(relative), parse_ok))
				.collect(),
			document_noop_hints: noop_hints
				.iter()
				.map(|(relative, hint)| (game_path(relative), *hint))
				.collect(),
			document_input_identities,
			cache_hit: true,
		};
		InputScriptCache::from_parts(&[mod_item], &[Some(snapshot)], None, None)
			.expect("build script cache")
	}

	#[test]
	fn lazy_script_cache_loads_only_requested_files_and_reuses_arc() {
		let temp = TempDir::new().expect("temp dir");
		let relative_a = "common/scripted_effects/a.txt";
		let relative_b = "common/scripted_effects/b.txt";
		fs::create_dir_all(temp.path().join("common/scripted_effects"))
			.expect("create scripts dir");
		fs::write(
			temp.path().join(relative_a),
			"effect_a = { add_prestige = 1 }\n",
		)
		.expect("write A");
		fs::write(
			temp.path().join(relative_b),
			"effect_b = { add_prestige = 2 }\n",
		)
		.expect("write B");
		let contributor_a = contributor(temp.path(), relative_a);
		let cache = cache_for_files(temp.path(), &[relative_a, relative_b], true, &[]);

		let concurrent_loads = (0..8)
			.map(|_| {
				let cache = cache.clone();
				let contributor = contributor_a.clone();
				std::thread::spawn(move || cache.load(&contributor).expect("load A"))
			})
			.collect::<Vec<_>>()
			.into_iter()
			.map(|handle| handle.join().expect("join lazy load"))
			.collect::<Vec<_>>();
		let first = concurrent_loads.first().expect("first lazy load").clone();
		assert!(
			concurrent_loads
				.iter()
				.all(|parsed| Arc::ptr_eq(&first, parsed)),
			"concurrent callers must share one parsed AST"
		);
		let second = cache.load(&contributor_a).expect("reuse A");

		assert!(Arc::ptr_eq(&first, &second));
		assert!(
			first.source.is_empty(),
			"raw source is not retained in memory"
		);
		assert!(
			cache
				.get("mod-a", &game_path(relative_b))
				.expect("query B")
				.is_none(),
			"an unrelated file must remain unloaded"
		);
	}

	#[test]
	fn lazy_script_cache_rejects_mutation_and_caches_the_failure() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/scripted_effects/a.txt";
		fs::create_dir_all(temp.path().join("common/scripted_effects"))
			.expect("create scripts dir");
		fs::write(temp.path().join(relative), "effect = { value = 1 }\n").expect("write source");
		let cache = cache_for_files(temp.path(), &[relative], true, &[]);
		let contributor = contributor(temp.path(), relative);
		fs::write(temp.path().join(relative), "effect = { value = 2 }\n").expect("mutate source");

		let first = cache.load(&contributor).expect_err("digest mismatch");
		fs::write(temp.path().join(relative), "effect = { value = 1 }\n").expect("restore source");
		let second = cache.load(&contributor).expect_err("cached failure");

		assert!(first.contains("digest changed"));
		assert_eq!(second, first);
	}

	#[test]
	fn lazy_script_cache_rejects_deleted_input() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/scripted_effects/a.txt";
		fs::create_dir_all(temp.path().join("common/scripted_effects"))
			.expect("create scripts dir");
		fs::write(temp.path().join(relative), "effect = { value = 1 }\n").expect("write source");
		let cache = cache_for_files(temp.path(), &[relative], true, &[]);
		fs::remove_file(temp.path().join(relative)).expect("delete source");

		let error = cache
			.load(&contributor(temp.path(), relative))
			.expect_err("deleted input");

		assert!(error.contains("failed to read snapshot-bound script"));
	}

	#[test]
	fn lazy_script_cache_rejects_parse_status_mismatch() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/scripted_effects/a.lua";
		fs::create_dir_all(temp.path().join("common/scripted_effects"))
			.expect("create scripts dir");
		fs::write(temp.path().join(relative), "--[[ no end\n").expect("write invalid source");
		let cache = cache_for_files(temp.path(), &[relative], true, &[]);

		let error = cache
			.load(&contributor(temp.path(), relative))
			.expect_err("parse-status mismatch");

		assert!(error.contains("parse status changed"));
	}

	#[test]
	fn noop_hints_are_available_without_loading_ast() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "common/scripted_effects/comments.txt";
		fs::create_dir_all(temp.path().join("common/scripted_effects"))
			.expect("create scripts dir");
		fs::write(temp.path().join(relative), "# comment only\n").expect("write source");
		let cache = cache_for_files(temp.path(), &[relative], true, &[(relative, true)]);

		assert_eq!(
			cache.is_noop_hint("mod-a", &game_path(relative)),
			Some(true)
		);
		assert!(!cache.is_loaded("mod-a", &game_path(relative)));
	}

	fn base_script(root: &Path, relative: &str) -> ParsedScriptFile {
		parse_script_bytes_cached("__game__eu4", root, &game_path(relative), b"a = 1\n")
	}

	#[test]
	fn decoded_base_scripts_are_keyed_by_game_path() {
		let root = Path::new("/base-game");
		let files = index_base_scripts(vec![base_script(root, "events/a.txt")])
			.expect("index base scripts");
		let parsed = files
			.get(&("__game__eu4".to_string(), game_path("events/a.txt")))
			.expect("keyed by game path");
		assert!(parsed.source.is_empty(), "preloaded sources are dropped");
		assert_eq!(
			parsed.path.as_deref(),
			Some(root.join("events").join("a.txt").as_path())
		);
	}

	#[test]
	fn decoded_base_scripts_sharing_a_game_path_fail_the_section() {
		// A decoded section that lists one game path twice would otherwise
		// leave only the last document under `events/a.txt`.
		let root = Path::new("/base-game");
		let error = index_base_scripts(vec![
			base_script(root, "events/a.txt"),
			base_script(root, "events/a.txt"),
		])
		.expect_err("two documents cannot share one game path");
		assert!(
			error.contains("__game__eu4:events/a.txt more than once"),
			"{error}"
		);
	}
}
