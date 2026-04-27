use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::OnceLock;

pub const BUILTIN_CATALOG_RAW: &str = include_str!("../data/eu4_builtin_catalog.json");

#[derive(Debug, Deserialize)]
struct BuiltinCatalog {
	#[serde(default)]
	reserved_keywords: Vec<String>,
	#[serde(default)]
	contextual_keywords: Vec<String>,
	#[serde(default)]
	alias_keywords: Vec<String>,
	#[serde(default)]
	builtin_triggers: Vec<BuiltinSymbol>,
	#[serde(default)]
	builtin_effects: Vec<BuiltinSymbol>,
	#[serde(default)]
	builtin_scope_changers: Vec<BuiltinSymbol>,
	#[serde(default)]
	builtin_iterators: Vec<BuiltinSymbol>,
	#[serde(default)]
	builtin_special_blocks: Vec<BuiltinSymbol>,
	#[serde(default)]
	game_only_candidates: Vec<BuiltinSymbol>,
}

#[derive(Debug, Deserialize)]
struct BuiltinSymbol {
	name: String,
}

#[derive(Debug)]
struct BuiltinLookup {
	reserved: HashSet<String>,
	contextual: HashSet<String>,
	aliases: HashSet<String>,
	triggers: HashSet<String>,
	effects: HashSet<String>,
	scope_changers: HashSet<String>,
	iterators: HashSet<String>,
	special_blocks: HashSet<String>,
	game_only: HashSet<String>,
	reserved_list: Vec<String>,
	contextual_list: Vec<String>,
	aliases_list: Vec<String>,
	triggers_list: Vec<String>,
	effects_list: Vec<String>,
	scope_changers_list: Vec<String>,
	iterators_list: Vec<String>,
	special_blocks_list: Vec<String>,
}

static LOOKUP: OnceLock<BuiltinLookup> = OnceLock::new();
static BUILTIN_CATALOG_HASH: OnceLock<String> = OnceLock::new();

fn load_lookup() -> &'static BuiltinLookup {
	LOOKUP.get_or_init(|| {
		let catalog: BuiltinCatalog = serde_json::from_str(BUILTIN_CATALOG_RAW)
			.expect("valid crates/foch-language/src/data/eu4_builtin_catalog.json catalog");

		let mut reserved_list = catalog.reserved_keywords;
		reserved_list.sort();
		reserved_list.dedup();

		let mut contextual_list = catalog.contextual_keywords;
		contextual_list.sort();
		contextual_list.dedup();

		let mut aliases_list = catalog.alias_keywords;
		aliases_list.sort();
		aliases_list.dedup();

		let mut triggers_list: Vec<String> = catalog
			.builtin_triggers
			.into_iter()
			.map(|item| item.name)
			.collect();
		triggers_list.sort();
		triggers_list.dedup();

		let mut effects_list: Vec<String> = catalog
			.builtin_effects
			.into_iter()
			.map(|item| item.name)
			.collect();
		effects_list.sort();
		effects_list.dedup();

		let mut scope_changers_list: Vec<String> = catalog
			.builtin_scope_changers
			.into_iter()
			.map(|item| item.name)
			.collect();
		scope_changers_list.sort();
		scope_changers_list.dedup();

		let mut iterators_list: Vec<String> = catalog
			.builtin_iterators
			.into_iter()
			.map(|item| item.name)
			.collect();
		iterators_list.sort();
		iterators_list.dedup();

		let mut special_blocks_list: Vec<String> = catalog
			.builtin_special_blocks
			.into_iter()
			.map(|item| item.name)
			.collect();
		special_blocks_list.sort();
		special_blocks_list.dedup();

		// Clausewitz keyword/builtin matching is case-insensitive in the
		// EU4 engine: vanilla writes `if`, `limit`, `OR`, `AND`, `NOT`,
		// `ai`, `religion` etc. with one canonical casing, but mods
		// freely use `IF`, `LIMIT`, `Or`, `And`, `AI`, `RELIGION`, …
		// Lowercase every catalog entry once at load time and lowercase
		// the lookup key so all variants resolve.
		fn to_lower_set(items: &[String]) -> HashSet<String> {
			items.iter().map(|s| s.to_ascii_lowercase()).collect()
		}

		let game_only: HashSet<String> = catalog
			.game_only_candidates
			.into_iter()
			.map(|item| item.name.to_ascii_lowercase())
			.collect();

		BuiltinLookup {
			reserved: to_lower_set(&reserved_list),
			contextual: to_lower_set(&contextual_list),
			aliases: to_lower_set(&aliases_list),
			triggers: to_lower_set(&triggers_list),
			effects: to_lower_set(&effects_list),
			scope_changers: to_lower_set(&scope_changers_list),
			iterators: to_lower_set(&iterators_list),
			special_blocks: to_lower_set(&special_blocks_list),
			game_only,
			reserved_list,
			contextual_list,
			aliases_list,
			triggers_list,
			effects_list,
			scope_changers_list,
			iterators_list,
			special_blocks_list,
		}
	})
}

fn matches_lower(set: &HashSet<String>, key: &str) -> bool {
	if set.contains(key) {
		return true;
	}
	if key.bytes().any(|b| b.is_ascii_uppercase()) {
		set.contains(&key.to_ascii_lowercase())
	} else {
		false
	}
}

pub fn is_reserved_keyword(key: &str) -> bool {
	matches_lower(&load_lookup().reserved, key)
}

pub fn is_contextual_keyword(key: &str) -> bool {
	matches_lower(&load_lookup().contextual, key)
}

pub fn is_alias_keyword(key: &str) -> bool {
	matches_lower(&load_lookup().aliases, key)
}

pub fn is_builtin_trigger(key: &str) -> bool {
	matches_lower(&load_lookup().triggers, key)
}

pub fn is_builtin_effect(key: &str) -> bool {
	matches_lower(&load_lookup().effects, key)
}

pub fn is_builtin_scope_changer(key: &str) -> bool {
	matches_lower(&load_lookup().scope_changers, key)
}

pub fn is_builtin_iterator(key: &str) -> bool {
	matches_lower(&load_lookup().iterators, key)
}

pub fn is_builtin_special_block(key: &str) -> bool {
	matches_lower(&load_lookup().special_blocks, key)
}

pub fn is_game_only_candidate(key: &str) -> bool {
	matches_lower(&load_lookup().game_only, key)
}

pub fn reserved_keywords() -> &'static [String] {
	&load_lookup().reserved_list
}

pub fn contextual_keywords() -> &'static [String] {
	&load_lookup().contextual_list
}

pub fn alias_keywords() -> &'static [String] {
	&load_lookup().aliases_list
}

pub fn builtin_trigger_names() -> &'static [String] {
	&load_lookup().triggers_list
}

pub fn builtin_effect_names() -> &'static [String] {
	&load_lookup().effects_list
}

pub fn builtin_scope_changer_names() -> &'static [String] {
	&load_lookup().scope_changers_list
}

pub fn builtin_iterator_names() -> &'static [String] {
	&load_lookup().iterators_list
}

pub fn builtin_special_block_names() -> &'static [String] {
	&load_lookup().special_blocks_list
}

pub fn builtin_catalog_hash() -> &'static str {
	BUILTIN_CATALOG_HASH.get_or_init(|| {
		let digest = Sha256::digest(BUILTIN_CATALOG_RAW.as_bytes());
		let mut out = String::with_capacity(16);
		for byte in digest.iter().take(8) {
			out.push_str(&format!("{byte:02x}"));
		}
		out
	})
}

#[cfg(test)]
mod tests {
	use super::{
		builtin_catalog_hash, is_alias_keyword, is_builtin_effect, is_builtin_iterator,
		is_builtin_scope_changer, is_builtin_special_block, is_builtin_trigger,
		is_contextual_keyword, is_reserved_keyword,
	};

	#[test]
	fn keyword_sets_are_loaded() {
		assert!(is_reserved_keyword("potential"));
		assert!(is_reserved_keyword("allow"));
		assert!(is_contextual_keyword("hidden_effect"));
		assert!(is_alias_keyword("ROOT"));
	}

	#[test]
	fn builtin_symbols_include_trigger_and_effect_examples() {
		assert!(is_builtin_trigger("num_of_cities"));
		assert!(is_builtin_trigger("has_country_flag"));
		assert!(is_builtin_effect("add_country_modifier"));
	}

	#[test]
	fn builtin_symbol_taxonomy_includes_scope_and_iterator_examples() {
		assert!(is_builtin_scope_changer("capital_scope"));
		assert!(is_builtin_scope_changer("owner"));
		assert!(is_builtin_iterator("all_core_province"));
		assert!(is_builtin_iterator("all_owned_province"));
		assert!(is_builtin_special_block("possible"));
		assert!(is_builtin_special_block("exclude_from_progress"));
		assert!(!builtin_catalog_hash().is_empty());
	}

	#[test]
	fn builtin_lookup_is_case_insensitive() {
		// Clausewitz keyword matching is case-insensitive in EU4. Mods
		// frequently write builtin triggers/effects/keywords with mixed
		// or upper casing (e.g. `LIMIT = { ... }`, `AI = no`, `AND`,
		// `RELIGION`). The analyzer must treat them the same as their
		// canonical lowercase forms.
		assert!(is_reserved_keyword("LIMIT"));
		assert!(is_reserved_keyword("Limit"));
		assert!(is_reserved_keyword("AND"));
		assert!(is_builtin_trigger("AI"));
		assert!(is_builtin_trigger("Ai"));
		assert!(is_builtin_trigger("RELIGION"));
		assert!(is_builtin_effect("ADD_COUNTRY_MODIFIER"));
	}
}
