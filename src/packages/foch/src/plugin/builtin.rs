//! Built-in adapter manifests for upstream EU4 DLLs.
//!
//! These let Foch drive the Unicode patch, the Menu patch, and EU4dll through
//! the legacy adapter without those packages shipping a `foch-plugin.toml`.
//! They carry the structural facts (entry, phase, status export, config keys)
//! verified from upstream source. `foch plugin install` downloads the pinned
//! GitHub release and binds its file digests here.

use super::manifest::Manifest;

const EU4_UNICODE_PATCH: &str = include_str!("builtin/eu4-unicode-patch.toml");
const EU4_MENU_PATCH: &str = include_str!("builtin/eu4-menu-patch.toml");
const EU4DLL: &str = include_str!("builtin/eu4dll.toml");

/// Every adapter manifest Foch ships with, parsed. Panics only on a build-time
/// authoring error in the embedded TOML, which the test below catches.
pub fn adapters() -> Vec<Manifest> {
	[EU4_UNICODE_PATCH, EU4_MENU_PATCH, EU4DLL]
		.into_iter()
		.map(|text| Manifest::parse(text).expect("built-in adapter manifest is valid"))
		.collect()
}

/// The adapter manifest for a plugin id, if Foch ships one.
pub fn adapter(plugin_id: &str) -> Option<Manifest> {
	adapters()
		.into_iter()
		.find(|manifest| manifest.plugin.id == plugin_id)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::plugin::manifest::{ConfigField, Kind, Phase, StatusMode};
	use semver::Version;

	#[test]
	fn built_in_adapters_parse() {
		let adapters = adapters();
		assert_eq!(adapters.len(), 3);
	}

	#[test]
	fn unicode_adapter_matches_verified_facts() {
		let unicode = adapter("io.github.yozoratempest.eu4-unicode-patch").unwrap();
		assert_eq!(unicode.plugin.version.to_string(), "0.1.15");
		assert_eq!(unicode.entry.kind, Kind::Legacy);
		assert_eq!(unicode.entry.phase, Phase::Entry);
		let status = unicode.status.unwrap();
		assert_eq!(status.mode, StatusMode::Sync);
		assert_eq!(status.export, "Eu4UnicodeProbeEnabled");
	}

	#[test]
	fn eu4dll_adapter_matches_release_93() {
		let eu4dll = adapter("io.github.matanki-saito.eu4dll").unwrap();
		assert_eq!(eu4dll.plugin.version, Version::parse("93.0.0").unwrap());
		assert_eq!(eu4dll.entry.kind, Kind::Legacy);
		assert_eq!(eu4dll.entry.phase, Phase::Entry);
		assert_eq!(eu4dll.entry.path, "plugins/plugin64.dll");
		assert!(eu4dll.status.is_none());
		assert_eq!(eu4dll.exe_relative, ["plugins/plugin.ini"]);
		assert!(eu4dll.conflicts.is_empty());
		assert!(
			eu4dll
				.plugin
				.description
				.as_deref()
				.unwrap()
				.contains("release 93")
		);
		let versions = &eu4dll.target.game_versions;
		assert!(versions.matches(&Version::parse("1.29.0").unwrap()));
		assert!(versions.matches(&Version::parse("1.37.5").unwrap()));
		assert!(!versions.matches(&Version::parse("1.28.0").unwrap()));
		assert!(!versions.matches(&Version::parse("1.38.0").unwrap()));
		match &eu4dll.config["error_test"] {
			ConfigField::Bool {
				default: false,
				adapter: Some(adapter),
				..
			} => {
				assert_eq!(adapter.yes.as_deref(), Some("yes"));
				assert_eq!(adapter.no.as_deref(), Some("no"));
			}
			other => panic!("unexpected error_test field: {other:?}"),
		}
		match &eu4dll.config["separate_character_code_point"] {
			ConfigField::Int { default: 38936, .. } => {}
			other => panic!("unexpected code point field: {other:?}"),
		}
		let text = toml::to_string(&eu4dll).unwrap();
		assert_eq!(
			crate::plugin::manifest::Manifest::parse(&text).unwrap(),
			eu4dll
		);
	}

	#[test]
	fn menu_adapter_loads_after_unicode_and_polls() {
		let menu = adapter("io.github.yozoratempest.eu4-menu-patch").unwrap();
		assert_eq!(menu.plugin.version.to_string(), "0.1.4-experimental");
		assert_eq!(menu.entry.phase, Phase::Deferred);
		assert_eq!(
			menu.load_after,
			["io.github.yozoratempest.eu4-unicode-patch"]
		);
		let status = menu.status.unwrap();
		assert_eq!(status.mode, StatusMode::Poll);
		assert_eq!(status.pending, [0]);
	}
}
