//! `f(modifiers) + E_id` for terrain and the same shape of category.
//!
//! Index 0 is `<UNK>` and contributes nothing. A name the exported table
//! does not contain, and a known name whose mechanics no longer match the
//! registered row, both resolve to 0. Inference never applies id dropout;
//! that stays on the Python trainer.

use serde_json::Value;

/// Mechanic axes, in the order the trainer exports.
pub const MECHANICS: [&str; 5] = [
	"movement_cost",
	"defence",
	"combat_width",
	"supply_limit",
	"attrition",
];

/// Exported residual category head. Row 0 of `embedding` is `<UNK>`.
#[derive(Clone, Debug)]
pub struct ResidualCategory {
	names: Vec<String>,
	modifiers: Vec<[f32; 5]>,
	f0_weight: Vec<f32>,
	f0_bias: Vec<f32>,
	f2_weight: Vec<f32>,
	f2_bias: Vec<f32>,
	embedding: Vec<f32>,
	width: usize,
}

impl ResidualCategory {
	/// Load the document [`crate`]'s Python `export_document` writes.
	pub fn from_json(text: &str) -> Option<Self> {
		let root: Value = serde_json::from_str(text).ok()?;
		if root.get("kind")?.as_str()? != "residual-category" {
			return None;
		}
		let names = string_list(root.get("names")?)?;
		let modifiers = modifier_rows(root.get("modifiers")?, names.len())?;
		let f0_weight = numbers(root.get("f0_weight")?)?;
		let f0_bias = numbers(root.get("f0_bias")?)?;
		let f2_weight = numbers(root.get("f2_weight")?)?;
		let f2_bias = numbers(root.get("f2_bias")?)?;
		let embedding = numbers(root.get("embedding")?)?;
		let width = f0_bias.len();
		if width == 0
			|| f2_bias.len() != width
			|| f0_weight.len() != width * MECHANICS.len()
			|| f2_weight.len() != width * width
			|| embedding.len() != (names.len() + 1) * width
		{
			return None;
		}
		if !finite(&f0_weight)
			|| !finite(&f0_bias)
			|| !finite(&f2_weight)
			|| !finite(&f2_bias)
			|| !finite(&embedding)
		{
			return None;
		}
		Some(Self {
			names,
			modifiers,
			f0_weight,
			f0_bias,
			f2_weight,
			f2_bias,
			embedding,
			width,
		})
	}

	/// Inference score. Unseen and edited categories use `E_id = 0`.
	pub fn score(&self, name: &str, modifiers: [f32; 5]) -> Vec<f32> {
		let id = self.resolve(name, modifiers);
		let mut hidden = linear(&self.f0_weight, &self.f0_bias, self.width, &modifiers);
		for value in &mut hidden {
			*value = value.max(0.0);
		}
		let mut out = linear(&self.f2_weight, &self.f2_bias, self.width, &hidden);
		if id != 0 {
			let row = &self.embedding[id * self.width..(id + 1) * self.width];
			for (slot, extra) in out.iter_mut().zip(row) {
				*slot += extra;
			}
		}
		out
	}

	/// The id the forward will use. `0` is `<UNK>`.
	pub fn resolve(&self, name: &str, modifiers: [f32; 5]) -> usize {
		let Some(index) = self.names.iter().position(|known| known == name) else {
			return 0;
		};
		if !same_mechanics(self.modifiers[index], modifiers) {
			return 0;
		}
		index + 1
	}
}

/// Terrain and casus-belli heads on one decide forward.
///
/// The scalar is `mean(f_t + E_t) + mean(f_c + E_c)`, matching the trainer's
/// `ResidualDecision`. A missing head contributes nothing. A terrain-only
/// export therefore ignores the casus belli name and its modifiers.
#[derive(Clone, Debug)]
pub struct DecisionForward {
	terrain: Option<ResidualCategory>,
	cb: Option<ResidualCategory>,
}

impl DecisionForward {
	/// Load `{ "terrain": <residual-category>, "cb": <residual-category> }`.
	/// Either object may be absent. A bare `residual-category` document is
	/// the terrain head alone, which is what terrain exports already are.
	pub fn from_json(text: &str) -> Option<Self> {
		let root: Value = serde_json::from_str(text).ok()?;
		if root.get("kind").and_then(Value::as_str) == Some("residual-category") {
			return Some(Self {
				terrain: Some(ResidualCategory::from_json(text)?),
				cb: None,
			});
		}
		let terrain = optional_head(root.get("terrain"))?;
		let cb = optional_head(root.get("cb"))?;
		if terrain.is_none() && cb.is_none() {
			return None;
		}
		Some(Self { terrain, cb })
	}

	pub fn cb_loaded(&self) -> bool {
		self.cb.is_some()
	}

	pub fn terrain(&self) -> Option<&ResidualCategory> {
		self.terrain.as_ref()
	}

	pub fn cb(&self) -> Option<&ResidualCategory> {
		self.cb.as_ref()
	}

	/// Inference score. Unseen and stat-modified categories use `E_id = 0`
	/// inside whichever head is loaded.
	pub fn score(
		&self,
		terrain_name: &str,
		terrain_modifiers: [f32; 5],
		cb_name: &str,
		cb_modifiers: [f32; 5],
	) -> f32 {
		let mut total = 0.0;
		if let Some(head) = &self.terrain {
			total += mean(&head.score(terrain_name, terrain_modifiers));
		}
		if let Some(head) = &self.cb {
			total += mean(&head.score(cb_name, cb_modifiers));
		}
		total
	}
}

fn optional_head(value: Option<&Value>) -> Option<Option<ResidualCategory>> {
	match value {
		None => Some(None),
		Some(value) => ResidualCategory::from_json(&value.to_string()).map(Some),
	}
}

fn mean(values: &[f32]) -> f32 {
	let width = values.len() as f32;
	if width == 0.0 {
		0.0
	} else {
		values.iter().sum::<f32>() / width
	}
}

fn same_mechanics(trained: [f32; 5], modifiers: [f32; 5]) -> bool {
	trained.iter().zip(modifiers).all(|(left, right)| {
		let scale = left.abs().max(right.abs());
		(left - right).abs() <= 1.0e-5 + 1.0e-5 * scale
	})
}

fn linear(weight: &[f32], bias: &[f32], out: usize, input: &[f32]) -> Vec<f32> {
	let input_len = weight.len() / out;
	(0..out)
		.map(|row| {
			let mut sum = bias[row];
			let start = row * input_len;
			for (value, factor) in input.iter().zip(&weight[start..start + input_len]) {
				sum += value * factor;
			}
			sum
		})
		.collect()
}

fn finite(values: &[f32]) -> bool {
	values.iter().all(|value| value.is_finite())
}

fn numbers(value: &Value) -> Option<Vec<f32>> {
	let mut out = Vec::new();
	fn walk(value: &Value, out: &mut Vec<f32>) -> Option<()> {
		match value {
			Value::Number(number) => {
				let parsed = number.as_f64()?;
				if !parsed.is_finite() {
					return None;
				}
				out.push(parsed as f32);
			}
			Value::Array(items) => {
				for item in items {
					walk(item, out)?;
				}
			}
			_ => return None,
		}
		Some(())
	}
	walk(value, &mut out)?;
	Some(out)
}

fn string_list(value: &Value) -> Option<Vec<String>> {
	value
		.as_array()?
		.iter()
		.map(|item| item.as_str().map(str::to_string))
		.collect()
}

fn modifier_rows(value: &Value, count: usize) -> Option<Vec<[f32; 5]>> {
	let rows = value.as_array()?;
	if rows.len() != count {
		return None;
	}
	rows.iter()
		.map(|row| {
			let flat = numbers(row)?;
			if flat.len() != MECHANICS.len() {
				return None;
			}
			let mut axes = [0.0f32; 5];
			axes.copy_from_slice(&flat);
			Some(axes)
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn model() -> ResidualCategory {
		// width 2. f0 is identity on the first two mechanics, f2 is identity,
		// embedding row 1 is [1, 0]. Plains matches the registered row.
		let text = r#"{
			"kind": "residual-category",
			"names": ["plains"],
			"modifiers": [[1.0, 0.0, 0.0, 4.0, 0.0]],
			"f0_weight": [[1, 0, 0, 0, 0], [0, 1, 0, 0, 0]],
			"f0_bias": [0, 0],
			"f2_weight": [[1, 0], [0, 1]],
			"f2_bias": [0, 0],
			"embedding": [[9, 9], [1, 0]]
		}"#;
		ResidualCategory::from_json(text).expect("fixture")
	}

	#[test]
	fn vanilla_keeps_its_residual_and_unknown_or_edited_values_do_not() {
		let model = model();
		let plains = [1.0, 0.0, 0.0, 4.0, 0.0];
		let edited = [9.0, 0.0, 0.0, 4.0, 0.0];
		let bog = [3.0, 1.0, 0.0, 1.0, 2.0];
		assert_eq!(model.resolve("plains", plains), 1);
		assert_eq!(model.resolve("plains", edited), 0);
		assert_eq!(model.resolve("haunted_bog", bog), 0);
		let vanilla = model.score("plains", plains);
		assert_eq!(vanilla, vec![2.0, 0.0]);
		assert_eq!(model.score("plains", edited), vec![9.0, 0.0]);
		assert_eq!(model.score("haunted_bog", bog), vec![3.0, 1.0]);
		assert!(ResidualCategory::from_json("{}").is_none());
	}

	#[test]
	fn ablation_export_matches_python_on_every_distribution() {
		let raw = include_str!("../tests/fixtures/residual_ablation.json");
		let root: Value = serde_json::from_str(raw).expect("ablation fixture");
		let terrain =
			ResidualCategory::from_json(&root["terrain"].to_string()).expect("terrain head");
		let cb = ResidualCategory::from_json(&root["cb"].to_string()).expect("cb head");
		let cases = root["cases"].as_array().expect("cases");
		assert!(!cases.is_empty());
		let mut seen = std::collections::BTreeSet::new();
		let mut worst = 0.0f32;
		let mut worst_at = String::new();
		for case in cases {
			let distribution = case["distribution"].as_str().expect("distribution");
			let head = case["head"].as_str().expect("head");
			let name = case["name"].as_str().expect("name");
			seen.insert(distribution);
			let modifiers = five(case.get("modifiers").expect("modifiers"));
			let expected = numbers(case.get("score").expect("score")).expect("score");
			let model = match head {
				"terrain" => &terrain,
				"cb" => &cb,
				other => panic!("unknown head {other}"),
			};
			let got = model.score(name, modifiers);
			assert_eq!(got.len(), expected.len(), "{distribution} {head} {name}");
			for (index, (left, right)) in got.iter().zip(expected).enumerate() {
				let error = (left - right).abs();
				if error > worst {
					worst = error;
					worst_at = format!("{distribution}/{head}/{name}[{index}]");
				}
			}
		}
		for required in [
			"vanilla_id",
			"vanilla_stat_modified",
			"overhaul_unseen_terrain",
			"overhaul_unseen_cb",
		] {
			assert!(seen.contains(required), "missing {required}");
		}
		assert!(
			worst < 1.0e-5,
			"python/rust max abs {worst:.3e} at {worst_at}"
		);
		println!("ablation parity max abs {worst:.3e} at {worst_at}");
	}

	#[test]
	fn casus_belli_changes_the_decision_only_through_mechanics_when_the_id_is_unknown() {
		let raw = include_str!("../tests/fixtures/residual_ablation.json");
		let root: Value = serde_json::from_str(raw).expect("ablation fixture");
		let forward = DecisionForward::from_json(raw).expect("both heads");
		assert!(forward.cb_loaded());
		let terrain_only =
			DecisionForward::from_json(&root["terrain"].to_string()).expect("terrain export");
		assert!(!terrain_only.cb_loaded());

		let shared = shared_cb_name(&root);
		let terrain = case(&root, "vanilla_id", "terrain", None);
		let registered = case(&root, "vanilla_id", "cb", Some(shared.as_str()));
		let rewritten = case(
			&root,
			"vanilla_stat_modified",
			"cb",
			Some(registered.name.as_str()),
		);
		let unseen = case(&root, "overhaul_unseen_cb", "cb", None);
		assert_eq!(registered.name, "conquest");
		assert_eq!(unseen.name, "eldritch_crusade");
		assert_ne!(registered.name, unseen.name);
		assert_ne!(registered.modifiers, rewritten.modifiers);

		let cb = forward.cb().expect("cb head");
		assert_ne!(cb.resolve(&registered.name, registered.modifiers), 0);
		assert_eq!(cb.resolve(&rewritten.name, rewritten.modifiers), 0);
		assert_eq!(cb.resolve(&unseen.name, unseen.modifiers), 0);
		let rewritten_only = cb.score(&rewritten.name, rewritten.modifiers);
		let unseen_only = cb.score(&unseen.name, unseen.modifiers);
		assert_eq!(
			rewritten_only,
			cb.score("overhaul-cb-absent-from-the-table", rewritten.modifiers)
		);
		assert_eq!(
			unseen_only,
			cb.score("overhaul-cb-absent-from-the-table", unseen.modifiers)
		);

		let vanilla = forward.score(
			&terrain.name,
			terrain.modifiers,
			&registered.name,
			registered.modifiers,
		);
		let edited = forward.score(
			&terrain.name,
			terrain.modifiers,
			&rewritten.name,
			rewritten.modifiers,
		);
		let novel = forward.score(
			&terrain.name,
			terrain.modifiers,
			&unseen.name,
			unseen.modifiers,
		);
		assert_ne!(vanilla, edited);
		assert_ne!(vanilla, novel);
		let terrain_mean = mean(
			&forward
				.terrain()
				.expect("terrain")
				.score(&terrain.name, terrain.modifiers),
		);
		assert!((edited - (terrain_mean + mean(&rewritten_only))).abs() < 1.0e-5);
		assert!((novel - (terrain_mean + mean(&unseen_only))).abs() < 1.0e-5);

		let without_cb = [
			terrain_only.score(
				&terrain.name,
				terrain.modifiers,
				&registered.name,
				registered.modifiers,
			),
			terrain_only.score(
				&terrain.name,
				terrain.modifiers,
				&rewritten.name,
				rewritten.modifiers,
			),
			terrain_only.score(
				&terrain.name,
				terrain.modifiers,
				&unseen.name,
				unseen.modifiers,
			),
		];
		assert!(without_cb.windows(2).all(|pair| pair[0] == pair[1]));
		assert!((without_cb[0] - terrain_mean).abs() < 1.0e-5);
	}

	struct Sample {
		name: String,
		modifiers: [f32; 5],
	}

	fn shared_cb_name(root: &Value) -> String {
		let vanilla = cases(root, "vanilla_id", "cb");
		let modified = cases(root, "vanilla_stat_modified", "cb");
		vanilla
			.iter()
			.map(|case| case["name"].as_str().expect("name").to_string())
			.find(|name| {
				modified
					.iter()
					.any(|case| case["name"].as_str() == Some(name.as_str()))
			})
			.expect("a rewritten casus belli keeps its name")
	}

	fn case(root: &Value, distribution: &str, head: &str, name: Option<&str>) -> Sample {
		let found = cases(root, distribution, head)
			.into_iter()
			.find(|case| name.is_none_or(|wanted| case["name"].as_str() == Some(wanted)))
			.expect("fixture case");
		Sample {
			name: found["name"].as_str().expect("name").to_string(),
			modifiers: five(found.get("modifiers").expect("modifiers")),
		}
	}

	fn cases<'a>(root: &'a Value, distribution: &str, head: &str) -> Vec<&'a Value> {
		root["cases"]
			.as_array()
			.expect("cases")
			.iter()
			.filter(|case| {
				case["distribution"].as_str() == Some(distribution)
					&& case["head"].as_str() == Some(head)
			})
			.collect()
	}

	fn five(value: &Value) -> [f32; 5] {
		let flat = numbers(value).expect("five modifiers");
		assert_eq!(flat.len(), MECHANICS.len());
		let mut axes = [0.0f32; 5];
		axes.copy_from_slice(&flat);
		axes
	}
}
