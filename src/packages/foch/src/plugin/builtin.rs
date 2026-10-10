//! Built-in adapter manifests for the first-party upstream DLLs.
//!
//! These let Foch drive the existing EU4 Unicode and Menu patches through the
//! legacy adapter without the upstream packages shipping a `foch-plugin.toml`.
//! They carry the structural facts (entry, phase, status export, config keys)
//! verified from upstream source; a package's file digests are bound when the
//! release asset is imported, not here.

use super::manifest::Manifest;

const EU4_UNICODE_PATCH: &str = include_str!("builtin/eu4-unicode-patch.toml");
const EU4_MENU_PATCH: &str = include_str!("builtin/eu4-menu-patch.toml");

/// Every adapter manifest Foch ships with, parsed. Panics only on a build-time
/// authoring error in the embedded TOML, which the test below catches.
pub fn adapters() -> Vec<Manifest> {
	[EU4_UNICODE_PATCH, EU4_MENU_PATCH]
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
	use crate::plugin::manifest::{Kind, Phase, StatusMode};

	#[test]
	fn built_in_adapters_parse() {
		let adapters = adapters();
		assert_eq!(adapters.len(), 2);
	}

	#[test]
	fn unicode_adapter_matches_verified_facts() {
		let unicode = adapter("io.github.yozoratempest.eu4-unicode-patch").unwrap();
		assert_eq!(unicode.entry.kind, Kind::Legacy);
		assert_eq!(unicode.entry.phase, Phase::Entry);
		let status = unicode.status.unwrap();
		assert_eq!(status.mode, StatusMode::Sync);
		assert_eq!(status.export, "Eu4UnicodeProbeEnabled");
	}

	#[test]
	fn menu_adapter_loads_after_unicode_and_polls() {
		let menu = adapter("io.github.yozoratempest.eu4-menu-patch").unwrap();
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
