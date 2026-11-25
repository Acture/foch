//! The pointer policy's forward pass in plain Rust.
//!
//! The trainer exports its conv-and-query weights as plain JSON numbers;
//! this module loads them and scores candidates exactly as the Python
//! policy does. Two consumers: the oracle's rollout continuation, which
//! needs the policy inside the world, and - later - the in-game DLL path,
//! which cannot link torch.

/// One 3x3 convolution layer: `weight[out][in][3][3]` plus bias.
struct Conv {
	out: usize,
	input: usize,
	weight: Vec<f32>,
	bias: Vec<f32>,
}

/// One linear layer: `weight[out][in]` plus bias.
struct Linear {
	out: usize,
	input: usize,
	weight: Vec<f32>,
	bias: Vec<f32>,
}

/// The loaded pointer policy.
pub struct PointerPolicy {
	conv1: Conv,
	conv2: Conv,
	query1: Linear,
	query2: Linear,
}

impl PointerPolicy {
	/// Parse the exported weight JSON: an object of nested number arrays.
	pub fn from_json(text: &str) -> Option<Self> {
		let field = |name: &str| -> Option<Vec<f32>> {
			let key = format!("\"{name}\":");
			let start = text.find(&key)? + key.len();
			parse_array(&text[start..])
		};
		let (c1w, c1b) = (field("conv1_weight")?, field("conv1_bias")?);
		let (c2w, c2b) = (field("conv2_weight")?, field("conv2_bias")?);
		let (q1w, q1b) = (field("query1_weight")?, field("query1_bias")?);
		let (q2w, q2b) = (field("query2_weight")?, field("query2_bias")?);
		let conv1 = Conv {
			out: c1b.len(),
			input: c1w.len() / c1b.len() / 9,
			weight: c1w,
			bias: c1b,
		};
		let conv2 = Conv {
			out: c2b.len(),
			input: c2w.len() / c2b.len() / 9,
			weight: c2w,
			bias: c2b,
		};
		let query1 = Linear {
			out: q1b.len(),
			input: q1w.len() / q1b.len(),
			weight: q1w,
			bias: q1b,
		};
		let query2 = Linear {
			out: q2b.len(),
			input: q2w.len() / q2b.len(),
			weight: q2w,
			bias: q2b,
		};
		Some(Self {
			conv1,
			conv2,
			query1,
			query2,
		})
	}

	/// Score each candidate. The board is row-major `(height, width,
	/// channels)`, matching the protocol's cells.
	pub fn scores(
		&self,
		board: &[f32],
		width: usize,
		height: usize,
		rules: &[f32],
		candidates: &[(u32, u32)],
	) -> Vec<f32> {
		let channels = self.conv1.input;
		let mut hidden = conv3x3(&self.conv1, board, channels, width, height);
		conv3x3_in_place(&self.conv2, &mut hidden, width, height);
		let mut query = linear(&self.query1, rules);
		for value in query.iter_mut() {
			*value = (*value).max(0.0);
		}
		// torch's query head has no activation on its last layer.
		let query = linear(&self.query2, &query);
		candidates
			.iter()
			.map(|(x, y)| {
				let x = (*x as usize).min(width - 1);
				let y = (*y as usize).min(height - 1);
				let offset = (y * width + x) * self.conv2.out;
				hidden[offset..offset + self.conv2.out]
					.iter()
					.zip(&query)
					.map(|(cell, q)| cell * q)
					.sum()
			})
			.collect()
	}

	/// The argmax candidate index.
	pub fn pick(
		&self,
		board: &[f32],
		width: usize,
		height: usize,
		rules: &[f32],
		candidates: &[(u32, u32)],
	) -> usize {
		let scores = self.scores(board, width, height, rules, candidates);
		let mut best = 0;
		let mut top = f32::NEG_INFINITY;
		for (index, score) in scores.iter().enumerate() {
			if *score > top {
				top = *score;
				best = index;
			}
		}
		best
	}
}

fn conv3x3(layer: &Conv, input: &[f32], channels: usize, width: usize, height: usize) -> Vec<f32> {
	let mut out = vec![0.0f32; width * height * layer.out];
	conv3x3_into(layer, input, channels, width, height, &mut out);
	out
}

fn conv3x3_in_place(layer: &Conv, grid: &mut Vec<f32>, width: usize, height: usize) {
	let channels = layer.input;
	let input = grid[..width * height * channels].to_vec();
	grid.truncate(0);
	grid.resize(width * height * layer.out, 0.0);
	conv3x3_into(layer, &input, channels, width, height, grid);
}

fn conv3x3_into(
	layer: &Conv,
	input: &[f32],
	channels: usize,
	width: usize,
	height: usize,
	out: &mut [f32],
) {
	for y in 0..height {
		for x in 0..width {
			for (oc, bias) in layer.bias.iter().enumerate() {
				let mut sum = *bias;
				for ic in 0..channels {
					for dy in 0..3usize {
						let sy = y + dy;
						if sy == 0 || sy > height {
							continue;
						}
						for dx in 0..3usize {
							let sx = x + dx;
							if sx == 0 || sx > width {
								continue;
							}
							let at = ((sy - 1) * width + (sx - 1)) * channels + ic;
							let w = layer.weight[((oc * channels + ic) * 3 + dy) * 3 + dx];
							sum += w * input[at];
						}
					}
				}
				out[(y * width + x) * layer.out + oc] = sum.max(0.0);
			}
		}
	}
}

fn linear(layer: &Linear, input: &[f32]) -> Vec<f32> {
	(0..layer.out)
		.map(|oc| {
			layer.bias[oc]
				+ input
					.iter()
					.zip(&layer.weight[oc * layer.input..(oc + 1) * layer.input])
					.map(|(value, weight)| value * weight)
					.sum::<f32>()
		})
		.collect()
}

/// Parse one `[...]` of nested numbers into a flat list.
fn parse_array(text: &str) -> Option<Vec<f32>> {
	let mut depth = 0;
	let mut end = 0;
	let mut values = Vec::new();
	let mut number = String::new();
	for (index, character) in text.char_indices() {
		match character {
			'[' => {
				depth += 1;
				if depth == 1 {
					continue;
				}
			}
			']' => {
				depth -= 1;
				if depth == 0 {
					end = index;
					break;
				}
			}
			'0'..='9' | '-' | '+' | '.' | 'e' | 'E' => number.push(character),
			',' | ' ' | '\n' | '\t' | '\r' => {
				if !number.is_empty() {
					values.push(number.parse().ok()?);
					number.clear();
				}
			}
			_ => return None,
		}
	}
	if !number.is_empty() {
		values.push(number.parse().ok()?);
	}
	let _ = end;
	Some(values)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_two_layer_net_parses_and_scores_finite() {
		let arrays = format!(
			"{{\"conv1_weight\":{:?},\"conv1_bias\":{:?},\"conv2_weight\":{:?},\"conv2_bias\":{:?},\"query1_weight\":{:?},\"query1_bias\":{:?},\"query2_weight\":{:?},\"query2_bias\":{:?}}}",
			vec![0.01f32; 32 * 13 * 9],
			vec![0.0f32; 32],
			vec![0.01f32; 32 * 32 * 9],
			vec![0.0f32; 32],
			vec![0.01f32; 32 * 9],
			vec![0.0f32; 32],
			vec![0.01f32; 32 * 32],
			vec![0.0f32; 32]
		);
		let policy = PointerPolicy::from_json(&arrays).expect("parse");
		let board = vec![0.1f32; 8 * 8 * 13];
		let scores = policy.scores(&board, 8, 8, &[0.5; 9], &[(1, 1), (2, 2)]);
		assert_eq!(scores.len(), 2);
		assert!(scores.iter().all(|value| value.is_finite()));
	}

	#[test]
	fn nested_arrays_flatten_in_order() {
		let values = parse_array("[[1,2],[3,4]]").expect("parse");
		assert_eq!(values, vec![1.0, 2.0, 3.0, 4.0]);
	}
}
