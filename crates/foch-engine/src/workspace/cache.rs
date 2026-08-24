use crate::cache::{
	CachedDocumentInputIdentity, CachedModData, ModParseCache, ModParseCacheStoreOutcome,
};
use foch::game::eu4::analysis::param_contracts::apply_registered_param_contracts;
use foch::game::eu4::script::documents::{
	ParsedTextDocument, build_semantic_index_from_owned_documents,
	discover_text_documents_from_paths, parse_discovered_text_documents,
};
use foch::game::eu4::script::parser::AstStatement;
use foch::model::{
	DocumentFamily, FamilyParseStats, ModCandidate, ParseFamilyStats, SemanticIndex,
};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Debug)]
pub(crate) struct LoadedModSnapshot {
	pub semantic_index: SemanticIndex,
	pub inventory_paths: Vec<PathBuf>,
	pub mod_hash: Option<String>,
	pub parsed_files: usize,
	pub parse_error_count: usize,
	pub parse_stats: ParseFamilyStats,
	#[cfg(test)]
	pub clausewitz_parse_cache_hits: usize,
	#[cfg(test)]
	pub clausewitz_parse_cache_misses: usize,
	pub document_parse_hints: HashMap<String, bool>,
	pub document_noop_hints: HashMap<String, bool>,
	pub document_input_identities: HashMap<String, CachedDocumentInputIdentity>,
	pub cache_hit: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProcessSnapshotCacheKey {
	mod_hash: String,
}

static PROCESS_SNAPSHOT_CACHE: OnceLock<
	Mutex<HashMap<ProcessSnapshotCacheKey, LoadedModSnapshot>>,
> = OnceLock::new();

pub(crate) fn load_or_build_mod_snapshot(
	game_key: &str,
	mod_item: &ModCandidate,
	filter: &super::FileFilter,
	mod_hash: Option<&str>,
) -> Result<Option<LoadedModSnapshot>, io::Error> {
	let cache = mod_hash.is_some().then(ModParseCache::open_default);
	load_or_build_mod_snapshot_with_cache(game_key, mod_item, filter, mod_hash, cache.as_ref())
}

fn load_or_build_mod_snapshot_with_cache(
	game_key: &str,
	mod_item: &ModCandidate,
	filter: &super::FileFilter,
	mod_hash: Option<&str>,
	cache: Option<&ModParseCache>,
) -> Result<Option<LoadedModSnapshot>, io::Error> {
	let Some(root) = mod_item.root_path.as_ref() else {
		return Ok(None);
	};
	let snapshot_started = Instant::now();
	eprintln!(
		"[merge] mod_snapshot: start mod_id={} files={}",
		mod_item.mod_id,
		mod_item.files.len()
	);
	// This label is a stable behavior namespace, not the detected install
	// version. Source identity is already fully bound by the ACF snapshot key.
	let disk_cache_game_key = game_key.to_string();
	let owned_mod_hash = mod_hash.map(ToOwned::to_owned);
	let process_cache_key = process_snapshot_cache_key(owned_mod_hash.as_deref());
	if let Some(key) = process_cache_key.as_ref()
		&& let Some(snapshot) = load_process_snapshot(key)
	{
		let profile = cache.and_then(|cache| {
			let mod_hash = owned_mod_hash.as_deref()?;
			cache.touch_entry(mod_hash, env!("CARGO_PKG_VERSION"), &disk_cache_game_key);
			cache.entry_profile(mod_hash, env!("CARGO_PKG_VERSION"), &disk_cache_game_key)
		});
		eprintln!(
			"[merge] mod_snapshot: cache_hit mod_id={} source=process elapsed_ms={} compressed_bytes={} uncompressed_bytes={} documents={} scopes={} definitions={} references={}",
			mod_item.mod_id,
			snapshot_started.elapsed().as_millis(),
			profile.map_or(0, |item| item.compressed_bytes),
			profile.map_or(0, |item| item.uncompressed_bytes),
			snapshot.semantic_index.documents.len(),
			snapshot.semantic_index.scopes.len(),
			snapshot.semantic_index.definitions.len(),
			snapshot.semantic_index.references.len()
		);
		return Ok(Some(snapshot));
	}

	let cache_lookup_started = Instant::now();
	if let (Some(cache), Some(mod_hash)) = (cache, owned_mod_hash.as_ref())
		&& let Some(cached) =
			cache.lookup(mod_hash, env!("CARGO_PKG_VERSION"), &disk_cache_game_key)
	{
		let profile =
			cache.entry_profile(mod_hash, env!("CARGO_PKG_VERSION"), &disk_cache_game_key);
		let snapshot = to_loaded_snapshot(cached, true, owned_mod_hash.clone());
		store_process_snapshot(process_cache_key.as_ref(), &snapshot);
		eprintln!(
			"[merge] mod_snapshot: cache_hit mod_id={} source=disk lookup_ms={} elapsed_ms={} compressed_bytes={} uncompressed_bytes={} documents={} scopes={} definitions={} references={}",
			mod_item.mod_id,
			cache_lookup_started.elapsed().as_millis(),
			snapshot_started.elapsed().as_millis(),
			profile.map_or(0, |item| item.compressed_bytes),
			profile.map_or(0, |item| item.uncompressed_bytes),
			snapshot.semantic_index.documents.len(),
			snapshot.semantic_index.scopes.len(),
			snapshot.semantic_index.definitions.len(),
			snapshot.semantic_index.references.len()
		);
		return Ok(Some(snapshot));
	}

	let inventory_paths = super::resolve::collect_relative_files(root, filter)?;
	let parse_started = Instant::now();
	let parsed_snapshot = parse_mod_snapshot(mod_item, root, &inventory_paths, filter);
	eprintln!(
		"[merge] mod_snapshot: parse_done mod_id={} elapsed_ms={} cache_hits={} cache_misses={}",
		mod_item.mod_id,
		parse_started.elapsed().as_millis(),
		parsed_snapshot.clausewitz_parse_cache_hits,
		parsed_snapshot.clausewitz_parse_cache_misses
	);
	eprintln!(
		"[merge] mod_snapshot: semantic_done mod_id={} elapsed_ms={} documents={} scopes={} definitions={} references={}",
		mod_item.mod_id,
		parsed_snapshot.semantic_elapsed_ms,
		parsed_snapshot.data.semantic_index.documents.len(),
		parsed_snapshot.data.semantic_index.scopes.len(),
		parsed_snapshot.data.semantic_index.definitions.len(),
		parsed_snapshot.data.semantic_index.references.len()
	);
	let mut data = parsed_snapshot.data;
	let store_started = Instant::now();
	let mut store_state = "skipped";
	let mut stored_profile = None;
	if let (Some(cache), Some(mod_hash)) = (cache, owned_mod_hash.as_ref()) {
		let (returned_data, store_result) = cache.store_owned(
			mod_hash,
			env!("CARGO_PKG_VERSION"),
			&disk_cache_game_key,
			data,
		);
		data = returned_data;
		match store_result {
			Ok(ModParseCacheStoreOutcome::Stored(profile)) => {
				store_state = "stored";
				stored_profile = Some(profile);
			}
			Ok(ModParseCacheStoreOutcome::RejectedTooLarge {
				compressed_bytes,
				cap_bytes,
			}) => {
				store_state = "rejected_too_large";
				tracing::warn!(
					target: "foch::workspace::resolve",
					mod_id = %mod_item.mod_id,
					compressed_bytes,
					cap_bytes,
					"mod parse cache entry is too large to remain resident"
				);
			}
			Err(err) => {
				store_state = "error";
				tracing::warn!(
					target: "foch::workspace::resolve",
					mod_id = %mod_item.mod_id,
					error = %err,
					"failed to store mod parse cache entry"
				);
			}
		}
	}
	eprintln!(
		"[merge] mod_snapshot: cache_store mod_id={} state={} elapsed_ms={} total_ms={} compressed_bytes={} uncompressed_bytes={}",
		mod_item.mod_id,
		store_state,
		store_started.elapsed().as_millis(),
		snapshot_started.elapsed().as_millis(),
		stored_profile.map_or(0, |item| item.compressed_bytes),
		stored_profile.map_or(0, |item| item.uncompressed_bytes)
	);

	let snapshot = to_loaded_snapshot_with_stats(
		data,
		parsed_snapshot.parse_stats,
		parsed_snapshot.parse_error_count,
		parsed_snapshot.clausewitz_parse_cache_hits,
		parsed_snapshot.clausewitz_parse_cache_misses,
		false,
		owned_mod_hash,
	);
	store_process_snapshot(process_cache_key.as_ref(), &snapshot);
	Ok(Some(snapshot))
}

fn process_snapshot_cache_key(mod_hash: Option<&str>) -> Option<ProcessSnapshotCacheKey> {
	Some(ProcessSnapshotCacheKey {
		mod_hash: mod_hash?.to_string(),
	})
}

fn load_process_snapshot(key: &ProcessSnapshotCacheKey) -> Option<LoadedModSnapshot> {
	let cache = PROCESS_SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	let mut snapshot = cache.lock().ok()?.get(key)?.clone();
	snapshot.cache_hit = true;
	Some(snapshot)
}

fn store_process_snapshot(key: Option<&ProcessSnapshotCacheKey>, snapshot: &LoadedModSnapshot) {
	let Some(key) = key else {
		return;
	};
	let cache = PROCESS_SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
	if let Ok(mut guard) = cache.lock() {
		guard.insert(key.clone(), snapshot.clone());
	}
}

fn to_loaded_snapshot(
	data: CachedModData,
	cache_hit: bool,
	mod_hash: Option<String>,
) -> LoadedModSnapshot {
	let parse_stats = parse_stats_from_index(&data.semantic_index);
	let parse_error_count = parse_stats.clausewitz_mainline.parse_issue_count;
	to_loaded_snapshot_with_stats(
		data,
		parse_stats,
		parse_error_count,
		0,
		0,
		cache_hit,
		mod_hash,
	)
}

fn to_loaded_snapshot_with_stats(
	data: CachedModData,
	parse_stats: ParseFamilyStats,
	parse_error_count: usize,
	_clausewitz_parse_cache_hits: usize,
	_clausewitz_parse_cache_misses: usize,
	cache_hit: bool,
	mod_hash: Option<String>,
) -> LoadedModSnapshot {
	let CachedModData {
		mut semantic_index,
		inventory_paths,
		document_noop_hints,
		document_input_identities,
	} = data;
	apply_registered_param_contracts(&mut semantic_index);
	let document_parse_hints = semantic_index
		.documents
		.iter()
		.map(|item| (normalize_relative_path(&item.path), item.parse_ok))
		.collect();
	let document_noop_hints = semantic_index
		.documents
		.iter()
		.zip(document_noop_hints)
		.map(|(item, is_noop)| (normalize_relative_path(&item.path), is_noop))
		.collect();
	let document_input_identities = semantic_index
		.documents
		.iter()
		.zip(document_input_identities)
		.filter_map(|(item, identity)| {
			identity.map(|identity| (normalize_relative_path(&item.path), identity))
		})
		.collect();
	LoadedModSnapshot {
		parsed_files: semantic_index.documents.len(),
		semantic_index,
		inventory_paths: inventory_paths.into_iter().map(PathBuf::from).collect(),
		mod_hash,
		parse_error_count,
		parse_stats,
		#[cfg(test)]
		clausewitz_parse_cache_hits: _clausewitz_parse_cache_hits,
		#[cfg(test)]
		clausewitz_parse_cache_misses: _clausewitz_parse_cache_misses,
		document_parse_hints,
		document_noop_hints,
		document_input_identities,
		cache_hit,
	}
}

struct ParsedModSnapshot {
	data: CachedModData,
	parse_stats: ParseFamilyStats,
	parse_error_count: usize,
	clausewitz_parse_cache_hits: usize,
	clausewitz_parse_cache_misses: usize,
	semantic_elapsed_ms: u128,
}

fn parse_mod_snapshot(
	mod_item: &ModCandidate,
	root: &Path,
	inventory_paths: &[PathBuf],
	filter: &super::FileFilter,
) -> ParsedModSnapshot {
	let documents = discover_text_documents_from_paths(root, inventory_paths)
		.into_iter()
		.filter(|document| filter.accepts(&document.relative_path))
		.collect::<Vec<_>>();
	let mut parsed = parse_discovered_text_documents(&mod_item.mod_id, root, &documents);
	let semantic_started = Instant::now();
	let document_noop_hints_by_path = collect_document_noop_hints(&parsed.documents);
	let mut document_input_identities_by_path = parsed
		.document_input_identities
		.drain(..)
		.map(|identity| {
			(
				normalize_relative_path(&identity.relative_path),
				CachedDocumentInputIdentity {
					size_bytes: identity.size_bytes,
					content_digest: identity.content_digest,
				},
			)
		})
		.collect::<HashMap<_, _>>();
	let semantic_index =
		build_semantic_index_from_owned_documents(std::mem::take(&mut parsed.documents));
	let document_noop_hints =
		document_noop_hints_for_index(&semantic_index, &document_noop_hints_by_path);
	let document_input_identities = semantic_index
		.documents
		.iter()
		.map(|document| {
			document_input_identities_by_path.remove(&normalize_relative_path(&document.path))
		})
		.collect();
	let mut normalized_inventory_paths = inventory_paths
		.iter()
		.map(|path| normalize_relative_path(path))
		.collect::<Vec<_>>();
	normalized_inventory_paths.sort();
	normalized_inventory_paths.dedup();
	let parse_stats = parsed.parse_stats;
	let parse_error_count = parse_stats.clausewitz_mainline.parse_issue_count;
	ParsedModSnapshot {
		data: CachedModData {
			semantic_index,
			inventory_paths: normalized_inventory_paths,
			document_noop_hints,
			document_input_identities,
		},
		parse_stats,
		parse_error_count,
		clausewitz_parse_cache_hits: parsed.clausewitz_cache_hits,
		clausewitz_parse_cache_misses: parsed.clausewitz_cache_misses,
		semantic_elapsed_ms: semantic_started.elapsed().as_millis(),
	}
}

pub(crate) fn build_transient_mod_snapshot(
	mod_item: &ModCandidate,
	filter: &super::FileFilter,
	full_snapshot: &LoadedModSnapshot,
) -> Result<Option<LoadedModSnapshot>, io::Error> {
	let Some(root) = mod_item.root_path.as_ref() else {
		return Ok(None);
	};
	let parsed = parse_mod_snapshot(mod_item, root, &mod_item.files, filter);
	Ok(Some(to_loaded_snapshot_with_stats(
		parsed.data,
		parsed.parse_stats,
		parsed.parse_error_count,
		parsed.clausewitz_parse_cache_hits,
		parsed.clausewitz_parse_cache_misses,
		full_snapshot.cache_hit,
		full_snapshot.mod_hash.clone(),
	)))
}

fn collect_document_noop_hints(documents: &[ParsedTextDocument]) -> HashMap<String, bool> {
	documents
		.iter()
		.filter_map(|document| match document {
			ParsedTextDocument::Clausewitz(file) => Some((
				normalize_relative_path(&file.relative_path),
				file.parse_issues.is_empty()
					&& !ast_statement_list_has_real_content(&file.ast.statements),
			)),
			ParsedTextDocument::Localisation(_)
			| ParsedTextDocument::Csv(_)
			| ParsedTextDocument::Json(_) => None,
		})
		.collect()
}

fn document_noop_hints_for_index(
	semantic_index: &SemanticIndex,
	by_path: &HashMap<String, bool>,
) -> Vec<bool> {
	semantic_index
		.documents
		.iter()
		.map(|document| {
			by_path
				.get(&normalize_relative_path(&document.path))
				.copied()
				.unwrap_or(false)
		})
		.collect()
}

fn ast_statement_list_has_real_content(statements: &[AstStatement]) -> bool {
	statements
		.iter()
		.any(|statement| !matches!(statement, AstStatement::Comment { .. }))
}

fn parse_stats_from_index(index: &SemanticIndex) -> ParseFamilyStats {
	let family_lookup = index
		.documents
		.iter()
		.map(|document| {
			(
				(
					document.mod_id.clone(),
					normalize_relative_path(&document.path),
				),
				document.family,
			)
		})
		.collect::<HashMap<_, _>>();
	let mut stats = ParseFamilyStats::default();
	for document in &index.documents {
		let family_stats = family_stats_mut(&mut stats, document.family);
		family_stats.documents += 1;
		if !document.parse_ok {
			family_stats.parse_failed_documents += 1;
		}
	}
	for issue in &index.parse_issues {
		let key = (issue.mod_id.clone(), normalize_relative_path(&issue.path));
		let family = family_lookup
			.get(&key)
			.copied()
			.unwrap_or(DocumentFamily::Clausewitz);
		family_stats_mut(&mut stats, family).parse_issue_count += 1;
	}
	stats
}

fn family_stats_mut(stats: &mut ParseFamilyStats, family: DocumentFamily) -> &mut FamilyParseStats {
	match family {
		DocumentFamily::Clausewitz => &mut stats.clausewitz_mainline,
		DocumentFamily::Localisation => &mut stats.localisation,
		DocumentFamily::Csv => &mut stats.csv,
		DocumentFamily::Json => &mut stats.json,
	}
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch::game::eu4::Eu4;
	use foch::playset::PlaysetEntry;
	use foch::playset::descriptor::ModDescriptor;
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn acf_snapshot_is_warm_before_walk_and_retained_subset_does_not_poison_full_cache() {
		let temp = TempDir::new().expect("temp dir");
		let cache_dir = temp.path().join("cache");
		let cache = ModParseCache::open(&cache_dir);
		let mod_root = temp.path().join("9001");
		fs::create_dir_all(mod_root.join("common").join("defines")).expect("create defines root");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create mod root");
		fs::write(
			mod_root
				.join("common")
				.join("defines")
				.join("cache_test.lua"),
			"NDefines.NCountry.CACHE_TEST = 1\n",
		)
		.expect("write defines file");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			"ME_give_claims = { add_prestige = 1 }\n",
		)
		.expect("write scripted effect");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("comments.txt"),
			"# comment only\n",
		)
		.expect("write comment-only script");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("empty_block.txt"),
			"empty_effect = {}\n",
		)
		.expect("write empty block script");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("comment_block.txt"),
			"comment_effect = { # retained empty override\n}\n",
		)
		.expect("write comment-only block script");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("omitted.txt"),
			"omitted_effect = { add_prestige = 2 }\n",
		)
		.expect("write omitted scripted effect");
		let mod_item = ModCandidate {
			entry: PlaysetEntry {
				enabled: true,
				position: Some(0),
				steam_id: Some("9001".to_string()),
				display_name: Some("cache-test".to_string()),
				..PlaysetEntry::default()
			},
			mod_id: "9001".to_string(),
			root_path: Some(mod_root.clone()),
			descriptor_path: Some(mod_root.join("descriptor.mod")),
			descriptor: Some(ModDescriptor {
				name: "cache-test".to_string(),
				path: None,
				tags: Vec::new(),
				dependencies: Vec::new(),
				replace_path: Vec::new(),
				version: None,
				remote_file_id: Some("9001".to_string()),
				supported_version: None,
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		};
		let filter = super::super::FileFilter::for_game(Eu4);
		let mod_hash = "acf-key-9001".to_string();

		let cold = load_or_build_mod_snapshot_with_cache(
			"eu4",
			&mod_item,
			&filter,
			Some(&mod_hash),
			Some(&cache),
		)
		.expect("load cold snapshot")
		.expect("cold snapshot");
		assert!(!cold.cache_hit);
		assert!(cold.mod_hash.is_some());
		assert_eq!(cold.parsed_files, 6);
		assert_eq!(cold.inventory_paths.len(), 6);
		assert_eq!(cold.document_input_identities.len(), 6);
		assert!(
			cold.semantic_index
				.documents
				.iter()
				.any(|document| document.path.ends_with("omitted.txt"))
		);

		let mut retained_mod = mod_item.clone();
		retained_mod.files = vec![
			PathBuf::from("common/defines/cache_test.lua"),
			PathBuf::from("common/scripted_effects/comment_block.txt"),
			PathBuf::from("common/scripted_effects/comments.txt"),
			PathBuf::from("common/scripted_effects/effects.txt"),
			PathBuf::from("common/scripted_effects/empty_block.txt"),
		];
		let entries_before_subset = fs::read_dir(&cache_dir)
			.expect("list semantic cache before subset")
			.flatten()
			.count();
		let subset = build_transient_mod_snapshot(&retained_mod, &filter, &cold)
			.expect("build retained snapshot")
			.expect("retained snapshot");
		let entries_after_subset = fs::read_dir(&cache_dir)
			.expect("list semantic cache after subset")
			.flatten()
			.count();
		assert_eq!(entries_after_subset, entries_before_subset);
		assert_eq!(subset.parsed_files, 5);
		assert_eq!(subset.inventory_paths.len(), 5);
		assert!(
			subset
				.semantic_index
				.documents
				.iter()
				.all(|document| !document.path.ends_with("omitted.txt"))
		);

		if let Some(process_cache) = PROCESS_SNAPSHOT_CACHE.get() {
			process_cache
				.lock()
				.expect("lock process snapshot cache")
				.remove(&ProcessSnapshotCacheKey {
					mod_hash: mod_hash.clone(),
				});
		}
		let hidden_root = temp.path().join("9001-hidden");
		fs::rename(&mod_root, &hidden_root).expect("hide mod tree before warm lookup");
		let warm_result = load_or_build_mod_snapshot_with_cache(
			"eu4",
			&mod_item,
			&filter,
			Some(&mod_hash),
			Some(&cache),
		);
		fs::rename(&hidden_root, &mod_root).expect("restore mod tree after warm lookup");
		let warm = warm_result
			.expect("warm lookup must not access the mod tree")
			.expect("warm snapshot");

		assert!(warm.cache_hit);
		assert_eq!(warm.mod_hash, cold.mod_hash);
		assert_eq!(warm.parsed_files, 6);
		assert_eq!(warm.inventory_paths, cold.inventory_paths);
		assert_eq!(
			warm.document_input_identities,
			cold.document_input_identities
		);
		assert!(
			warm.semantic_index
				.documents
				.iter()
				.any(|document| document.path.ends_with("omitted.txt")),
			"a retained transient snapshot must not overwrite the full ACF snapshot"
		);
		let effects_bytes = fs::read(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
		)
		.expect("read scripted effect identity input");
		assert_eq!(
			warm.document_input_identities
				.get("common/scripted_effects/effects.txt"),
			Some(&CachedDocumentInputIdentity {
				size_bytes: effects_bytes.len() as u64,
				content_digest: blake3::hash(&effects_bytes).to_hex().to_string(),
			})
		);
		assert_eq!(
			warm.document_noop_hints
				.get("common/scripted_effects/comments.txt"),
			Some(&true)
		);
		assert_eq!(
			warm.document_noop_hints
				.get("common/scripted_effects/empty_block.txt"),
			Some(&false),
			"an empty assignment block can clear or override prior content"
		);
		assert_eq!(
			warm.document_noop_hints
				.get("common/scripted_effects/comment_block.txt"),
			Some(&false),
			"comments inside an assigned block do not make the assignment a no-op"
		);
		assert_eq!(
			warm.document_noop_hints
				.get("common/scripted_effects/effects.txt"),
			Some(&false)
		);
		let semantic_entry = fs::read_dir(&cache_dir)
			.expect("list semantic cache")
			.filter_map(Result::ok)
			.map(|entry| entry.path())
			.find(|path| path.extension().and_then(|value| value.to_str()) == Some("rkyv"))
			.expect("semantic cache entry");
		fs::remove_file(semantic_entry).expect("remove semantic cache entry");
		if let Some(process_cache) = PROCESS_SNAPSHOT_CACHE.get() {
			process_cache
				.lock()
				.expect("lock process snapshot cache")
				.remove(&ProcessSnapshotCacheKey {
					mod_hash: mod_hash.clone(),
				});
		}
		let rebuilt = load_or_build_mod_snapshot_with_cache(
			"eu4",
			&mod_item,
			&filter,
			Some(&mod_hash),
			Some(&cache),
		)
		.expect("rebuild semantic snapshot")
		.expect("rebuilt snapshot");
		assert!(!rebuilt.cache_hit);
		assert_eq!(rebuilt.clausewitz_parse_cache_hits, 6);
		assert_eq!(rebuilt.clausewitz_parse_cache_misses, 0);
	}
}
