//! War-score rules stored on the game's objects after the loaders return.
//!
//! A wargoal name is the string at [`crate::WARGOAL_ENTRY_NAME`] on the wargoal
//! object. A define value is the number in the defines object. A later store
//! of the same key replaces it and starts a new generation. The training delta
//! is still the integer `AddEffectiveWarScore` wrote.

use std::collections::BTreeMap;

/// Loaded wargoal names and define values. Generation `0` means nothing has been reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScoreRules {
	wargoals: BTreeMap<String, ()>,
	defines: BTreeMap<String, u32>,
	generation: u64,
	defines_applied: bool,
}

impl ScoreRules {
	/// One wargoal definition the file function just stored. The same name again is an override.
	pub fn note_wargoal(&mut self, name: &str) {
		if name.is_empty() {
			return;
		}
		self.wargoals.insert(name.to_string(), ());
		self.bump();
	}

	/// One define the applicator stored. The same bits for the same key do not start a new generation.
	pub fn note_define(&mut self, key: &str, value: f32) {
		if key.is_empty() {
			return;
		}
		let bits = value.to_bits();
		if self.defines.get(key).copied() == Some(bits) {
			return;
		}
		self.defines.insert(key.to_string(), bits);
		self.bump();
	}

	/// The defines object has the merged values.
	pub fn note_defines_applied(&mut self) {
		self.defines_applied = true;
		self.bump();
	}

	pub fn generation(&self) -> u64 {
		self.generation
	}

	/// Both the wargoal database and the define walk have reported in.
	pub fn ready(&self) -> bool {
		self.defines_applied && !self.wargoals.is_empty()
	}

	pub fn wargoals(&self) -> impl Iterator<Item = &str> {
		self.wargoals.keys().map(String::as_str)
	}

	pub fn defines_applied(&self) -> bool {
		self.defines_applied
	}

	pub fn defines(&self) -> impl Iterator<Item = (&str, f32)> {
		self.defines
			.iter()
			.map(|(key, bits)| (key.as_str(), f32::from_bits(*bits)))
	}

	pub fn define(&self, key: &str) -> Option<f32> {
		self.defines.get(key).copied().map(f32::from_bits)
	}

	/// Content of the loaded rules. A replaced define or wargoal changes it.
	pub fn identity(&self) -> u64 {
		let mut hash = 0xcbf2_9ce4_8422_2325u64;
		hash = mix_bytes(hash, &[u8::from(self.defines_applied)]);
		for name in self.wargoals() {
			hash = mix_bytes(hash, name.as_bytes());
			hash = mix_bytes(hash, &[0xff]);
		}
		for (key, value) in self.defines() {
			hash = mix_bytes(hash, key.as_bytes());
			hash = mix_bytes(hash, &value.to_bits().to_le_bytes());
		}
		hash
	}

	fn bump(&mut self) {
		self.generation = self.generation.wrapping_add(1);
	}
}

fn mix_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
	for byte in bytes {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}
	hash
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_replaced_define_starts_a_new_generation() {
		let mut rules = ScoreRules::default();
		rules.note_wargoal("superiority");
		rules.note_define("WARSCORE_MAX_FROM_BATTLES", 40.0);
		rules.note_defines_applied();
		let loaded = rules.generation();
		rules.note_define("WARSCORE_MAX_FROM_BATTLES", 40.0);
		assert_eq!(rules.generation(), loaded);
		rules.note_define("WARSCORE_MAX_FROM_BATTLES", 80.0);
		assert_ne!(rules.generation(), loaded);
		assert_eq!(rules.define("WARSCORE_MAX_FROM_BATTLES"), Some(80.0));
		rules.note_wargoal("superiority");
		assert_ne!(rules.generation(), loaded);
	}

	#[test]
	fn rules_are_not_ready_before_both_loads() {
		let mut rules = ScoreRules::default();
		assert!(!rules.ready());
		rules.note_wargoal("superiority");
		assert!(!rules.ready());
		rules.note_defines_applied();
		assert!(rules.ready());
		assert_eq!(rules.wargoals().collect::<Vec<_>>(), vec!["superiority"]);
	}
}
