//! Fingerprint of a playset's effective state for analysis attribution.
//!
//! The value identifies the ordered inputs and configured decisions recorded
//! with a merge report. It does not authorize reuse of a previous output. The
//! fingerprint covers:
//!
//! - The ordered enabled-mods list (each entry is `(mod_id, version)`).
//! - The sorted local foch.toml `[[overrides]]`.
//! - The sorted local foch.toml `[[resolutions]]`.
//!
//! It does NOT cover:
//!
//! - Mod file contents (a workshop mod content update with the same version
//!   field will be treated as the same playset).
//! - Vanilla game data version (tracked separately by the analyzed base
//!   snapshot identity).

use super::{DepOverride, ResolutionEntry};
use blake3::Hasher;

/// Compute a stable hex fingerprint of a playset's effective merge state.
///
/// `mods` is the ordered enabled-mods list as `(mod_id, version)` pairs; pass
/// the version verbatim from each mod's descriptor (or `""` if absent).
/// `overrides` and `resolutions` come from the resolved `Project`.
pub fn compute_playset_fingerprint(
	mods: &[(String, String)],
	overrides: &[DepOverride],
	resolutions: &[ResolutionEntry],
) -> String {
	let mut hasher = Hasher::new();
	hasher.update(b"foch.playset_fingerprint.v1\n");

	hasher.update(b"mods:\n");
	for (mod_id, version) in mods {
		hasher.update(mod_id.as_bytes());
		hasher.update(b"\t");
		hasher.update(version.as_bytes());
		hasher.update(b"\n");
	}

	hasher.update(b"overrides:\n");
	let mut sorted_overrides: Vec<&DepOverride> = overrides.iter().collect();
	sorted_overrides.sort_by(|a, b| {
		a.mod_id
			.cmp(&b.mod_id)
			.then_with(|| a.dep_id.cmp(&b.dep_id))
	});
	for entry in sorted_overrides {
		hasher.update(entry.mod_id.as_bytes());
		hasher.update(b"\t");
		hasher.update(entry.dep_id.as_bytes());
		hasher.update(b"\n");
	}

	hasher.update(b"resolutions:\n");
	let mut serialized_resolutions: Vec<Vec<u8>> =
		resolutions.iter().map(serialize_resolution_entry).collect();
	serialized_resolutions.sort();
	for entry in serialized_resolutions {
		hasher.update(&entry);
		hasher.update(b"\n");
	}

	hasher.finalize().to_hex().to_string()
}

/// One resolution entry as the bytes the fingerprint hashes. `file` is the
/// game path's canonical text. `use_file` names a host file and contributes
/// its encoded bytes, which are its text for every UTF-8 path, so a name that
/// is not UTF-8 is not replaced by the same text as another.
fn serialize_resolution_entry(entry: &ResolutionEntry) -> Vec<u8> {
	let file = entry
		.file
		.as_ref()
		.map(|path| path.as_str())
		.unwrap_or_default();
	let conflict_id = entry.conflict_id.as_deref().unwrap_or_default();
	let mod_id = entry.mod_id.as_deref().unwrap_or_default();
	let prefer_mod = entry.prefer_mod.as_deref().unwrap_or_default();
	let prefer_candidate = entry
		.prefer_candidate
		.map(|candidate| candidate.to_string())
		.unwrap_or_default();
	let use_file: &[u8] = entry
		.use_file
		.as_ref()
		.map(|path| path.as_os_str().as_encoded_bytes())
		.unwrap_or_default();
	let keep_existing = entry.keep_existing.unwrap_or(false);
	let priority_boost = entry.priority_boost.unwrap_or(0);
	let mut serialized: Vec<u8> = format!(
		"file={file}|conflict_id={conflict_id}|mod={mod_id}|prefer_mod={prefer_mod}|prefer_candidate={prefer_candidate}|use_file="
	)
	.into_bytes();
	serialized.extend_from_slice(use_file);
	serialized.extend_from_slice(
		format!("|keep_existing={keep_existing}|priority_boost={priority_boost}").as_bytes(),
	);
	serialized
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn empty_inputs_produce_a_stable_hash() {
		let fp = compute_playset_fingerprint(&[], &[], &[]);
		assert_eq!(fp.len(), 64); // blake3 hex
		assert_eq!(fp, compute_playset_fingerprint(&[], &[], &[]));
	}

	#[test]
	fn mods_order_changes_the_fingerprint() {
		let a = compute_playset_fingerprint(
			&[("1".into(), "1.0".into()), ("2".into(), "1.0".into())],
			&[],
			&[],
		);
		let b = compute_playset_fingerprint(
			&[("2".into(), "1.0".into()), ("1".into(), "1.0".into())],
			&[],
			&[],
		);
		assert_ne!(a, b, "mod order is part of the playset identity");
	}

	#[test]
	fn override_order_is_normalized() {
		let a = compute_playset_fingerprint(
			&[],
			&[DepOverride::new("a", "b"), DepOverride::new("c", "d")],
			&[],
		);
		let b = compute_playset_fingerprint(
			&[],
			&[DepOverride::new("c", "d"), DepOverride::new("a", "b")],
			&[],
		);
		assert_eq!(a, b, "override order should not affect the fingerprint");
	}

	/// Reports record the fingerprint, so typing the resolution paths must not
	/// move it: the value is the one the path text produced before.
	#[test]
	fn the_fingerprint_is_unchanged_by_typed_resolution_paths() {
		let resolutions = crate::project::Project::from_toml_str(
			r#"
[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "1234567890"

[[resolutions]]
conflict_id = "ab12cd34"
use_file = "manual/Pirate Events.txt"

[[resolutions]]
file = "common/ideas/00_basic ideas.txt"
keep_existing = true
"#,
		)
		.expect("parse config")
		.resolutions;
		assert_eq!(
			compute_playset_fingerprint(
				&[("mod-a".to_string(), "1.0".to_string())],
				&[DepOverride::new("a", "b")],
				&resolutions,
			),
			"6c608ae6f96e223ec40d1a9173973583f439a0eab63cf2f284078abc6d4ae2e0"
		);
	}

	#[test]
	fn resolution_field_difference_changes_fingerprint() {
		let entry_a = ResolutionEntry {
			file: None,
			conflict_id: Some("abc".to_string()),
			mod_id: None,
			r#match: None,
			prefer_mod: Some("X".to_string()),
			prefer_candidate: None,
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: None,
			policy: None,
		};
		let entry_b = ResolutionEntry {
			file: None,
			conflict_id: Some("abc".to_string()),
			mod_id: None,
			r#match: None,
			prefer_mod: Some("Y".to_string()),
			prefer_candidate: None,
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: None,
			policy: None,
		};
		let a = compute_playset_fingerprint(&[], &[], &[entry_a]);
		let b = compute_playset_fingerprint(&[], &[], &[entry_b]);
		assert_ne!(a, b);
	}
}
