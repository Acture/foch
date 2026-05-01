use crate::model::ModCandidate;
use std::collections::HashMap;

/// Index that resolves a dependency string declared in a `descriptor.mod`'s
/// `dependencies = { ... }` block to a concrete [`ModCandidate`] in the
/// workspace.
///
/// Paradox launcher accepts either the human-readable mod `name` (the value of
/// the `name = "..."` field in `descriptor.mod`) or, occasionally, the
/// workshop steam id. We index both so the consumer (missing-mod-dependency missing-dep
/// diagnostic and the upcoming DAG builder) can ask a single question.
///
/// Lookups are exact-match for now. The launcher's own behavior around case is
/// not publicly documented; switching to a normalized lookup would be a small
/// follow-up if real corpora reveal disagreements.
#[derive(Debug, Default)]
pub struct ModIdentityIndex {
	by_name: HashMap<String, usize>,
	by_steam_id: HashMap<String, usize>,
}

impl ModIdentityIndex {
	/// Build an index from the workspace's mod candidates. Later candidates
	/// with a colliding key win — this matches playlist semantics where a
	/// later position overrides earlier ones.
	pub fn from_mods(mods: &[ModCandidate]) -> Self {
		let mut by_name = HashMap::new();
		let mut by_steam_id = HashMap::new();
		for (idx, candidate) in mods.iter().enumerate() {
			if let Some(descriptor) = candidate.descriptor.as_ref()
				&& !descriptor.name.is_empty()
			{
				by_name.insert(descriptor.name.clone(), idx);
			}
			let steam_id = candidate.entry.steam_id.as_deref().unwrap_or("").trim();
			if !steam_id.is_empty() {
				by_steam_id.insert(steam_id.to_string(), idx);
			}
			// `mod_id` typically equals the steam id for workshop mods; index
			// it as well so callers can resolve by either.
			if !candidate.mod_id.is_empty() {
				by_steam_id.insert(candidate.mod_id.clone(), idx);
			}
		}
		Self {
			by_name,
			by_steam_id,
		}
	}

	/// Resolve a dependency token (as it appears inside
	/// `dependencies = { ... }`) to the index of the matching candidate.
	///
	/// The lookup tries, in order:
	/// 1. exact `descriptor.name`
	/// 2. exact steam id (fallback for descriptors that put a workshop id in
	///    `dependencies`)
	pub fn lookup(&self, token: &str) -> Option<usize> {
		let trimmed = token.trim();
		if trimmed.is_empty() {
			return None;
		}
		if let Some(&idx) = self.by_name.get(trimmed) {
			return Some(idx);
		}
		self.by_steam_id.get(trimmed).copied()
	}

	/// Convenience: returns true iff the token resolves to any candidate.
	pub fn contains(&self, token: &str) -> bool {
		self.lookup(token).is_some()
	}
}

#[cfg(test)]
mod tests {
	use super::ModIdentityIndex;
	use crate::domain::descriptor::ModDescriptor;
	use crate::domain::playlist::PlaylistEntry;
	use crate::model::ModCandidate;

	fn make_candidate(name: &str, steam_id: Option<&str>) -> ModCandidate {
		let descriptor = ModDescriptor {
			name: name.to_string(),
			..ModDescriptor::default()
		};
		let entry = PlaylistEntry {
			steam_id: steam_id.map(str::to_string),
			..PlaylistEntry::default()
		};
		ModCandidate {
			entry,
			mod_id: steam_id.unwrap_or_default().to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(descriptor),
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	#[test]
	fn matches_by_name() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert_eq!(index.lookup("Extended Timeline"), Some(0));
	}

	#[test]
	fn matches_by_steam_id() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert_eq!(index.lookup("12345"), Some(0));
	}

	#[test]
	fn unknown_token_returns_none() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert!(index.lookup("Nonexistent").is_none());
		assert!(index.lookup("99999").is_none());
	}

	#[test]
	fn empty_or_whitespace_token_returns_none() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert!(index.lookup("").is_none());
		assert!(index.lookup("   ").is_none());
	}

	#[test]
	fn case_sensitive_name_match() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		// Exact-match policy: a different case is currently a miss. If the
		// launcher turns out to be case-insensitive this should be relaxed.
		assert!(index.lookup("extended timeline").is_none());
	}

	#[test]
	fn lookups_trim_surrounding_whitespace() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert_eq!(index.lookup("  Extended Timeline  "), Some(0));
	}

	#[test]
	fn contains_helper_aligns_with_lookup() {
		let mods = vec![make_candidate("Extended Timeline", Some("12345"))];
		let index = ModIdentityIndex::from_mods(&mods);
		assert!(index.contains("Extended Timeline"));
		assert!(index.contains("12345"));
		assert!(!index.contains("ghost"));
	}
}
