use crate::base_data::InstalledBaseSnapshot;
use foch::game::eu4::script::{ParsedScriptFile, parse_script_bytes_cached};
use foch::model::ModCandidate;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::LoadedModSnapshot;

type ScriptCacheKey = (String, String);

#[derive(Debug)]
struct LazyScriptFile {
	mod_id: String,
	root_path: PathBuf,
	absolute_path: PathBuf,
	relative_path: PathBuf,
	expected_size_bytes: u64,
	expected_content_digest: String,
	expected_parse_ok: Option<bool>,
	parsed: OnceLock<Result<Arc<ParsedScriptFile>, String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceScriptCache {
	loaded: Arc<HashMap<ScriptCacheKey, Arc<ParsedScriptFile>>>,
	lazy: Arc<HashMap<ScriptCacheKey, Arc<LazyScriptFile>>>,
	noop_hints: Arc<HashMap<ScriptCacheKey, bool>>,
}

impl WorkspaceScriptCache {
	pub(crate) fn from_parts(
		mods: &[ModCandidate],
		mod_snapshots: &[Option<LoadedModSnapshot>],
		installed_base_snapshot: Option<&InstalledBaseSnapshot>,
		base_game_root: Option<&Path>,
	) -> Result<Self, String> {
		let mut loaded = HashMap::new();
		if let (Some(installed), Some(root)) = (installed_base_snapshot, base_game_root) {
			match installed.snapshot.parsed_script_files(root) {
				Ok(documents) => {
					for document in documents {
						insert_loaded(&mut loaded, document);
					}
				}
				Err(err) => {
					tracing::warn!(
						target: "foch::workspace::scripts",
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
					noop_hints.insert(
						(mod_item.mod_id.clone(), normalize_path(Path::new(path))),
						*is_noop,
					);
				}
			} else {
				tracing::debug!(
					target: "foch::workspace::scripts",
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
			for (path, identity) in &snapshot.document_input_identities {
				let relative_path = PathBuf::from(path);
				if !is_safe_relative_path(&relative_path) {
					return Err(format!(
						"semantic snapshot contains unsafe path {path} for {}",
						mod_item.mod_id
					));
				}
				let key = (mod_item.mod_id.clone(), normalize_path(&relative_path));
				let expected_parse_ok = snapshot.document_parse_hints.get(&key.1).copied();
				let entry = Arc::new(LazyScriptFile {
					mod_id: mod_item.mod_id.clone(),
					root_path: root_path.clone(),
					absolute_path: root_path.join(&relative_path),
					relative_path,
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
			loaded: Arc::new(loaded),
			lazy: Arc::new(lazy),
			noop_hints: Arc::new(noop_hints),
		})
	}

	pub(crate) fn get(
		&self,
		mod_id: &str,
		relative_path: &Path,
	) -> Result<Option<Arc<ParsedScriptFile>>, String> {
		let key = (mod_id.to_string(), normalize_path(relative_path));
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
	pub(crate) fn is_loaded(&self, mod_id: &str, relative_path: &Path) -> bool {
		matches!(self.get(mod_id, relative_path), Ok(Some(_)))
	}

	pub(crate) fn is_noop_hint(&self, mod_id: &str, relative_path: &Path) -> Option<bool> {
		self.noop_hints
			.get(&(mod_id.to_string(), normalize_path(relative_path)))
			.copied()
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
		documents.sort_by(|lhs, rhs| {
			(lhs.mod_id.as_str(), lhs.relative_path.as_os_str())
				.cmp(&(rhs.mod_id.as_str(), rhs.relative_path.as_os_str()))
		});
		Ok(documents)
	}

	pub(crate) fn load(
		&self,
		contributor: &super::ResolvedFileContributor,
	) -> Result<Arc<ParsedScriptFile>, String> {
		let relative_path = contributor
			.absolute_path
			.strip_prefix(&contributor.root_path)
			.map_err(|_| {
				format!(
					"{} is outside contributor root {}",
					contributor.absolute_path.display(),
					contributor.root_path.display()
				)
			})?;
		if let Some(parsed) = self.get(&contributor.mod_id, relative_path)? {
			return Ok(parsed);
		}
		let key = (contributor.mod_id.clone(), normalize_path(relative_path));
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
		contributor: &super::ResolvedFileContributor,
	) -> Result<(), String> {
		if contributor.mod_id != self.mod_id
			|| contributor.root_path != self.root_path
			|| contributor.absolute_path != self.absolute_path
		{
			return Err(format!(
				"lazy AST contributor identity does not match semantic snapshot for {}:{}",
				self.mod_id,
				normalize_path(&self.relative_path)
			));
		}
		if contributor.parse_ok_hint != self.expected_parse_ok {
			return Err(format!(
				"lazy AST parse-status hint changed for {}:{}: snapshot={:?}, contributor={:?}",
				self.mod_id,
				normalize_path(&self.relative_path),
				self.expected_parse_ok,
				contributor.parse_ok_hint
			));
		}
		if self.expected_parse_ok.is_none() {
			return Err(format!(
				"lazy AST has no semantic parse-status hint for {}:{}",
				self.mod_id,
				normalize_path(&self.relative_path)
			));
		}
		Ok(())
	}

	fn load_verified(&self) -> Result<Arc<ParsedScriptFile>, String> {
		let bytes = std::fs::read(&self.absolute_path).map_err(|error| {
			format!(
				"failed to read snapshot-bound script {}:{}: {error}",
				self.mod_id,
				normalize_path(&self.relative_path)
			)
		})?;
		if bytes.len() as u64 != self.expected_size_bytes {
			return Err(format!(
				"snapshot-bound script size changed for {}:{}: expected {}, observed {}",
				self.mod_id,
				normalize_path(&self.relative_path),
				self.expected_size_bytes,
				bytes.len()
			));
		}
		let observed_digest = blake3::hash(&bytes).to_hex().to_string();
		if observed_digest != self.expected_content_digest {
			return Err(format!(
				"snapshot-bound script digest changed for {}:{}: expected {}, observed {}",
				self.mod_id,
				normalize_path(&self.relative_path),
				self.expected_content_digest,
				observed_digest
			));
		}
		let parsed =
			parse_script_bytes_cached(&self.mod_id, &self.root_path, &self.absolute_path, &bytes)
				.ok_or_else(|| {
				format!(
					"failed to parse snapshot-bound script {}:{}",
					self.mod_id,
					normalize_path(&self.relative_path)
				)
			})?;
		let observed_parse_ok = parsed.parse_issues.is_empty();
		if Some(observed_parse_ok) != self.expected_parse_ok {
			return Err(format!(
				"snapshot-bound script parse status changed for {}:{}: expected {:?}, observed {observed_parse_ok}",
				self.mod_id,
				normalize_path(&self.relative_path),
				self.expected_parse_ok
			));
		}
		Ok(Arc::new(parsed))
	}
}

fn insert_loaded(
	files: &mut HashMap<ScriptCacheKey, Arc<ParsedScriptFile>>,
	mut document: ParsedScriptFile,
) {
	document.source.clear();
	let key = (
		document.mod_id.clone(),
		normalize_path(&document.relative_path),
	);
	files.insert(key, Arc::new(document));
}

fn is_safe_relative_path(path: &Path) -> bool {
	!path.as_os_str().is_empty()
		&& !path.is_absolute()
		&& path
			.components()
			.all(|component| matches!(component, Component::Normal(_)))
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::cache::CachedDocumentInputIdentity;
	use foch::model::{ParseFamilyStats, SemanticIndex};
	use foch::playset::PlaysetEntry;
	use std::fs;
	use tempfile::TempDir;

	fn contributor(root: &Path, relative: &str) -> super::super::ResolvedFileContributor {
		super::super::ResolvedFileContributor {
			mod_id: "mod-a".to_string(),
			root_path: root.to_path_buf(),
			absolute_path: root.join(relative),
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
	) -> WorkspaceScriptCache {
		let document_input_identities = files
			.iter()
			.map(|relative| {
				let bytes = fs::read(root.join(relative)).expect("read semantic input");
				(
					(*relative).to_string(),
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
			files: files.iter().map(PathBuf::from).collect(),
		};
		let snapshot = LoadedModSnapshot {
			semantic_index: SemanticIndex::default(),
			inventory_paths: files.iter().map(PathBuf::from).collect(),
			mod_hash: Some("hash-a".to_string()),
			parsed_files: files.len(),
			parse_error_count: 0,
			parse_stats: ParseFamilyStats::default(),
			clausewitz_parse_cache_hits: 0,
			clausewitz_parse_cache_misses: 0,
			document_parse_hints: files
				.iter()
				.map(|relative| ((*relative).to_string(), parse_ok))
				.collect(),
			document_noop_hints: noop_hints
				.iter()
				.map(|(relative, hint)| ((*relative).to_string(), *hint))
				.collect(),
			document_input_identities,
			cache_hit: true,
		};
		WorkspaceScriptCache::from_parts(&[mod_item], &[Some(snapshot)], None, None)
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
				.get("mod-a", Path::new(relative_b))
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

		assert_eq!(cache.is_noop_hint("mod-a", Path::new(relative)), Some(true));
		assert!(!cache.is_loaded("mod-a", Path::new(relative)));
	}
}
