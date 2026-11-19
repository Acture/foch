//! Coefficient resolution: which multiplier applies to one subject's tribute.
//!
//! Subject kinds are the script keys in `common/subject_types`, not a fixed
//! enum. A missing key walks `copy_from` parents (cycle-safe) and then the
//! `default` key before the global percent.

use crate::subject_types::SubjectTypeGraph;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The effective runtime configuration. Built from the manifest config
/// (host-delivered) merged with `tribute-config.json` from the data directory;
/// the slider panel mutates a copy and swaps it in atomically.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TributeConfig {
	pub enabled: bool,
	/// Percent for subjects of any kind. Replaced, not stacked, by kind and
	/// tag values.
	pub global_percent: i64,
	/// Percent per subject-type key (`vassal`, `core_eyalet`, a mod key, or
	/// `default`). A missing key walks `copy_from`.
	pub kind_percent: BTreeMap<String, i64>,
	/// Percent per subject tag (three-letter tag, uppercase); replaces the
	/// kind chain.
	pub tag_percent: BTreeMap<String, i64>,
}

impl TributeConfig {
	/// A const-constructible all-100% configuration, so the process-wide
	/// RwLock can be initialized statically.
	pub const ONCE: Self = Self {
		enabled: true,
		global_percent: 100,
		kind_percent: BTreeMap::new(),
		tag_percent: BTreeMap::new(),
	};
}

impl Default for TributeConfig {
	fn default() -> Self {
		Self {
			enabled: true,
			global_percent: 100,
			kind_percent: BTreeMap::new(),
			tag_percent: BTreeMap::new(),
		}
	}
}

impl TributeConfig {
	/// The multiplier for one subject.
	///
	/// Priority is `tag_percent[tag]`, then `kind_percent` of the exact key,
	/// then each `copy_from` parent, then `kind_percent["default"]` when the
	/// walk did not already visit that key, then `global_percent`. Values
	/// replace; they do not stack. The settlement detour calls this once per
	/// subject per month.
	pub fn coefficient(&self, tag: &str, kind: &str, types: &SubjectTypeGraph) -> f64 {
		if !self.enabled {
			return 1.0;
		}
		if !tag.is_empty()
			&& let Some(percent) = self.tag_percent.get(tag)
		{
			return scale(*percent);
		}
		let chain = types.ancestry(kind);
		for key in &chain {
			if let Some(percent) = self.kind_percent.get(key) {
				return scale(*percent);
			}
		}
		if !chain.iter().any(|key| key == "default")
			&& let Some(percent) = self.kind_percent.get("default")
		{
			return scale(*percent);
		}
		scale(self.global_percent)
	}
}

fn scale(percent: i64) -> f64 {
	percent as f64 / 100.0
}

#[cfg(test)]
mod tests {
	use super::*;

	fn chain() -> SubjectTypeGraph {
		SubjectTypeGraph::from_script(
			"\
default = {}
vassal = { copy_from = default }
eyalet = { copy_from = vassal }
core_eyalet = { copy_from = eyalet }
beylerbey = { copy_from = core_eyalet }
",
		)
		.unwrap()
	}

	fn config() -> TributeConfig {
		let mut cfg = TributeConfig {
			global_percent: 80,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("tributary_state".into(), 50);
		cfg.kind_percent.insert("colony".into(), 120);
		cfg.tag_percent.insert("MUG".into(), 10);
		cfg
	}

	#[test]
	fn tag_replaces_kind_replaces_global() {
		let cfg = config();
		let types = SubjectTypeGraph::EMPTY;
		assert!((cfg.coefficient("MUG", "tributary_state", &types) - 0.10).abs() < 1e-9);
		assert!((cfg.coefficient("KOR", "tributary_state", &types) - 0.50).abs() < 1e-9);
		assert!((cfg.coefficient("KOR", "colony", &types) - 1.20).abs() < 1e-9);
		assert!((cfg.coefficient("KOR", "vassal", &types) - 0.80).abs() < 1e-9);
	}

	#[test]
	fn disabled_means_passthrough() {
		let mut cfg = config();
		cfg.enabled = false;
		assert!(
			(cfg.coefficient("MUG", "tributary_state", &SubjectTypeGraph::EMPTY) - 1.0).abs()
				< 1e-9
		);
	}

	#[test]
	fn kind_keys_are_open_strings() {
		for kind in [
			"junior_partner",
			"vassal",
			"march",
			"client_vassal",
			"colony",
			"tributary_state",
			"daimyo_vassal",
			"appanage",
			"core_eyalet",
			"beylerbey",
		] {
			let mut cfg = TributeConfig::default();
			cfg.kind_percent.insert(kind.into(), 33);
			assert!(
				(cfg.coefficient("XXX", kind, &SubjectTypeGraph::EMPTY) - 0.33).abs() < 1e-9,
				"{kind}"
			);
		}
	}

	#[test]
	fn core_eyalet_inherits_eyalet_then_vassal_then_default() {
		let types = chain();
		let mut cfg = TributeConfig {
			global_percent: 10,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("eyalet".into(), 70);
		cfg.kind_percent.insert("vassal".into(), 50);
		cfg.kind_percent.insert("default".into(), 30);
		assert!((cfg.coefficient("AAA", "core_eyalet", &types) - 0.70).abs() < 1e-9);

		cfg.kind_percent.remove("eyalet");
		assert!((cfg.coefficient("AAA", "core_eyalet", &types) - 0.50).abs() < 1e-9);

		cfg.kind_percent.remove("vassal");
		assert!((cfg.coefficient("AAA", "core_eyalet", &types) - 0.30).abs() < 1e-9);

		cfg.kind_percent.remove("default");
		assert!((cfg.coefficient("AAA", "core_eyalet", &types) - 0.10).abs() < 1e-9);
	}

	#[test]
	fn exact_key_and_tag_beat_the_parent_chain() {
		let types = chain();
		let mut cfg = TributeConfig {
			global_percent: 10,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("vassal".into(), 50);
		cfg.kind_percent.insert("beylerbey".into(), 80);
		cfg.tag_percent.insert("BYZ".into(), 15);
		assert!((cfg.coefficient("TRE", "beylerbey", &types) - 0.80).abs() < 1e-9);
		assert!((cfg.coefficient("BYZ", "beylerbey", &types) - 0.15).abs() < 1e-9);
	}

	#[test]
	fn default_applies_when_the_chain_never_names_it() {
		let types = SubjectTypeGraph::from_script("custom = {}\n").unwrap();
		let mut cfg = TributeConfig {
			global_percent: 80,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("default".into(), 40);
		assert!((cfg.coefficient("KOR", "custom", &types) - 0.40).abs() < 1e-9);
		cfg.kind_percent.insert("custom".into(), 25);
		assert!((cfg.coefficient("KOR", "custom", &types) - 0.25).abs() < 1e-9);
	}

	#[test]
	fn a_cycle_uses_the_first_set_percent_and_stops() {
		let types = SubjectTypeGraph::from_script(
			"a = { copy_from = b }\nb = { copy_from = c }\nc = { copy_from = a }\n",
		)
		.unwrap();
		let mut cfg = TributeConfig {
			global_percent: 80,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("c".into(), 40);
		assert!((cfg.coefficient("KOR", "a", &types) - 0.40).abs() < 1e-9);
		cfg.kind_percent.remove("c");
		cfg.kind_percent.insert("default".into(), 15);
		assert!((cfg.coefficient("KOR", "a", &types) - 0.15).abs() < 1e-9);
		cfg.kind_percent.clear();
		assert!((cfg.coefficient("KOR", "a", &types) - 0.80).abs() < 1e-9);
		assert_eq!(types.ancestry("a"), vec!["a", "b", "c"]);
	}

	#[test]
	fn runtime_config_round_trips_as_json() {
		let cfg = config();
		let text = serde_json::to_string(&cfg).unwrap();
		let back: TributeConfig = serde_json::from_str(&text).unwrap();
		assert_eq!(cfg, back);
	}
}
