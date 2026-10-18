use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::OnceLock;

pub const BUILTIN_CATALOG_RAW: &str = include_str!("data/eu4_builtin_catalog.json");

#[derive(Debug, Deserialize)]
struct BuiltinCatalog {
	#[serde(default)]
	reserved_keywords: Vec<String>,
	#[serde(default)]
	contextual_keywords: Vec<String>,
	#[serde(default)]
	alias_keywords: Vec<String>,
	base_scopes: BuiltinBaseScopes,
}

#[derive(Debug, Deserialize)]
struct BuiltinBaseScopes {
	country: String,
	province: String,
}

#[derive(Debug)]
struct BuiltinLookup {
	reserved: HashSet<String>,
	contextual: HashSet<String>,
	aliases: HashSet<String>,
	base_scopes: BuiltinBaseScopes,
	reserved_list: Vec<String>,
	contextual_list: Vec<String>,
	aliases_list: Vec<String>,
}

static LOOKUP: OnceLock<BuiltinLookup> = OnceLock::new();
static BUILTIN_CATALOG_HASH: OnceLock<String> = OnceLock::new();

fn load_lookup() -> &'static BuiltinLookup {
	LOOKUP.get_or_init(|| {
		let catalog: BuiltinCatalog = serde_json::from_str(BUILTIN_CATALOG_RAW)
			.expect("valid src/game/eu4/base/data/eu4_builtin_catalog.json catalog");

		let mut reserved_list = catalog.reserved_keywords;
		let base_scopes = catalog.base_scopes;
		reserved_list.sort();
		reserved_list.dedup();

		let mut contextual_list = catalog.contextual_keywords;
		contextual_list.sort();
		contextual_list.dedup();

		let mut aliases_list = catalog.alias_keywords;
		aliases_list.sort();
		aliases_list.dedup();

		// Clausewitz keyword matching is case-insensitive in the EU4 engine:
		// vanilla writes `if`, `limit`, `OR` with one canonical casing, but
		// mods freely use `IF`, `LIMIT`, `Or`. Lowercase every catalog entry
		// once at load time and lowercase the lookup key so all variants
		// resolve.
		fn to_lower_set(items: &[String]) -> HashSet<String> {
			items.iter().map(|s| s.to_ascii_lowercase()).collect()
		}

		BuiltinLookup {
			reserved: to_lower_set(&reserved_list),
			contextual: to_lower_set(&contextual_list),
			aliases: to_lower_set(&aliases_list),
			base_scopes,
			reserved_list,
			contextual_list,
			aliases_list,
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

pub fn reserved_keywords() -> &'static [String] {
	&load_lookup().reserved_list
}

pub fn contextual_keywords() -> &'static [String] {
	&load_lookup().contextual_list
}

pub fn alias_keywords() -> &'static [String] {
	&load_lookup().aliases_list
}

pub fn builtin_base_scope_names() -> (&'static str, &'static str) {
	let base_scopes = &load_lookup().base_scopes;
	(base_scopes.country.as_str(), base_scopes.province.as_str())
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
		builtin_catalog_hash, is_alias_keyword, is_contextual_keyword, is_reserved_keyword,
	};

	#[test]
	fn keyword_sets_are_loaded() {
		assert!(is_reserved_keyword("potential"));
		assert!(is_reserved_keyword("allow"));
		assert!(is_contextual_keyword("hidden_effect"));
		assert!(is_alias_keyword("ROOT"));
		assert!(!builtin_catalog_hash().is_empty());
	}

	#[test]
	fn keyword_lookup_is_case_insensitive() {
		// Clausewitz keyword matching is case-insensitive in EU4. Mods
		// frequently write keywords with mixed or upper casing (e.g.
		// `LIMIT = { ... }`, `AND`).
		assert!(is_reserved_keyword("LIMIT"));
		assert!(is_reserved_keyword("Limit"));
		assert!(is_reserved_keyword("AND"));
	}
}
