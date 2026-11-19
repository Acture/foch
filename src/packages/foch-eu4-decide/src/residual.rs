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
}
