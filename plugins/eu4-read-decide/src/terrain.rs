//! Mod-compatible terrain decoding: category mechanics -> bucket.
//!
//! The engine's terrain object carries the CATEGORY name (e.g.
//! "hills", or any name a mod added in `common/terrain/*.txt`). The
//! five-bucket model needs a projection that survives modded values
//! and brand-new categories, so this module reads the ACTIVE
//! category definitions — game files plus the playset's mod overlay,
//! honoring replace_path — and:
//!
//! 1. matches a known vanilla category name to its bucket directly,
//!    using the bucket profiles RECOMPUTED from the active files (a
//!    mod that edits vanilla mechanics moves the profiles with it);
//! 2. projects an unknown or mod-altered category onto the nearest
//!    bucket in normalized mechanic space (movement_cost, defence,
//!    combat_width, supply_limit, attrition);
//! 3. falls back to plains with one logged line per distinct unknown
//!    name when no mechanics exist at all. Terrain never refuses a
//!    frame — that is the mod-compatibility red line.
//!
//! The five one-hot columns stay the contract of the current weights.
//! The v2 trainer replaces that projection with `f(mechanics) + E_id`
//! (see `src/tools/eu4-decide/docs/v2-features.md`): a residual id
//! embedding with `<UNK> = 0` and 30–50% id dropout, so a mod category
//! the embedding has never seen still contributes through `f`. The
//! same shape covers war goals. CTerrain's mechanic fields were not
//! pinned in the binary (no rip-relative lea of `movement_cost`); the
//! active `map/terrain.txt` is the mechanic source.

use std::collections::BTreeMap;

/// The mechanic axes the projection uses. `combat_width` is a
/// modifier in terrain files (0 = none); larger |values| matter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mechanics {
	pub movement_cost: Option<f32>,
	pub defence: Option<f32>,
	pub combat_width: Option<f32>,
	pub supply_limit: Option<f32>,
	pub attrition: Option<f32>,
}

/// One parsed category: its mechanic values plus the classic `type`
/// field the file itself declares (mods usually tag custom terrain
/// with one of the classic types).
#[derive(Clone, Debug, Default)]
pub struct CategoryDef {
	pub mechanics: Mechanics,
	pub classic_type: Option<String>,
}

impl Mechanics {
	/// The five axes the residual head consumes, with the same neutral
	/// defaults as the trainer (`movement_cost` and `supply_limit` are 1).
	pub fn axes(&self) -> [f32; 5] {
		[
			self.movement_cost.unwrap_or(1.0),
			self.defence.unwrap_or(0.0),
			self.combat_width.unwrap_or(0.0),
			self.supply_limit.unwrap_or(1.0),
			self.attrition.unwrap_or(0.0),
		]
	}
}

/// The decoder: active category mechanics plus the five bucket
/// profiles computed from the SAME sources.
pub struct TerrainCodec {
	categories: BTreeMap<String, CategoryDef>,
	buckets: [(String, Mechanics); 5],
	/// Per-axis scale (max absolute bucket value, at least 1) so supply
	/// does not drown defence when projecting a mod category.
	scale: [f32; 5],
	unknown_seen: BTreeMap<String, usize>,
}

/// The vanilla category names in bucket order — column semantics of
/// the plane, not a mod assumption: buckets stay fixed, profiles move
/// with the active files.
pub const BUCKET_NAMES: [&str; 5] = ["plains", "forest", "hills", "mountains", "marsh"];

impl TerrainCodec {
	/// Build from the raw text of every active `common/terrain/*.txt`
	/// (game files then mod overlay, later definitions winning). Each
	/// block is `name = { movement_cost = .. defence = .. ... }`.
	pub fn from_sources(sources: &[String]) -> Self {
		let mut categories: BTreeMap<String, CategoryDef> = BTreeMap::new();
		for text in sources {
			for (name, definition) in parse_categories(text) {
				categories.insert(name, definition);
			}
		}
		let buckets = BUCKET_NAMES.map(|name| {
			let mechanics = categories
				.get(name)
				.map(|definition| definition.mechanics)
				.unwrap_or_default();
			(name.to_string(), mechanics)
		});
		let scale = axis_scale(&buckets);
		Self {
			categories,
			buckets,
			scale,
			unknown_seen: BTreeMap::new(),
		}
	}

	/// Resolve a category name to a plane bucket index (0..5). Unknown
	/// names project by mechanics; total unknowns fall back to plains
	/// and are counted for the log.
	pub fn bucket(&mut self, name: &str) -> usize {
		let lower = name.to_ascii_lowercase();
		if let Some(slot) = BUCKET_NAMES.iter().position(|b| *b == lower) {
			return slot;
		}
		let Some(definition) = self.categories.get(&lower).cloned() else {
			*self.unknown_seen.entry(lower).or_insert(0) += 1;
			return 0;
		};
		if let Some(kind) = definition.classic_type.as_deref()
			&& let Some(slot) = BUCKET_NAMES.iter().position(|b| *b == kind)
		{
			return slot;
		}
		let vector = definition.mechanics.axes();
		let mut best = 0usize;
		let mut best_distance = f32::INFINITY;
		for (slot, (_, bucket_mechanics)) in self.buckets.iter().enumerate() {
			let bucket_vector = bucket_mechanics.axes();
			let distance = vector
				.iter()
				.zip(bucket_vector)
				.zip(self.scale)
				.map(|((a, b), scale)| (a - b).abs() / scale)
				.sum::<f32>();
			if distance < best_distance {
				best_distance = distance;
				best = slot;
			}
		}
		best
	}

	/// Names that fell back without mechanics, with counts — the host
	/// log drains this.
	pub fn drain_unknown(&mut self) -> BTreeMap<String, usize> {
		std::mem::take(&mut self.unknown_seen)
	}

	/// Mechanics registered for `name`, or the neutral defaults when the
	/// active files do not define it. This is the vector the residual
	/// head scores.
	pub fn mechanics_of(&self, name: &str) -> Mechanics {
		self.categories
			.get(&name.to_ascii_lowercase())
			.map(|definition| definition.mechanics)
			.unwrap_or_default()
	}

	/// Inference score `f(modifiers) + E_id`. Unknown names and categories
	/// whose mechanics differ from the exported row use `E_id = 0`.
	pub fn residual_score(
		&self,
		model: &foch_eu4_decide::ResidualCategory,
		name: &str,
	) -> Vec<f32> {
		let mechanics = self.mechanics_of(name);
		model.score(name, mechanics.axes())
	}
}

/// Max absolute value of each mechanic across the five bucket profiles.
fn axis_scale(buckets: &[(String, Mechanics); 5]) -> [f32; 5] {
	let mut scale = [0.0f32; 5];
	for (_, mechanics) in buckets {
		for (slot, value) in mechanics.axes().into_iter().enumerate() {
			scale[slot] = scale[slot].max(value.abs());
		}
	}
	for value in &mut scale {
		if *value < 1.0e-6 {
			*value = 1.0;
		}
	}
	scale
}

/// Brace-depth parser for the real `map/terrain.txt` layout:
/// `categories = { <name> = { type = .. defence = .. .. } }`. Block
/// bodies span lines, so a depth walk (comments stripped) collects
/// each category's fields until its closing brace.
fn parse_categories(text: &str) -> Vec<(String, CategoryDef)> {
	let mut out = Vec::new();
	let mut depth = 0usize;
	let mut inside_categories = false;
	let mut current: Option<(String, CategoryDef)> = None;

	for raw in text.lines() {
		let line = raw.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		let opens = line.matches('{').count();
		let closes = line.matches('}').count();

		if let Some((head, rest)) = line.split_once('=') {
			let (key, value) = (head.trim(), rest.trim());
			if depth == 0 && key == "categories" && value.starts_with('{') {
				inside_categories = true;
			} else if inside_categories && depth == 1 && value.starts_with('{') {
				if let Some(category) = current.take() {
					out.push(category);
				}
				current = Some((key.to_ascii_lowercase(), CategoryDef::default()));
				// Single-line block: fields follow the brace.
				let inline = value.trim_start_matches('{').trim_end_matches('}');
				absorb_fields(current.as_mut().map(|(_, d)| d), inline);
			} else if depth >= 2
				&& let Some((_, definition)) = current.as_mut()
			{
				absorb_fields(Some(definition), line);
			}
		}
		depth = depth + opens - closes;
		if inside_categories && depth <= 1 {
			if let Some(category) = current.take() {
				out.push(category);
			}
			if depth == 0 {
				inside_categories = false;
			}
		}
	}
	if let Some(category) = current.take() {
		out.push(category);
	}
	out
}

/// Read `key = value` mechanic fields out of a body fragment.
fn absorb_fields(target: Option<&mut CategoryDef>, body: &str) {
	let Some(definition) = target else { return };
	let tokens: Vec<&str> = body.split_whitespace().collect();
	let mut index = 0;
	while index + 2 < tokens.len() + 1 {
		if tokens.get(index + 1) == Some(&"=") {
			let key = tokens[index];
			let value = tokens.get(index + 2).copied();
			match key {
				"movement_cost" => {
					definition.mechanics.movement_cost = value.and_then(|v| v.parse().ok())
				}
				"defence" => definition.mechanics.defence = value.and_then(|v| v.parse().ok()),
				"combat_width" => {
					definition.mechanics.combat_width = value.and_then(|v| v.parse().ok())
				}
				"supply_limit" => {
					definition.mechanics.supply_limit = value.and_then(|v| v.parse().ok())
				}
				"attrition" => definition.mechanics.attrition = value.and_then(|v| v.parse().ok()),
				"type" => {
					definition.classic_type = value.map(|v| v.to_ascii_lowercase());
				}
				_ => {}
			}
			index += 3;
		} else {
			index += 1;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn vanilla() -> String {
		// The real map/terrain.txt shape: a categories wrapper, one
		// block per category, fields on their own lines.
		[
			"categories = {",
			"plains = {",
			"  type = plains",
			"  movement_cost = 1.0",
			"  supply_limit = 4",
			"}",
			"forest = {",
			"  type = forest",
			"  movement_cost = 2.0",
			"  defence = 1",
			"}",
			"hills = {",
			"  type = hills",
			"  movement_cost = 2.0",
			"  defence = 2",
			"}",
			"mountains = {",
			"  type = mountains",
			"  movement_cost = 3.0",
			"  defence = 3",
			"  combat_width = -3",
			"}",
			"marsh = {",
			"  type = marsh",
			"  movement_cost = 3.0",
			"  defence = 1",
			"}",
			"}",
		]
		.join("\n")
	}

	#[test]
	fn vanilla_names_hit_their_buckets_directly() {
		let mut codec = TerrainCodec::from_sources(&[vanilla()]);
		assert_eq!(codec.bucket("Hills"), 2);
		assert_eq!(codec.bucket("marsh"), 4);
		assert!(codec.drain_unknown().is_empty());
	}

	#[test]
	fn modded_categories_resolve_by_classic_type_then_mechanics() {
		let overlay = [
			"categories = {",
			"my_haunted_forest = {",
			"  type = forest",
			"  movement_cost = 2.0",
			"}",
			"rocky_plateau = {",
			"  movement_cost = 3.0",
			"  defence = 3",
			"  combat_width = -3",
			"}",
			"}",
		]
		.join("\n");
		let mut codec = TerrainCodec::from_sources(&[vanilla(), overlay]);
		assert_eq!(codec.bucket("my_haunted_forest"), 1, "classic type first");
		assert_eq!(
			codec.bucket("rocky_plateau"),
			3,
			"mechanics project mountains"
		);
		assert!(codec.drain_unknown().is_empty());
	}

	#[test]
	fn total_unknowns_fall_back_to_plains_and_are_logged() {
		let mut codec = TerrainCodec::from_sources(&[vanilla()]);
		assert_eq!(codec.bucket("lava"), 0);
		assert_eq!(codec.drain_unknown().get("lava"), Some(&1));
		assert!(codec.drain_unknown().is_empty(), "drained once");
	}

	#[test]
	fn modded_vanilla_values_move_the_profiles() {
		// A mod flattens mountains into plains-like mechanics; a
		// typeless modded category with those numbers must now project
		// to plains, not mountains.
		let overlay = [
			"categories = {",
			"mountains = { movement_cost = 1.0 supply_limit = 4 }",
			"rocky_plateau = { movement_cost = 1.0 supply_limit = 4 }",
			"}",
		]
		.join("\n");
		let mut codec = TerrainCodec::from_sources(&[vanilla(), overlay]);
		assert_eq!(codec.bucket("rocky_plateau"), 0);
	}

	#[test]
	fn a_typed_category_and_a_color_block_still_resolve() {
		let text = [
			"categories = {",
			"farmlands = {",
			"  color = { 179 255 64 }",
			"  type = plains",
			"  movement_cost = 1.1",
			"  supply_limit = 10",
			"  terrain_override = {",
			"    104 109",
			"  }",
			"}",
			"}",
		]
		.join("\n");
		let mut codec = TerrainCodec::from_sources(&[text]);
		assert_eq!(codec.bucket("farmlands"), 0);
		assert!(codec.drain_unknown().is_empty());
	}

	#[test]
	fn attrition_projects_a_custom_category_onto_the_matching_bucket() {
		let overlay = [
			"categories = {",
			"marsh = { movement_cost = 3.0 defence = 1 attrition = 2 }",
			"haunted_bog = { movement_cost = 3.0 defence = 1 attrition = 2 }",
			"}",
		]
		.join("\n");
		let mut codec = TerrainCodec::from_sources(&[vanilla(), overlay]);
		assert_eq!(codec.bucket("haunted_bog"), 4);
		assert!(codec.drain_unknown().is_empty());
	}

	#[test]
	fn the_residual_forward_keeps_vanilla_ids_and_zeros_mod_ids() {
		let model = foch_eu4_decide::ResidualCategory::from_json(
			r#"{
			"kind": "residual-category",
			"names": ["plains"],
			"modifiers": [[1.0, 0.0, 0.0, 4.0, 0.0]],
			"f0_weight": [[1, 0, 0, 0, 0], [0, 1, 0, 0, 0]],
			"f0_bias": [0, 0],
			"f2_weight": [[1, 0], [0, 1]],
			"f2_bias": [0, 0],
			"embedding": [[9, 9], [1, 0]]
		}"#,
		)
		.expect("residual document");
		let vanilla = [
			"categories = {",
			"plains = { movement_cost = 1.0 supply_limit = 4 }",
			"haunted_bog = { movement_cost = 3 defence = 1 attrition = 2 }",
			"}",
		]
		.join("\n");
		let codec = TerrainCodec::from_sources(&[vanilla]);
		assert_eq!(codec.residual_score(&model, "plains"), vec![2.0, 0.0]);
		assert_eq!(codec.residual_score(&model, "haunted_bog"), vec![3.0, 1.0]);
		let edited = [
			"categories = {",
			"plains = { movement_cost = 9 supply_limit = 4 }",
			"}",
		]
		.join("\n");
		let codec = TerrainCodec::from_sources(&[edited]);
		assert_eq!(
			model.resolve("plains", codec.mechanics_of("plains").axes()),
			0
		);
		assert_eq!(codec.residual_score(&model, "plains"), vec![9.0, 0.0]);
	}
}
