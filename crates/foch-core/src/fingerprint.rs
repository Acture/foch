//! Fingerprint of a playset's effective state for merge-result reuse.
//!
//! When `foch merge --out X` runs against a directory whose previous report
//! shows a matching fingerprint, the merge can be skipped entirely and the
//! cached report reused. The fingerprint covers:
//!
//! - The ordered enabled-mods list (each entry is `(mod_id, version)`).
//! - The sorted local foch.toml `[[overrides]]`.
//! - The sorted local foch.toml `[[resolutions]]`.
//!
//! It does NOT cover:
//!
//! - Mod file contents (a workshop mod content update with the same version
//!   field will be treated as the same playset).
//! - Vanilla game data version (caller should invalidate the out_dir on game
//!   patch).

use crate::config::{DepOverride, ResolutionEntry};
use blake3::Hasher;

/// Compute a stable hex fingerprint of a playset's effective merge state.
///
/// `mods` is the ordered enabled-mods list as `(mod_id, version)` pairs; pass
/// the version verbatim from each mod's descriptor (or `""` if absent).
/// `overrides` and `resolutions` come from the resolved `FochConfig`.
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
	let mut serialized_resolutions: Vec<String> =
		resolutions.iter().map(serialize_resolution_entry).collect();
	serialized_resolutions.sort();
	for entry in serialized_resolutions {
		hasher.update(entry.as_bytes());
		hasher.update(b"\n");
	}

	hasher.finalize().to_hex().to_string()
}

fn serialize_resolution_entry(entry: &ResolutionEntry) -> String {
	let file = entry
		.file
		.as_ref()
		.map(|p| p.to_string_lossy().into_owned())
		.unwrap_or_default();
	let conflict_id = entry.conflict_id.clone().unwrap_or_default();
	let mod_id = entry.mod_id.clone().unwrap_or_default();
	let prefer_mod = entry.prefer_mod.clone().unwrap_or_default();
	let use_file = entry
		.use_file
		.as_ref()
		.map(|p| p.to_string_lossy().into_owned())
		.unwrap_or_default();
	let keep_existing = entry.keep_existing.unwrap_or(false);
	let priority_boost = entry.priority_boost.unwrap_or(0);
	format!(
		"file={file}|conflict_id={conflict_id}|mod={mod_id}|prefer_mod={prefer_mod}|use_file={use_file}|keep_existing={keep_existing}|priority_boost={priority_boost}"
	)
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

	#[test]
	fn resolution_field_difference_changes_fingerprint() {
		let entry_a = ResolutionEntry {
			file: None,
			conflict_id: Some("abc".to_string()),
			mod_id: None,
			r#match: None,
			prefer_mod: Some("X".to_string()),
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: None,
		};
		let entry_b = ResolutionEntry {
			file: None,
			conflict_id: Some("abc".to_string()),
			mod_id: None,
			r#match: None,
			prefer_mod: Some("Y".to_string()),
			use_file: None,
			keep_existing: None,
			priority_boost: None,
			handler: None,
		};
		let a = compute_playset_fingerprint(&[], &[], &[entry_a]);
		let b = compute_playset_fingerprint(&[], &[], &[entry_b]);
		assert_ne!(a, b);
	}
}
