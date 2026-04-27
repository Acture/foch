use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use foch_core::model::{ModCandidate, ParseFamilyStats, SemanticIndex};
use foch_language::analysis_version::analysis_rules_version;
use foch_language::analyzer::documents::{
	DiscoveredTextDocument, build_semantic_index_from_documents, discover_text_documents,
	parse_discovered_text_documents,
};
use foch_language::analyzer::param_contracts::apply_registered_param_contracts;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const MOD_SNAPSHOT_CACHE_DIR_ENV: &str = "FOCH_MOD_SNAPSHOT_CACHE_DIR";
const MOD_SNAPSHOT_SCHEMA_VERSION: u32 = 10;

#[derive(Clone, Debug)]
pub(crate) struct LoadedModSnapshot {
	pub semantic_index: SemanticIndex,
	pub parsed_files: usize,
	pub parse_error_count: usize,
	pub parse_stats: ParseFamilyStats,
	pub document_parse_hints: HashMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredModSemanticSnapshot {
	schema_version: u32,
	game: String,
	game_version: String,
	analysis_rules_version: String,
	mod_identity: String,
	manifest_hash: u64,
	generated_by_cli_version: String,
	parsed_files: usize,
	parse_error_count: usize,
	parse_stats: ParseFamilyStats,
	semantic_index: SemanticIndex,
}

pub(crate) fn load_or_build_mod_snapshot(
	game_key: &str,
	game_version: Option<&str>,
	mod_item: &ModCandidate,
	filter: &super::FileFilter,
) -> Option<LoadedModSnapshot> {
	let root = mod_item.root_path.as_ref()?;
	let documents: Vec<DiscoveredTextDocument> = discover_text_documents(root)
		.into_iter()
		.filter(|doc| filter.accepts(&doc.relative_path))
		.collect();
	let manifest_hash = semantic_manifest_hash(&documents);
	let mod_identity = mod_cache_identity(mod_item);
	let resolved_game_version = game_version.map(str::to_string);
	let cache_path = resolved_game_version.as_ref().map(|version| {
		mod_snapshot_cache_file(
			game_key,
			version,
			analysis_rules_version(),
			&mod_identity,
			manifest_hash,
		)
	});

	if let (Some(cache_path), Some(game_version)) =
		(cache_path.as_ref(), resolved_game_version.as_ref())
		&& let Some(entry) = load_mod_snapshot(cache_path)
		&& entry.schema_version == MOD_SNAPSHOT_SCHEMA_VERSION
		&& entry.game == game_key
		&& entry.game_version == *game_version
		&& entry.analysis_rules_version == analysis_rules_version()
		&& entry.mod_identity == mod_identity
		&& entry.manifest_hash == manifest_hash
	{
		return Some(to_loaded_snapshot(entry));
	}

	let parsed = parse_discovered_text_documents(&mod_item.mod_id, root, &documents);
	let semantic_index = build_semantic_index_from_documents(&parsed.documents);
	let parse_error_count = parsed.parse_stats.clausewitz_mainline.parse_issue_count;
	let parsed_files = parsed.documents.len();
	let entry = StoredModSemanticSnapshot {
		schema_version: MOD_SNAPSHOT_SCHEMA_VERSION,
		game: game_key.to_string(),
		game_version: resolved_game_version.clone().unwrap_or_default(),
		analysis_rules_version: analysis_rules_version().to_string(),
		mod_identity,
		manifest_hash,
		generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
		parsed_files,
		parse_error_count,
		parse_stats: parsed.parse_stats,
		semantic_index,
	};
	if let Some(cache_path) = cache_path.as_ref() {
		store_mod_snapshot(cache_path, &entry);
	}
	Some(to_loaded_snapshot(entry))
}

pub(crate) fn mod_snapshot_cache_root() -> PathBuf {
	if let Ok(override_dir) = std::env::var(MOD_SNAPSHOT_CACHE_DIR_ENV) {
		return PathBuf::from(override_dir);
	}
	dirs::cache_dir()
		.unwrap_or_else(std::env::temp_dir)
		.join("foch")
		.join("mod_snapshots")
}

fn to_loaded_snapshot(entry: StoredModSemanticSnapshot) -> LoadedModSnapshot {
	let mut semantic_index = entry.semantic_index;
	apply_registered_param_contracts(&mut semantic_index);
	let document_parse_hints = semantic_index
		.documents
		.iter()
		.map(|item| (normalize_relative_path(&item.path), item.parse_ok))
		.collect();
	LoadedModSnapshot {
		semantic_index,
		parsed_files: entry.parsed_files,
		parse_error_count: entry.parse_error_count,
		parse_stats: entry.parse_stats,
		document_parse_hints,
	}
}

fn mod_cache_identity(mod_item: &ModCandidate) -> String {
	if mod_item.mod_id != "<missing-steam-id>" {
		return sanitize_component(&mod_item.mod_id);
	}
	if let Some(descriptor) = mod_item.descriptor.as_ref() {
		return sanitize_component(&descriptor.name);
	}
	if let Some(name) = mod_item.entry.display_name.as_ref() {
		return sanitize_component(name);
	}
	if let Some(root) = mod_item.root_path.as_ref() {
		let mut hasher = DefaultHasher::new();
		root.to_string_lossy().replace('\\', "/").hash(&mut hasher);
		return format!("root-{:016x}", hasher.finish());
	}
	"unknown".to_string()
}

fn mod_snapshot_cache_file(
	game_key: &str,
	game_version: &str,
	analysis_rules_version: &str,
	mod_identity: &str,
	manifest_hash: u64,
) -> PathBuf {
	mod_snapshot_cache_root()
		.join(game_key)
		.join(sanitize_component(game_version))
		.join(sanitize_component(analysis_rules_version))
		.join(mod_identity)
		.join(format!("{manifest_hash:016x}.bin.gz"))
}

fn load_mod_snapshot(path: &Path) -> Option<StoredModSemanticSnapshot> {
	let file = fs::File::open(path).ok()?;
	let reader = BufReader::new(file);
	let decoder = GzDecoder::new(reader);
	bincode::deserialize_from(decoder).ok()
}

fn store_mod_snapshot(path: &Path, entry: &StoredModSemanticSnapshot) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let tmp = path.with_extension("bin.gz.tmp");
	let Ok(file) = fs::File::create(&tmp) else {
		return;
	};
	let mut encoder = GzEncoder::new(file, Compression::default());
	if bincode::serialize_into(&mut encoder, entry).is_err() {
		let _ = fs::remove_file(&tmp);
		return;
	}
	if encoder.finish().is_err() {
		let _ = fs::remove_file(&tmp);
		return;
	}
	let _ = fs::rename(tmp, path);
}

fn semantic_manifest_hash(files: &[DiscoveredTextDocument]) -> u64 {
	let mut entries: Vec<String> = Vec::new();
	for file in files {
		let relative = normalize_relative_path(&file.relative_path);
		let mut entry = relative;
		if let Ok(metadata) = fs::metadata(&file.absolute_path) {
			entry.push('|');
			entry.push_str(&metadata.len().to_string());
			entry.push('|');
			entry.push_str(&modified_nanos(&metadata).to_string());
		}
		entries.push(entry);
	}
	entries.sort();

	let mut hasher = DefaultHasher::new();
	for entry in entries {
		entry.hash(&mut hasher);
	}
	hasher.finish()
}

fn modified_nanos(metadata: &fs::Metadata) -> u128 {
	metadata
		.modified()
		.ok()
		.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |duration| duration.as_nanos())
}

fn sanitize_component(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for ch in value.chars() {
		if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
			out.push(ch);
		} else {
			out.push('_');
		}
	}
	if out.is_empty() {
		"unknown".to_string()
	} else {
		out
	}
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
	use super::{
		MOD_SNAPSHOT_CACHE_DIR_ENV, MOD_SNAPSHOT_SCHEMA_VERSION, StoredModSemanticSnapshot,
		load_or_build_mod_snapshot, mod_snapshot_cache_file, store_mod_snapshot,
	};
	use foch_core::domain::descriptor::ModDescriptor;
	use foch_core::domain::playlist::PlaylistEntry;
	use foch_core::model::{ModCandidate, ParseFamilyStats};
	use foch_language::analysis_version::analysis_rules_version;
	use std::sync::Mutex;
	use tempfile::TempDir;

	static MOD_CACHE_ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn load_or_build_mod_snapshot_rejects_old_schema_version() {
		let _guard = MOD_CACHE_ENV_LOCK.lock().expect("env lock");
		let temp = TempDir::new().expect("temp dir");
		unsafe {
			std::env::set_var(MOD_SNAPSHOT_CACHE_DIR_ENV, temp.path());
		}

		let mod_root = temp.path().join("9001");
		std::fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create mod root");
		std::fs::write(
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
				display_name: Some("schema-test".to_string()),
			},
			mod_id: "9001".to_string(),
			root_path: Some(mod_root.clone()),
			descriptor_path: Some(mod_root.join("descriptor.mod")),
			descriptor: Some(ModDescriptor {
				name: "schema-test".to_string(),
				path: None,
				tags: Vec::new(),
				dependencies: Vec::new(),
				version: None,
				remote_file_id: Some("9001".to_string()),
				supported_version: None,
			}),
			descriptor_error: None,
			files: Vec::new(),
		};

		let documents =
			super::discover_text_documents(mod_item.root_path.as_ref().expect("mod root"));
		let manifest_hash = super::semantic_manifest_hash(&documents);
		let cache_path = mod_snapshot_cache_file(
			"eu4",
			"1.0.0-test",
			analysis_rules_version(),
			"9001",
			manifest_hash,
		);
		store_mod_snapshot(
			&cache_path,
			&StoredModSemanticSnapshot {
				schema_version: MOD_SNAPSHOT_SCHEMA_VERSION - 1,
				game: "eu4".to_string(),
				game_version: "1.0.0-test".to_string(),
				analysis_rules_version: analysis_rules_version().to_string(),
				mod_identity: "9001".to_string(),
				manifest_hash,
				generated_by_cli_version: env!("CARGO_PKG_VERSION").to_string(),
				parsed_files: 0,
				parse_error_count: 0,
				parse_stats: ParseFamilyStats::default(),
				semantic_index: Default::default(),
			},
		);

		let filter = super::super::FileFilter::for_game(foch_core::domain::game::Game::EuropaUniversalis4);
		let loaded = load_or_build_mod_snapshot("eu4", Some("1.0.0-test"), &mod_item, &filter)
			.expect("rebuild snapshot");
		assert_eq!(loaded.parsed_files, 1);

		let rebuilt_path = mod_snapshot_cache_file(
			"eu4",
			"1.0.0-test",
			analysis_rules_version(),
			"9001",
			manifest_hash,
		);
		let rebuilt = super::load_mod_snapshot(&rebuilt_path).expect("load rebuilt snapshot");
		assert_eq!(rebuilt.schema_version, MOD_SNAPSHOT_SCHEMA_VERSION);
		assert!(
			rebuilt
				.semantic_index
				.definitions
				.first()
				.and_then(|definition| definition.param_contract.as_ref())
				.is_some()
		);

		unsafe {
			std::env::remove_var(MOD_SNAPSHOT_CACHE_DIR_ENV);
		}
	}
}
