use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use foch_core::model::{ScopeKind, ScopeType, base_scope};
use foch_cwt::CwtSchemaGraph;
use foch_language::analyzer::parser::{AstStatement, AstValue, parse_clausewitz_file};
use foch_language::analyzer::semantic_index::{
	cwt_iterator_scope_type, cwt_scope_changer_target_type, cwt_special_block_scope_kind,
};
use walkdir::WalkDir;

const PROVINCE_ITERATOR_KEYS: &[&str] = &[
	"all_core_province",
	"all_neighbor_province",
	"all_owned_province",
	"all_owned_province_cumulative",
	"all_province",
	"all_province_in_state",
	"all_state_province",
	"all_trade_node_member_province",
	"any_core_province",
	"any_empty_neighbor_province",
	"any_friendly_coast_border_province",
	"any_heretic_province",
	"any_owned_province",
	"any_province",
	"any_province_in_state",
	"any_trade_node_member_province",
	"every_claimed_province",
	"every_core_province",
	"every_empty_neighbor_province",
	"every_heretic_province",
	"every_neighbor_province",
	"every_owned_province",
	"every_province",
	"every_province_in_state",
	"every_trade_node_member_province",
	"every_tribal_land_province",
	"random_area_province",
	"random_core_province",
	"random_empty_neighbor_province",
	"random_heretic_province",
	"random_neighbor_province",
	"random_owned_province",
	"random_province",
	"random_province_in_state",
	"random_trade_node_member_province",
	"every_neighbor_sea_zone",
	"any_tribal_land",
];

const COUNTRY_ITERATOR_KEYS: &[&str] = &[
	"all_ally",
	"all_countries_including_self",
	"all_country",
	"all_elector",
	"all_federation_members",
	"all_known_country",
	"all_neighbor_country",
	"all_rival_country",
	"all_subject_country",
	"all_trade_node_member_country",
	"all_war_enemy_countries",
	"any_ally",
	"any_core_country",
	"any_country",
	"any_country_active_in_node",
	"any_elector",
	"any_enemy_country",
	"any_great_power",
	"any_hired_mercenary_company",
	"any_known_country",
	"any_neighbor_country",
	"any_other_great_power",
	"any_privateering_country",
	"any_rival_country",
	"any_trade_node_member_country",
	"any_war_enemy_country",
	"every_ally",
	"every_core_country",
	"every_country",
	"every_country_including_inactive",
	"every_elector",
	"every_enemy_country",
	"every_federation_member",
	"every_known_country",
	"every_neighbor_country",
	"every_rival_country",
	"every_subject_country",
	"every_trade_node_member_country",
	"every_war_enemy_country",
	"random_ally",
	"random_core_country",
	"random_country",
	"random_elector",
	"random_enemy_country",
	"random_hired_mercenary_company",
	"random_known_country",
	"random_neighbor_country",
	"random_privateering_country",
	"random_rival_country",
	"random_subject_country",
	"random_war_enemy_country",
];

const PROVINCE_SCOPE_CHANGER_KEYS: &[&str] = &[
	"capital_scope",
	"sea_zone",
	"area_for_scope_province",
	"region_for_scope_province",
];

const COUNTRY_SCOPE_CHANGER_KEYS: &[&str] = &[
	"owner",
	"controller",
	"attacker_leader",
	"defender_leader",
	"emperor",
	"colonial_parent",
	"other_overlord",
	"same_overlord",
	"strongest_trade_power",
	"unit_owner",
];

const TRIGGER_SPECIAL_BLOCK_KEYS: &[&str] = &[
	"possible",
	"visible",
	"happened",
	"provinces_to_highlight",
	"exclude_from_progress",
];

#[derive(Debug)]
struct OptionScopeMismatch {
	key: String,
	legacy: Option<ScopeType>,
	cwt: Option<ScopeType>,
	path: Option<PathBuf>,
	line: Option<usize>,
}

#[derive(Debug)]
struct ScopeKindMismatch {
	key: String,
	legacy: ScopeKind,
	cwt: ScopeKind,
	path: Option<PathBuf>,
	line: Option<usize>,
}

#[test]
#[ignore = "run manually as a parity gate before swapping production callers"]
fn keyword_tables_match_cwt_helpers() {
	let graph = schema_graph();

	let iterator_mismatches = iterator_keys()
		.into_iter()
		.filter_map(|key| {
			let legacy = legacy_iterator_scope_type(key);
			let cwt = cwt_iterator_scope_type(graph, key);
			(legacy != cwt).then(|| OptionScopeMismatch {
				key: key.to_string(),
				legacy,
				cwt,
				path: None,
				line: None,
			})
		})
		.collect::<Vec<_>>();
	let iterator_summary = option_scope_summary(
		"iterator keyword diff",
		iterator_keys().len(),
		&iterator_mismatches,
	);

	let scope_changer_mismatches = scope_changer_keys()
		.into_iter()
		.filter_map(|key| {
			let legacy = legacy_scope_changer_target_type(key);
			let cwt = cwt_scope_changer_target_type(graph, key);
			(legacy != cwt).then(|| OptionScopeMismatch {
				key: key.to_string(),
				legacy,
				cwt,
				path: None,
				line: None,
			})
		})
		.collect::<Vec<_>>();
	let scope_changer_summary = option_scope_summary(
		"scope_changer keyword diff",
		scope_changer_keys().len(),
		&scope_changer_mismatches,
	);

	let special_block_mismatches = special_block_keys()
		.into_iter()
		.filter_map(|key| {
			let legacy = legacy_special_block_scope_kind(key);
			let cwt = cwt_special_block_scope_kind(graph, key);
			(legacy != cwt).then(|| ScopeKindMismatch {
				key: key.to_string(),
				legacy,
				cwt,
				path: None,
				line: None,
			})
		})
		.collect::<Vec<_>>();
	let special_block_summary = scope_kind_summary(
		"special_block keyword diff",
		special_block_keys().len(),
		&special_block_mismatches,
	);

	println!("{}", iterator_summary);
	println!("{}", scope_changer_summary);
	println!("{}", special_block_summary);

	let mut failures = Vec::new();
	if !iterator_mismatches.is_empty() {
		failures.push(format!(
			"iterator keyword diff mismatches:\n{}",
			format_option_scope_mismatches(&iterator_mismatches)
		));
	}
	if !scope_changer_mismatches.is_empty() {
		failures.push(format!(
			"scope_changer keyword diff mismatches:\n{}",
			format_option_scope_mismatches(&scope_changer_mismatches)
		));
	}
	if !special_block_mismatches.is_empty() {
		failures.push(format!(
			"special_block keyword diff mismatches:\n{}",
			format_scope_kind_mismatches(&special_block_mismatches)
		));
	}
	assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
#[ignore = "requires a local EU4 install and is run manually as an acceptance gate"]
fn vanilla_corpus_matches_cwt_helpers() {
	let graph = schema_graph();
	let eu4_root = eu4_root();
	if !eu4_root.is_dir() {
		println!("EU4 install not found at {}", eu4_root.display());
		return;
	}

	let mut files_checked = 0usize;
	let mut files_with_parse_diagnostics = 0usize;
	let mut keys_checked = 0usize;
	let mut iterator_mismatches = Vec::new();
	let mut scope_changer_mismatches = Vec::new();
	let mut special_block_mismatches = Vec::new();

	for entry in WalkDir::new(&eu4_root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("txt"))
	{
		files_checked += 1;
		let parsed = parse_clausewitz_file(entry.path());
		if !parsed.diagnostics.is_empty() {
			files_with_parse_diagnostics += 1;
		}
		walk_keys(entry.path(), &parsed.ast.statements, &mut |key, line| {
			keys_checked += 1;

			let legacy_iterator = legacy_iterator_scope_type(key);
			let cwt_iterator = cwt_iterator_scope_type(graph, key);
			if legacy_iterator != cwt_iterator {
				iterator_mismatches.push(OptionScopeMismatch {
					key: key.to_string(),
					legacy: legacy_iterator,
					cwt: cwt_iterator,
					path: Some(relative_to(&eu4_root, entry.path())),
					line: Some(line),
				});
			}

			let legacy_scope_changer = legacy_scope_changer_target_type(key);
			let cwt_scope_changer = cwt_scope_changer_target_type(graph, key);
			if legacy_scope_changer != cwt_scope_changer {
				scope_changer_mismatches.push(OptionScopeMismatch {
					key: key.to_string(),
					legacy: legacy_scope_changer,
					cwt: cwt_scope_changer,
					path: Some(relative_to(&eu4_root, entry.path())),
					line: Some(line),
				});
			}

			let legacy_special_block = legacy_special_block_scope_kind(key);
			let cwt_special_block = cwt_special_block_scope_kind(graph, key);
			if legacy_special_block != cwt_special_block {
				special_block_mismatches.push(ScopeKindMismatch {
					key: key.to_string(),
					legacy: legacy_special_block,
					cwt: cwt_special_block,
					path: Some(relative_to(&eu4_root, entry.path())),
					line: Some(line),
				});
			}
		});
	}

	println!(
		"vanilla corpus: files={} parse_diagnostic_files={} keys={}",
		files_checked, files_with_parse_diagnostics, keys_checked
	);
	let iterator_summary =
		option_scope_summary("vanilla iterator diff", keys_checked, &iterator_mismatches);
	let scope_changer_summary = option_scope_summary(
		"vanilla scope_changer diff",
		keys_checked,
		&scope_changer_mismatches,
	);
	let special_block_summary = scope_kind_summary(
		"vanilla special_block diff",
		keys_checked,
		&special_block_mismatches,
	);
	println!("{}", iterator_summary);
	println!("{}", scope_changer_summary);
	println!("{}", special_block_summary);

	let mut failures = Vec::new();
	if !iterator_mismatches.is_empty() {
		failures.push(format!(
			"vanilla iterator diff mismatches:\n{}",
			format_option_scope_mismatches(&iterator_mismatches)
		));
	}
	if !scope_changer_mismatches.is_empty() {
		failures.push(format!(
			"vanilla scope_changer diff mismatches:\n{}",
			format_option_scope_mismatches(&scope_changer_mismatches)
		));
	}
	if !special_block_mismatches.is_empty() {
		failures.push(format!(
			"vanilla special_block diff mismatches:\n{}",
			format_scope_kind_mismatches(&special_block_mismatches)
		));
	}
	assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn schema_graph() -> &'static CwtSchemaGraph {
	ensure_base_scopes_initialized();
	static GRAPH: OnceLock<CwtSchemaGraph> = OnceLock::new();
	GRAPH.get_or_init(|| {
		CwtSchemaGraph::from_directory(&vendor_schema_dir()).expect("load vendored cwtools schema")
	})
}

fn ensure_base_scopes_initialized() {
	if !base_scope::is_initialized() {
		base_scope::init_base_scopes("country", "province");
	}
}

fn vendor_schema_dir() -> PathBuf {
	workspace_root().join("vendor").join("cwtools-eu4-config")
}

fn workspace_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("crates dir")
		.parent()
		.expect("workspace root")
		.to_path_buf()
}

fn eu4_root() -> PathBuf {
	std::env::var_os("EU4_ROOT")
		.map(PathBuf::from)
		.or_else(|| {
			std::env::var_os("HOME").map(|home| {
				PathBuf::from(home)
					.join("Library")
					.join("Application Support")
					.join("Steam")
					.join("steamapps")
					.join("common")
					.join("Europa Universalis IV")
			})
		})
		.expect("resolve EU4 root")
}

fn iterator_keys() -> Vec<&'static str> {
	PROVINCE_ITERATOR_KEYS
		.iter()
		.chain(COUNTRY_ITERATOR_KEYS.iter())
		.copied()
		.collect()
}

fn scope_changer_keys() -> Vec<&'static str> {
	PROVINCE_SCOPE_CHANGER_KEYS
		.iter()
		.chain(COUNTRY_SCOPE_CHANGER_KEYS.iter())
		.copied()
		.collect()
}

fn special_block_keys() -> Vec<&'static str> {
	TRIGGER_SPECIAL_BLOCK_KEYS.to_vec()
}

fn legacy_iterator_scope_type(key: &str) -> Option<ScopeType> {
	if PROVINCE_ITERATOR_KEYS.contains(&key) {
		Some(base_scope::province())
	} else if COUNTRY_ITERATOR_KEYS.contains(&key) {
		Some(base_scope::country())
	} else {
		None
	}
}

fn legacy_scope_changer_target_type(key: &str) -> Option<ScopeType> {
	if PROVINCE_SCOPE_CHANGER_KEYS.contains(&key) {
		Some(base_scope::province())
	} else if COUNTRY_SCOPE_CHANGER_KEYS.contains(&key) {
		Some(base_scope::country())
	} else {
		None
	}
}

fn legacy_special_block_scope_kind(key: &str) -> ScopeKind {
	if TRIGGER_SPECIAL_BLOCK_KEYS.contains(&key) {
		ScopeKind::Trigger
	} else {
		ScopeKind::Block
	}
}

fn walk_keys(path: &Path, statements: &[AstStatement], visit: &mut impl FnMut(&str, usize)) {
	for statement in statements {
		walk_statement(path, statement, visit);
	}
}

fn walk_statement(path: &Path, statement: &AstStatement, visit: &mut impl FnMut(&str, usize)) {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} => {
			visit(key, key_span.start.line);
			walk_value(path, value, visit);
		}
		AstStatement::Item { value, .. } => walk_value(path, value, visit),
		AstStatement::Comment { .. } => {}
	}
}

fn walk_value(path: &Path, value: &AstValue, visit: &mut impl FnMut(&str, usize)) {
	if let AstValue::Block { items, .. } = value {
		walk_keys(path, items, visit);
	}
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
	path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn option_scope_summary(label: &str, total: usize, mismatches: &[OptionScopeMismatch]) -> String {
	format!(
		"{}: matched={} mismatched={}",
		label,
		total.saturating_sub(mismatches.len()),
		mismatches.len()
	)
}

fn scope_kind_summary(label: &str, total: usize, mismatches: &[ScopeKindMismatch]) -> String {
	format!(
		"{}: matched={} mismatched={}",
		label,
		total.saturating_sub(mismatches.len()),
		mismatches.len()
	)
}

fn format_option_scope_mismatches(mismatches: &[OptionScopeMismatch]) -> String {
	mismatches
		.iter()
		.take(100)
		.map(|mismatch| {
			format!(
				"{}{} hand={} cwt={}",
				mismatch.key,
				format_location(mismatch.path.as_deref(), mismatch.line),
				format_scope_option(mismatch.legacy),
				format_scope_option(mismatch.cwt),
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn format_scope_kind_mismatches(mismatches: &[ScopeKindMismatch]) -> String {
	mismatches
		.iter()
		.take(100)
		.map(|mismatch| {
			format!(
				"{}{} hand={:?} cwt={:?}",
				mismatch.key,
				format_location(mismatch.path.as_deref(), mismatch.line),
				mismatch.legacy,
				mismatch.cwt,
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn format_location(path: Option<&Path>, line: Option<usize>) -> String {
	match (path, line) {
		(Some(path), Some(line)) => format!(" @ {}:{}", path.display(), line),
		(Some(path), None) => format!(" @ {}", path.display()),
		_ => String::new(),
	}
}

fn format_scope_option(scope: Option<ScopeType>) -> String {
	match scope {
		Some(scope) => scope.name().to_string(),
		None => "None".to_string(),
	}
}
