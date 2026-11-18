//! The province-graph GNN's forward pass in plain Rust.
//!
//! Mirrors the reference module exactly: two GATv2 layers (heads=2,
//! concat=False), a rules MLP, and a pointer head scoring
//! `[candidate ‖ our position ‖ query]`. Weights arrive as the flat
//! JSON the fixture generator / exporter writes; [`GeneralGnn::from_json`]
//! validates shape (non-empty layers, divisible lengths, matching
//! dimensions, finite values) and refuses malformed input with `None`
//! rather than panicking — this runs where a crash takes the host down
//! with it.
//!
//! Edge protocol: `edges` is a list of DIRECTED (src, dst) pairs in
//! PyG edge_index order — a symmetric adjacency lists both directions
//! explicitly. Self-loops follow torch_geometric's
//! remove-then-add semantics: any self-loop in the input is dropped
//! and exactly one is added per node, so input self-loops never
//! double-count.
//!
//! Choosing among candidates (argmax vs sampling, and any temperature)
//! is deployment policy owned by the caller; this module only scores.

/// Observables per province row — the plane contract this net reads.
pub const NODE: usize = 29;
/// War-level rule features.
pub const RULES: usize = 26;
/// Attention heads per GATv2 layer, as in the reference module.
pub const HEADS: usize = 2;
const NEGATIVE_SLOPE: f32 = 0.2;

/// One linear layer, torch layout: `weight[out][in]` row-major plus bias.
struct Linear {
	out: usize,
	input: usize,
	weight: Vec<f32>,
	bias: Vec<f32>,
}

impl Linear {
	fn parse(field: &dyn Fn(&str) -> Option<Vec<f32>>, prefix: &str) -> Option<Self> {
		let weight = field(&format!("{prefix}.weight"))?;
		let bias = field(&format!("{prefix}.bias"))?;
		Self::from_parts(weight, bias)
	}

	fn from_parts(weight: Vec<f32>, bias: Vec<f32>) -> Option<Self> {
		// Empty or non-divisible shapes would divide by zero or slice
		// out of bounds later; non-finite weights poison every score.
		if bias.is_empty() || !weight.len().is_multiple_of(bias.len()) {
			return None;
		}
		if !weight.iter().all(|v| v.is_finite()) || !bias.iter().all(|v| v.is_finite()) {
			return None;
		}
		Some(Self {
			out: bias.len(),
			input: weight.len() / bias.len(),
			weight,
			bias,
		})
	}

	fn apply(&self, input: &[f32]) -> Vec<f32> {
		(0..self.out)
			.map(|row| {
				let mut sum = self.bias[row];
				let w = &self.weight[row * self.input..(row + 1) * self.input];
				for (value, weight) in input.iter().zip(w) {
					sum += value * weight;
				}
				sum
			})
			.collect()
	}
}

/// One GATv2 layer exactly as torch_geometric computes it: per edge
/// `att · LeakyReLU(lin_l·src + lin_r·dst)`, softmax per target node
/// (self-loop included), messages carry `lin_l·src`, heads are
/// averaged, then the layer bias is added. The per-head channel count
/// derives from the attention vector's length instead of being pinned,
/// so small test fixtures and the full model share one code path.
struct GatV2 {
	lin_l: Linear,
	lin_r: Linear,
	/// `[head][channels]` attention vector.
	att: Vec<f32>,
	bias: Vec<f32>,
}

impl GatV2 {
	fn parse(field: &dyn Fn(&str) -> Option<Vec<f32>>, prefix: &str) -> Option<Self> {
		let att = field(&format!("{prefix}.att"))?;
		let bias = field(&format!("{prefix}.bias"))?;
		let layer = Self {
			lin_l: Linear::parse(field, &format!("{prefix}.lin_l"))?,
			lin_r: Linear::parse(field, &format!("{prefix}.lin_r"))?,
			att,
			bias,
		};
		layer.validate()?;
		Some(layer)
	}

	/// Output width of this layer: the per-head channel count.
	fn channels(&self) -> usize {
		self.att.len() / HEADS
	}

	fn validate(&self) -> Option<()> {
		if self.att.is_empty()
			|| !self.att.len().is_multiple_of(HEADS)
			|| !self.att.iter().all(|v| v.is_finite())
		{
			return None;
		}
		if self.bias.len() != self.channels() || !self.bias.iter().all(|v| v.is_finite()) {
			return None;
		}
		// Both projections see the same node features and must produce
		// every head's channels — a mismatched lin_r width would zip
		// silently and score wrong instead of refusing.
		(self.lin_l.out == self.att.len()
			&& self.lin_r.out == self.att.len()
			&& self.lin_l.input == self.lin_r.input)
			.then_some(())
	}

	/// `nodes` is `[n][input]` row-major; `edges` DIRECTED (src, dst).
	/// Explicit per-head indexing mirrors the softmax bookkeeping.
	#[allow(clippy::needless_range_loop)]
	fn propagate(&self, nodes: &[f32], n: usize, edges: &[(u32, u32)]) -> Vec<f32> {
		let input = self.lin_l.input;
		let xl: Vec<Vec<f32>> = (0..n)
			.map(|i| self.lin_l.apply(&nodes[i * input..(i + 1) * input]))
			.collect();
		let xr: Vec<Vec<f32>> = (0..n)
			.map(|i| self.lin_r.apply(&nodes[i * input..(i + 1) * input]))
			.collect();

		// Incoming edges per target, self-loop first. Input self-loops
		// are dropped: torch_geometric removes them before adding its
		// own, and duplicating one would skew the softmax.
		let mut incoming: Vec<Vec<u32>> = (0..n).map(|i| vec![i as u32]).collect();
		for &(s, t) in edges {
			if s != t {
				incoming[t as usize].push(s);
			}
		}

		let channels = self.channels();
		let mut out = vec![0.0f32; n * channels];
		for t in 0..n {
			let sources = &incoming[t];
			let mut raw = vec![vec![0.0f32; HEADS]; sources.len()];
			for (slot, &s) in sources.iter().enumerate() {
				for head in 0..HEADS {
					let base = head * channels;
					let mut dot = 0.0f32;
					for c in 0..channels {
						let v = xl[s as usize][base + c] + xr[t][base + c];
						let v = if v < 0.0 { NEGATIVE_SLOPE * v } else { v };
						dot += v * self.att[base + c];
					}
					raw[slot][head] = dot;
				}
			}
			let mut alpha = vec![vec![0.0f32; HEADS]; sources.len()];
			for head in 0..HEADS {
				let top = raw
					.iter()
					.map(|r| r[head])
					.fold(f32::NEG_INFINITY, f32::max);
				let sum: f32 = raw.iter().map(|r| (r[head] - top).exp()).sum();
				for (slot, r) in raw.iter().enumerate() {
					alpha[slot][head] = (r[head] - top).exp() / sum;
				}
			}
			for c in 0..channels {
				let mut total = 0.0f32;
				for head in 0..HEADS {
					let base = head * channels;
					let mut acc = 0.0f32;
					for (slot, &s) in sources.iter().enumerate() {
						acc += alpha[slot][head] * xl[s as usize][base + c];
					}
					total += acc;
				}
				out[t * channels + c] = total / HEADS as f32 + self.bias[c];
			}
		}
		out
	}
}

/// Structural descriptor of a JSON number tree: (nesting depth, leaf
/// count). Siblings must share it — a ragged `[[30], [26]]` refuses
/// instead of flattening into a silently wrong matrix.
fn shape(value: &serde_json::Value) -> Option<(u8, usize)> {
	match value {
		serde_json::Value::Number(_) => Some((0, 1)),
		serde_json::Value::Array(items) => {
			let described: Vec<(u8, usize)> = items.iter().map(shape).collect::<Option<_>>()?;
			let first = *described.first()?;
			if described.iter().any(|d| *d != first) {
				return None;
			}
			Some((first.0 + 1, first.1 * described.len()))
		}
		_ => None,
	}
}

/// Flatten a structurally validated number tree in row-major order.
/// torch tensors arrive 2-deep (`[out][in]`) except the attention
/// vector, which is 3-deep (`[1][heads][channels]`); both flatten to
/// the layout the forward pass slices.
fn numbers(value: &serde_json::Value) -> Option<Vec<f32>> {
	match value {
		serde_json::Value::Number(n) => n.as_f64().map(|f| vec![f as f32]),
		serde_json::Value::Array(items) => {
			let mut out = Vec::with_capacity(items.len());
			for item in items {
				out.extend(numbers(item)?);
			}
			Some(out)
		}
		_ => None,
	}
}

/// The loaded province-graph policy.
pub struct GeneralGnn {
	gat1: GatV2,
	gat2: GatV2,
	query1: Linear,
	query2: Linear,
	head1: Linear,
	head2: Linear,
}

/// ReLU on layer outputs — the reference uses `F.relu` between layers;
/// the leaky variant lives only inside the attention logits.
fn relu_grid(values: &[f32]) -> Vec<f32> {
	values.iter().map(|&v| v.max(0.0)).collect()
}

impl GeneralGnn {
	/// Parse and validate the exported weight JSON: an object of nested
	/// number arrays, either at the root (the exporter's format) or
	/// under a `"weights"` key (the fixture format). The whole document
	/// must be valid JSON — a truncated file is refused, while legally
	/// whitespace-only-but-empty objects simply lack the fields.
	/// Malformed weights — missing fields, empty or non-divisible
	/// shapes, non-finite values, or layers whose widths do not chain
	/// (`gat2` input must equal `gat1`'s output width, both
	/// projections must share an input width, the head must take three
	/// `gat2`-width blocks and end in one output, the query must start
	/// at `RULES`) — yield `None`, never a panic and never a silently
	/// zip-truncated wrong score.
	pub fn from_json(text: &str) -> Option<Self> {
		let root: serde_json::Value = serde_json::from_str(text).ok()?;
		let container = root.get("weights").unwrap_or(&root);
		let field = |name: &str| -> Option<Vec<f32>> {
			let value = container.get(name)?;
			shape(value)?; // ragged or non-numeric trees refuse here
			numbers(value)
		};
		let gnn = Self {
			gat1: GatV2::parse(&field, "gat1")?,
			gat2: GatV2::parse(&field, "gat2")?,
			query1: Linear::parse(&field, "query.0")?,
			query2: Linear::parse(&field, "query.2")?,
			head1: Linear::parse(&field, "head.0")?,
			head2: Linear::parse(&field, "head.2")?,
		};
		gnn.validate().then_some(gnn)
	}

	fn validate(&self) -> bool {
		self.gat1.lin_l.input == NODE
			&& self.gat2.lin_l.input == self.gat1.channels()
			&& self.query1.input == RULES
			&& self.query2.input == self.query1.out
			&& self.query2.out == self.gat2.channels()
			&& self.head1.input == self.gat2.channels() * 3
			&& self.head2.input == self.head1.out
			&& self.head2.out == 1
	}

	/// Score each candidate. `nodes` is `[n][NODE]` row-major, `edges`
	/// a DIRECTED (src, dst) list, `rules` the 26-column rules row,
	/// `cands` node indices. Our position is column 8's argmax, exactly
	/// as the reference forward does.
	pub fn scores(
		&self,
		nodes: &[f32],
		edges: &[(u32, u32)],
		rules: &[f32],
		cands: &[usize],
	) -> Vec<f32> {
		let n = nodes.len() / NODE;
		let mut h = relu_grid(&self.gat1.propagate(nodes, n, edges));
		h = relu_grid(&self.gat2.propagate(&h, n, edges));
		let mut here = 0usize;
		let mut top = f32::NEG_INFINITY;
		for i in 0..n {
			let v = nodes[i * NODE + 8];
			if v > top {
				top = v;
				here = i;
			}
		}
		let q = relu_grid(&self.query1.apply(&rules[..RULES]));
		let q = self.query2.apply(&q);
		let width = self.gat2.channels();
		cands
			.iter()
			.map(|&c| {
				let mut joined = vec![0.0f32; width * 3];
				joined[..width].copy_from_slice(&h[c * width..(c + 1) * width]);
				joined[width..width * 2].copy_from_slice(&h[here * width..(here + 1) * width]);
				joined[width * 2..].copy_from_slice(&q);
				let hidden = relu_grid(&self.head1.apply(&joined));
				self.head2.apply(&hidden)[0]
			})
			.collect()
	}

	/// The argmax candidate index.
	pub fn pick(
		&self,
		nodes: &[f32],
		edges: &[(u32, u32)],
		rules: &[f32],
		cands: &[usize],
	) -> usize {
		self.scores(nodes, edges, rules, cands)
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.total_cmp(b.1))
			.map(|(index, _)| index)
			.unwrap_or(0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn malformed_empty_bias_is_refused_without_panicking() {
		let data = r#"{"gat1.att":[],"gat1.lin_l.weight":[],"gat1.lin_l.bias":[]}"#;
		assert!(GeneralGnn::from_json(data).is_none());
	}

	#[test]
	fn a_two_layer_net_parses_and_scores_finite() {
		let arrays = format!(
			"{{\"gat1.att\":{:?},\"gat1.bias\":{:?},\"gat1.lin_l.weight\":{:?},\"gat1.lin_l.bias\":{:?},\"gat1.lin_r.weight\":{:?},\"gat1.lin_r.bias\":{:?},\"gat2.att\":{:?},\"gat2.bias\":{:?},\"gat2.lin_l.weight\":{:?},\"gat2.lin_l.bias\":{:?},\"gat2.lin_r.weight\":{:?},\"gat2.lin_r.bias\":{:?},\"query.0.weight\":{:?},\"query.0.bias\":{:?},\"query.2.weight\":{:?},\"query.2.bias\":{:?},\"head.0.weight\":{:?},\"head.0.bias\":{:?},\"head.2.weight\":{:?},\"head.2.bias\":{:?}}}",
			vec![0.01f32; HEADS * 4],
			vec![0.0f32; 4],
			vec![0.01f32; HEADS * 4 * NODE],
			vec![0.0f32; HEADS * 4],
			vec![0.01f32; HEADS * 4 * NODE],
			vec![0.0f32; HEADS * 4],
			vec![0.02f32; HEADS * 4],
			vec![0.0f32; 4],
			vec![0.01f32; HEADS * 4 * 4],
			vec![0.0f32; HEADS * 4],
			vec![0.01f32; HEADS * 4 * 4],
			vec![0.0f32; HEADS * 4],
			vec![0.01f32; 4 * RULES],
			vec![0.0f32; 4],
			vec![0.01f32; 4 * 4],
			vec![0.0f32; 4],
			vec![0.01f32; 4 * 12],
			vec![0.0f32; 4],
			vec![0.01f32; 4],
			vec![0.0f32; 1],
		);
		let gnn = GeneralGnn::from_json(&arrays).expect("parse");
		let mut nodes = vec![0.0f32; 3 * NODE];
		nodes[NODE] = 1.0;
		let edges = vec![(0, 1), (1, 0), (1, 2), (2, 1)];
		let scores = gnn.scores(&nodes, &edges, &[0.5; RULES], &[0, 2]);
		assert_eq!(scores.len(), 2);
		assert!(scores.iter().all(|v| v.is_finite()));
		assert_eq!(gnn.scores(&nodes, &edges, &[0.5; RULES], &[0, 2]), scores);
	}
}
