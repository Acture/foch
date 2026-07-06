use crate::cache::{CachedModData, ModParseCache, compute_mod_hash_with_filter};
use foch_core::model::{
	DocumentFamily, FamilyParseStats, ModCandidate, ParseFamilyStats, SemanticIndex,
};
use foch_language::analyzer::documents::{
	ParsedTextDocument, build_semantic_index_from_documents, discover_text_documents,
	parse_discovered_text_documents,
};
use foch_language::analyzer::param_contracts::apply_registered_param_contracts;
use foch_language::analyzer::semantic_index::ParsedScriptFile;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct LoadedModSnapshot {
	pub semantic_index: SemanticIndex,
	pub parsed_documents: Vec<ParsedScriptFile>,
	pub mod_hash: Option<String>,
	pub parsed_files: usize,
	pub parse_error_count: usize,
	pub parse_stats: ParseFamilyStats,
	pub document_parse_hints: HashMap<String, bool>,
	pub cache_hit: bool,
}

pub(crate) fn load_or_build_mod_snapshot(
	game_key: &str,
	game_version: Option<&str>,
	mod_item: &ModCandidate,
	filter: &super::FileFilter,
	mod_hash: Option<&str>,
) -> Option<LoadedModSnapshot> {
	let root = mod_item.root_path.as_ref()?;
	let cache_game_version = game_version.map(|version| format!("{game_key} {version}"));
	let cache = cache_game_version
		.as_ref()
		.map(|_| ModParseCache::open_default());
	let owned_mod_hash = mod_hash.map(ToOwned::to_owned).or_else(|| {
		cache
			.as_ref()
			.and_then(|_| compute_mod_hash_with_filter(root, filter).ok())
	});

	if let (Some(cache), Some(mod_hash), Some(cache_game_version)) = (
		cache.as_ref(),
		owned_mod_hash.as_ref(),
		cache_game_version.as_ref(),
	) && let Some(mut cached) =
		cache.lookup(mod_hash, env!("CARGO_PKG_VERSION"), cache_game_version)
	{
		crate::cache::parsed_scripts::rebase_parsed_documents(root, &mut cached.parsed_documents);
		return Some(to_loaded_snapshot(cached, true, owned_mod_hash.clone()));
	}

	let documents = discover_text_documents(root)
		.into_iter()
		.filter(|doc| filter.accepts(&doc.relative_path))
		.collect::<Vec<_>>();
	let parsed = parse_discovered_text_documents(&mod_item.mod_id, root, &documents);
	let semantic_index = build_semantic_index_from_documents(&parsed.documents);
	let parsed_documents = parsed
		.documents
		.iter()
		.filter_map(|doc| match doc {
			ParsedTextDocument::Clausewitz(file) => Some(file.clone()),
			_ => None,
		})
		.collect::<Vec<_>>();
	let parse_stats = parsed.parse_stats;
	let parse_error_count = parse_stats.clausewitz_mainline.parse_issue_count;
	let data = CachedModData {
		semantic_index,
		parsed_documents,
	};
	if let (Some(cache), Some(mod_hash), Some(cache_game_version)) = (
		cache.as_ref(),
		owned_mod_hash.as_ref(),
		cache_game_version.as_ref(),
	) && let Err(err) = cache.store(
		mod_hash,
		env!("CARGO_PKG_VERSION"),
		cache_game_version,
		&data,
	) {
		tracing::warn!(
			target: "foch::workspace::resolve",
			mod_id = %mod_item.mod_id,
			error = %err,
			"failed to store mod parse cache entry"
		);
	}

	Some(to_loaded_snapshot_with_stats(
		data,
		parse_stats,
		parse_error_count,
		false,
		owned_mod_hash,
	))
}

fn to_loaded_snapshot(
	data: CachedModData,
	cache_hit: bool,
	mod_hash: Option<String>,
) -> LoadedModSnapshot {
	let parse_stats = parse_stats_from_index(&data.semantic_index);
	let parse_error_count = parse_stats.clausewitz_mainline.parse_issue_count;
	to_loaded_snapshot_with_stats(data, parse_stats, parse_error_count, cache_hit, mod_hash)
}

fn to_loaded_snapshot_with_stats(
	data: CachedModData,
	parse_stats: ParseFamilyStats,
	parse_error_count: usize,
	cache_hit: bool,
	mod_hash: Option<String>,
) -> LoadedModSnapshot {
	let mut semantic_index = data.semantic_index;
	apply_registered_param_contracts(&mut semantic_index);
	let document_parse_hints = semantic_index
		.documents
		.iter()
		.map(|item| (normalize_relative_path(&item.path), item.parse_ok))
		.collect();
	LoadedModSnapshot {
		parsed_files: semantic_index.documents.len(),
		semantic_index,
		parsed_documents: data.parsed_documents,
		mod_hash,
		parse_error_count,
		parse_stats,
		document_parse_hints,
		cache_hit,
	}
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
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::game::Game;
	use foch_core::domain::playlist::PlaylistEntry;
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn load_or_build_mod_snapshot_reuses_content_addressed_cache() {
		let temp = TempDir::new().expect("temp dir");
		let cache_dir = temp.path().join("cache");
		unsafe {
			std::env::set_var("FOCH_MOD_PARSE_CACHE_DIR", &cache_dir);
		}
		let mod_root = temp.path().join("9001");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create mod root");
		fs::write(
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt"),
			"ME_give_claims = { add_prestige = 1 }\n",
		)
		.expect("write scripted effect");
		let mod_item = ModCandidate {
			entry: PlaylistEntry {
				enabled: true,
				position: Some(0),
				steam_id: Some("9001".to_string()),
				display_name: Some("cache-test".to_string()),
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
			descriptor_error: None,
			files: Vec::new(),
		};
		let filter = super::super::FileFilter::for_game(Game::EuropaUniversalis4);

		let cold = load_or_build_mod_snapshot("eu4", Some("1.0.0-test"), &mod_item, &filter, None)
			.expect("cold snapshot");
		let warm = load_or_build_mod_snapshot("eu4", Some("1.0.0-test"), &mod_item, &filter, None)
			.expect("warm snapshot");

		assert!(!cold.cache_hit);
		assert!(warm.cache_hit);
		assert!(cold.mod_hash.is_some());
		assert_eq!(warm.mod_hash, cold.mod_hash);
		assert_eq!(warm.parsed_files, 1);
		assert_eq!(warm.parsed_documents.len(), 1);
		let cached_document = &warm.parsed_documents[0];
		assert_eq!(
			cached_document.relative_path,
			std::path::PathBuf::from("common/scripted_effects/effects.txt")
		);
		assert_eq!(
			cached_document.path,
			mod_root
				.join("common")
				.join("scripted_effects")
				.join("effects.txt")
		);
		assert_eq!(
			cached_document.source,
			"ME_give_claims = { add_prestige = 1 }\n"
		);

		let script_cache = super::super::WorkspaceScriptCache::from_parts(
			std::slice::from_ref(&mod_item),
			&[Some(warm.clone())],
			None,
			None,
		);
		assert!(
			script_cache
				.get(
					"9001",
					std::path::Path::new("common/scripted_effects/effects.txt")
				)
				.is_some()
		);
		unsafe {
			std::env::remove_var("FOCH_MOD_PARSE_CACHE_DIR");
		}
	}
}
