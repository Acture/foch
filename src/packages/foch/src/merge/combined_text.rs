//! Text several mods each wrote for one place, shown together.
//!
//! A tooltip or description is something a player reads. Where mods set one
//! differently, the merge shows every version under a key of its own,
//! `FOCH_TEXT_…`, whose text in each language is each version's text in
//! turn: a localisation key a version names is read in that language, and
//! text a version wrote in place is shown as written.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::game::eu4::content::EU4_LOCALISATION_LANGUAGE_HEADERS;
use crate::game::eu4::derived_localisation::EffectiveLocalisation;

pub(crate) const COMBINED_TEXT_PREFIX: &str = "FOCH_TEXT_";

/// The fields whose value is text the game shows.
const TEXT_FIELDS: [&str; 3] = ["tooltip", "desc", "custom_tooltip"];

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Each combined key with the versions it shows, in load order. The key is a
/// digest of the versions, so a key means the same text in every merge.
fn registry() -> &'static Mutex<BTreeMap<String, Vec<String>>> {
	static REGISTRY: OnceLock<Mutex<BTreeMap<String, Vec<String>>>> = OnceLock::new();
	REGISTRY.get_or_init(Default::default)
}

/// Whether a field holds text the game shows.
pub(crate) fn is_text_field(key: &str) -> bool {
	TEXT_FIELDS.contains(&key)
}

/// The key showing every distinct version, in load order, or `None` when
/// they are one version. A version that is itself combined shows its own
/// versions.
pub(crate) fn combine<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<String> {
	let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
	let mut parts = Vec::<String>::new();
	for version in versions {
		let expanded = registry
			.get(version)
			.cloned()
			.unwrap_or_else(|| vec![version.to_owned()]);
		for part in expanded {
			if !parts.contains(&part) {
				parts.push(part);
			}
		}
	}
	if parts.len() < 2 {
		return None;
	}
	let digest = blake3::hash(parts.join("\u{1f}").as_bytes()).to_hex();
	let key = format!("{COMBINED_TEXT_PREFIX}{}", &digest[..24]);
	registry.entry(key.clone()).or_insert(parts);
	Some(key)
}

/// The versions a combined key shows.
pub(crate) fn versions(key: &str) -> Option<Vec<String>> {
	registry()
		.lock()
		.unwrap_or_else(|error| error.into_inner())
		.get(key)
		.cloned()
}

/// The combined keys `text` names.
pub(crate) fn keys_in(text: &str) -> Vec<String> {
	let mut keys = Vec::new();
	let mut rest = text;
	while let Some(start) = rest.find(COMBINED_TEXT_PREFIX) {
		let tail = &rest[start..];
		let end = tail
			.find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
			.unwrap_or(tail.len());
		keys.push(tail[..end].to_owned());
		rest = &tail[end..];
	}
	keys
}

/// A localisation file defining each key in every language as its versions'
/// text, one per line.
pub(crate) fn render_localisation(keys: &[String], texts: &EffectiveLocalisation) -> Vec<u8> {
	let mut bytes = UTF8_BOM.to_vec();
	for language in EU4_LOCALISATION_LANGUAGE_HEADERS {
		bytes.extend_from_slice(format!("{language}:\n").as_bytes());
		for key in keys {
			let Some(parts) = versions(key) else {
				continue;
			};
			let text = parts
				.iter()
				.map(|part| {
					texts
						.get(language, part)
						.or_else(|| texts.get("l_english", part))
						.unwrap_or(part)
						.to_owned()
				})
				.collect::<Vec<_>>()
				.join("\\n");
			bytes.extend_from_slice(format!(" {key}:0 \"{text}\"\n").as_bytes());
		}
	}
	bytes
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn one_version_needs_no_combined_key() {
		assert_eq!(combine(["SAME_TT", "SAME_TT"]), None);
	}

	#[test]
	fn combined_key_shows_each_version_in_each_language() {
		let key = combine(["LEFT_TT", "written in place"]).expect("two versions");
		let mut texts = EffectiveLocalisation::default();
		texts.insert("l_english", "LEFT_TT", "Left text");
		texts.insert("l_german", "LEFT_TT", "Linker Text");

		let rendered =
			String::from_utf8(render_localisation(std::slice::from_ref(&key), &texts)).unwrap();

		assert!(
			rendered.contains(&format!(" {key}:0 \"Left text\\nwritten in place\"")),
			"{rendered}"
		);
		assert!(
			rendered.contains(&format!(" {key}:0 \"Linker Text\\nwritten in place\"")),
			"{rendered}"
		);
		assert_eq!(keys_in(&format!("tooltip = {key}\n")), vec![key]);
	}

	#[test]
	fn a_combined_version_shows_its_own_versions() {
		let inner = combine(["A_TT", "B_TT"]).unwrap();
		let outer = combine([inner.as_str(), "C_TT"]).unwrap();

		assert_eq!(versions(&outer).unwrap(), ["A_TT", "B_TT", "C_TT"]);
	}
}
