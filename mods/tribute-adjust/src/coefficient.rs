//! Coefficient resolution: which multiplier applies to one subject's tribute.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The subject kinds EU4 distinguishes on the subject side of the relation.
/// The discriminant read in the detour maps onto this enum; the order here is
/// our own, the mapping lives in the signature table.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum SubjectKind {
	JuniorPartner,
	Vassal,
	March,
	ClientState,
	Colony,
	Tributary,
	Daimyo,
	Appanage,
	Dominion,
}

impl SubjectKind {
	pub const ALL: [SubjectKind; 9] = [
		SubjectKind::JuniorPartner,
		SubjectKind::Vassal,
		SubjectKind::March,
		SubjectKind::ClientState,
		SubjectKind::Colony,
		SubjectKind::Tributary,
		SubjectKind::Daimyo,
		SubjectKind::Appanage,
		SubjectKind::Dominion,
	];

	/// Manifest / config file spelling, stable for the runtime config format.
	pub fn key(self) -> &'static str {
		match self {
			SubjectKind::JuniorPartner => "junior_partner",
			SubjectKind::Vassal => "vassal",
			SubjectKind::March => "march",
			SubjectKind::ClientState => "client_state",
			SubjectKind::Colony => "colony",
			SubjectKind::Tributary => "tributary",
			SubjectKind::Daimyo => "daimyo",
			SubjectKind::Appanage => "appanage",
			SubjectKind::Dominion => "dominion",
		}
	}
}

/// The effective runtime configuration. Built from the manifest config
/// (host-delivered) merged with `tribute-config.json` from the data directory;
/// the slider panel mutates a copy and swaps it in atomically.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TributeConfig {
	pub enabled: bool,
	/// Percent for subjects of any kind. Replaced, not stacked, by kind and
	/// tag values.
	pub global_percent: i64,
	/// Percent per subject kind; a missing kind falls back to `global_percent`.
	pub kind_percent: BTreeMap<String, i64>,
	/// Percent per subject tag (three-letter tag, uppercase); replaces the
	/// kind value.
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
	/// The multiplier for one subject: tag replaces kind replaces global.
	/// Clamp is not applied here; the panel and the config reader clamp on
	/// write so the hot path stays branch-light. The settlement detour calls
	/// this once per subject per month.
	#[allow(dead_code)] // until the signature table lands and detour::attach calls it
	pub fn coefficient(&self, tag: &str, kind: SubjectKind) -> f64 {
		if !self.enabled {
			return 1.0;
		}
		let percent = self
			.tag_percent
			.get(tag)
			.or_else(|| self.kind_percent.get(kind.key()))
			.copied()
			.unwrap_or(self.global_percent);
		percent as f64 / 100.0
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn config() -> TributeConfig {
		let mut cfg = TributeConfig {
			global_percent: 80,
			..TributeConfig::default()
		};
		cfg.kind_percent.insert("tributary".into(), 50);
		cfg.kind_percent.insert("colony".into(), 120);
		cfg.tag_percent.insert("MUG".into(), 10);
		cfg
	}

	#[test]
	fn tag_replaces_kind_replaces_global() {
		let cfg = config();
		assert!((cfg.coefficient("MUG", SubjectKind::Tributary) - 0.10).abs() < 1e-9);
		assert!((cfg.coefficient("KOR", SubjectKind::Tributary) - 0.50).abs() < 1e-9);
		assert!((cfg.coefficient("KOR", SubjectKind::Colony) - 1.20).abs() < 1e-9);
		// No kind entry: the global value.
		assert!((cfg.coefficient("KOR", SubjectKind::Vassal) - 0.80).abs() < 1e-9);
	}

	#[test]
	fn disabled_means_passthrough() {
		let mut cfg = config();
		cfg.enabled = false;
		assert!((cfg.coefficient("MUG", SubjectKind::Tributary) - 1.0).abs() < 1e-9);
	}

	#[test]
	fn every_kind_has_a_key_round_trip() {
		for kind in SubjectKind::ALL {
			let mut cfg = TributeConfig::default();
			cfg.kind_percent.insert(kind.key().into(), 33);
			assert!(
				(cfg.coefficient("XXX", kind) - 0.33).abs() < 1e-9,
				"{kind:?}"
			);
		}
	}

	#[test]
	fn runtime_config_round_trips_as_json() {
		let cfg = config();
		let text = serde_json::to_string(&cfg).unwrap();
		let back: TributeConfig = serde_json::from_str(&text).unwrap();
		assert_eq!(cfg, back);
	}
}
