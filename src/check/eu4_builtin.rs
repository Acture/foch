use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct BuiltinCatalog {
	reserved_keywords: Vec<String>,
	contextual_keywords: Vec<String>,
	alias_keywords: Vec<String>,
	builtin_triggers: Vec<BuiltinSymbol>,
	builtin_effects: Vec<BuiltinSymbol>,
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
}

static LOOKUP: OnceLock<BuiltinLookup> = OnceLock::new();

fn load_lookup() -> &'static BuiltinLookup {
	LOOKUP.get_or_init(|| {
		let raw = include_str!("data/eu4_builtin_catalog.json");
		let catalog: BuiltinCatalog = serde_json::from_str(raw)
			.expect("valid src/check/data/eu4_builtin_catalog.json catalog");

		BuiltinLookup {
			reserved: catalog.reserved_keywords.into_iter().collect(),
			contextual: catalog.contextual_keywords.into_iter().collect(),
			aliases: catalog.alias_keywords.into_iter().collect(),
			triggers: catalog
				.builtin_triggers
				.into_iter()
				.map(|item| item.name)
				.collect(),
			effects: catalog
				.builtin_effects
				.into_iter()
				.map(|item| item.name)
				.collect(),
		}
	})
}

pub fn is_reserved_keyword(key: &str) -> bool {
	load_lookup().reserved.contains(key)
}

pub fn is_contextual_keyword(key: &str) -> bool {
	load_lookup().contextual.contains(key)
}

pub fn is_alias_keyword(key: &str) -> bool {
	load_lookup().aliases.contains(key)
}

pub fn is_builtin_trigger(key: &str) -> bool {
	load_lookup().triggers.contains(key)
}

pub fn is_builtin_effect(key: &str) -> bool {
	load_lookup().effects.contains(key)
}

#[cfg(test)]
mod tests {
	use super::{
		is_alias_keyword, is_builtin_effect, is_builtin_trigger, is_contextual_keyword,
		is_reserved_keyword,
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
}
