//! Name embeddings for idea groups, personalities, and government attributes.
//!
//! Unseen names hash to a stable vector. Two different unseen names do not share one row.

pub const EMBED: usize = 8;

/// Learned rows for names. Absent names use [`HashEmbed`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameTable {
	rows: Vec<(String, [f32; EMBED])>,
}

impl NameTable {
	pub fn insert(&mut self, name: impl Into<String>, row: [f32; EMBED]) {
		let name = name.into();
		if let Some(existing) = self.rows.iter_mut().find(|(known, _)| known == &name) {
			existing.1 = row;
		} else {
			self.rows.push((name, row));
		}
	}

	pub fn embed(&self, name: &str) -> [f32; EMBED] {
		self.rows
			.iter()
			.find(|(known, _)| known == name)
			.map(|(_, row)| *row)
			.unwrap_or_else(|| HashEmbed::embed(name))
	}
}

/// Sum of at most `limit` names. Missing slots contribute zero.
pub fn sum_names(table: &NameTable, names: &[&str], limit: usize) -> [f32; EMBED] {
	let mut sum = [0.0; EMBED];
	for name in names.iter().take(limit) {
		let row = table.embed(name);
		for (total, value) in sum.iter_mut().zip(row) {
			*total += value;
		}
	}
	sum
}

/// Mean of every name. An empty set is the zero feature. There is no count cap.
/// Names are sorted first so the feature does not depend on listing order.
pub fn mean_names(table: &NameTable, names: &[&str]) -> [f32; EMBED] {
	if names.is_empty() {
		return [0.0; EMBED];
	}
	let mut ordered: Vec<&str> = names.to_vec();
	ordered.sort_unstable();
	let mut sum = [0.0; EMBED];
	for name in &ordered {
		let row = table.embed(name);
		for (total, value) in sum.iter_mut().zip(row) {
			*total += value;
		}
	}
	let count = ordered.len() as f32;
	for value in &mut sum {
		*value /= count;
	}
	sum
}

pub struct HashEmbed;

impl HashEmbed {
	pub fn embed(name: &str) -> [f32; EMBED] {
		let first = fnv1a(name.as_bytes());
		let mut salted = name.as_bytes().to_vec();
		salted.push(0xff);
		let second = fnv1a(&salted);
		let mut out = [0.0; EMBED];
		for (index, value) in out.iter_mut().enumerate() {
			let byte = if index < 4 {
				(first >> (index * 8)) as u8
			} else {
				(second >> ((index - 4) * 8)) as u8
			};
			*value = (f32::from(byte) / 127.5) - 1.0;
		}
		out
	}
}

/// Row `index` of the Sylvester Hadamard basis, scaled to the average norm
/// of a [`HashEmbed`] vector. The first eight rows are mutually orthogonal
/// and the next eight are their negatives, so seeding known names this way
/// avoids hash collisions between names that must stay separable. Names
/// beyond sixteen fall back to [`HashEmbed`].
pub fn hadamard_row(index: usize) -> [f32; EMBED] {
	if index >= 16 {
		return HashEmbed::embed(&format!("hadamard:{index}"));
	}
	let sign = if index >= 8 { -1.0 } else { 1.0 };
	let bits = index % 8;
	let mut row = [0.0; EMBED];
	for (column, value) in row.iter_mut().enumerate() {
		let mask = bits & column;
		let flip = mask.count_ones() % 2;
		*value = sign * (1.0 - 2.0 * f32::from(flip == 1)) / 3f32.sqrt();
	}
	row
}

fn fnv1a(bytes: &[u8]) -> u64 {
	let mut hash = 0xcbf29ce484222325;
	for byte in bytes {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(0x100000001b3);
	}
	hash
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn idea_group_order_does_not_change_the_feature() {
		let table = NameTable::default();
		let left = mean_names(&table, &["trade_ideas", "offensive_ideas", "FRA_ideas"]);
		let right = mean_names(&table, &["FRA_ideas", "offensive_ideas", "trade_ideas"]);
		assert_eq!(left, right);
	}

	#[test]
	fn mod_groups_are_not_capped_at_eight() {
		let table = NameTable::default();
		assert_eq!(mean_names(&table, &[]), [0.0; EMBED]);
		let names: Vec<String> = (0..12).map(|index| format!("mod_group_{index}")).collect();
		let refs: Vec<&str> = names.iter().map(String::as_str).collect();
		let pooled = mean_names(&table, &refs);
		assert!(pooled.iter().any(|value| *value != 0.0));
	}

	#[test]
	fn unseen_names_do_not_share_one_vector() {
		assert_ne!(
			HashEmbed::embed("is_merchant_republic"),
			HashEmbed::embed("mod_government_attribute")
		);
	}

	#[test]
	fn personalities_sum_at_most_three_names() {
		let table = NameTable::default();
		let three = sum_names(
			&table,
			&[
				"trader_personality",
				"careful_personality",
				"conqueror_personality",
				"ignored_personality",
			],
			3,
		);
		let expected = sum_names(
			&table,
			&[
				"trader_personality",
				"careful_personality",
				"conqueror_personality",
			],
			3,
		);
		assert_eq!(three, expected);
	}
}
