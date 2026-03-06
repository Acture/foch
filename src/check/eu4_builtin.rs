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
	reserved_list: Vec<String>,
	contextual_list: Vec<String>,
	aliases_list: Vec<String>,
	triggers_list: Vec<String>,
	effects_list: Vec<String>,
}

static LOOKUP: OnceLock<BuiltinLookup> = OnceLock::new();

fn load_lookup() -> &'static BuiltinLookup {
	LOOKUP.get_or_init(|| {
		let raw = include_str!("data/eu4_builtin_catalog.json");
		let catalog: BuiltinCatalog = serde_json::from_str(raw)
			.expect("valid src/check/data/eu4_builtin_catalog.json catalog");

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

		BuiltinLookup {
			reserved: reserved_list.iter().cloned().collect(),
			contextual: contextual_list.iter().cloned().collect(),
			aliases: aliases_list.iter().cloned().collect(),
			triggers: triggers_list.iter().cloned().collect(),
			effects: effects_list.iter().cloned().collect(),
			reserved_list,
			contextual_list,
			aliases_list,
			triggers_list,
			effects_list,
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
