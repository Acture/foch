//! Scoring and suggestion output — read-only by construction.
//!
//! The GNN scores the candidate provinces; this module turns scores
//! into a suggestion record for the log. Choice policy (argmax vs
//! temperature sampling) is the caller's: the record carries both the
//! full ranking and one sampled pick under an explicit temperature,
//! and nothing here writes game memory or takes any action.

use foch_eu4_gnn::GeneralGnn;
use foch_eu4_state::plane::State;

/// One decision's suggestion.
#[derive(Clone, Debug)]
pub struct Suggestion {
	/// Candidate NODE indices (u32, the plane contract's addressing);
	/// the caller maps them back to province ids through the graph.
	pub candidates: Vec<u32>,
	/// One score per candidate, same order.
	pub scores: Vec<f32>,
	/// The argmax index into candidates.
	pub argmax: usize,
	/// The temperature-sampled index into candidates.
	pub sampled: usize,
	/// The sampling temperature used.
	pub temperature: f32,
}

/// Score a frame. `unit` is the uniform variiate for the sampled pick
/// (`[0,1)`), so tests stay deterministic and the live caller owns
/// its randomness.
pub fn suggest(gnn: &GeneralGnn, state: &State, temperature: f32, unit: f32) -> Option<Suggestion> {
	if state.candidates.is_empty() || state.nodes.is_empty() {
		return None;
	}
	let cands: Vec<usize> = state.candidates.iter().map(|&c| c as usize).collect();
	let scores = gnn.scores(&state.nodes_flat(), &state.edges, &state.rules, &cands);
	if scores.iter().any(|s| !s.is_finite()) {
		return None;
	}
	let argmax = scores
		.iter()
		.enumerate()
		.max_by(|a, b| a.1.total_cmp(b.1))
		.map(|(i, _)| i)?;
	let sampled = sample_pick(&scores, temperature, unit);
	Some(Suggestion {
		candidates: state.candidates.clone(),
		scores,
		argmax,
		sampled,
		temperature,
	})
}

/// Temperature sampling over scores with one caller-owned uniform
/// variate — the choice policy stays in this crate (the forward crate
/// only scores). Temperature <= 0.01 degenerates to argmax.
fn sample_pick(scores: &[f32], temperature: f32, unit: f32) -> usize {
	if temperature <= 0.01 || scores.is_empty() {
		return scores
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.total_cmp(b.1))
			.map(|(i, _)| i)
			.unwrap_or(0);
	}
	let top = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
	let weights: Vec<f32> = scores
		.iter()
		.map(|s| ((s - top) / temperature).exp())
		.collect();
	let total: f32 = weights.iter().sum();
	let mut walked = 0.0f32;
	for (index, weight) in weights.iter().enumerate() {
		walked += weight / total;
		if unit < walked {
			return index;
		}
	}
	weights.len() - 1
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_eu4_state::plane::{NODE, RULES};

	/// The GNN crate's committed parity fixture doubles as deterministic
	/// weights for end-to-end inference tests.
	fn fixture_gnn() -> GeneralGnn {
		let path = concat!(
			env!("CARGO_MANIFEST_DIR"),
			"/../../src/packages/foch-eu4-gnn/tests/data/parity.json"
		);
		let text = std::fs::read_to_string(path)
			.unwrap_or_else(|e| panic!("fixture missing ({e}); regenerate per foch-eu4-gnn"));
		GeneralGnn::from_json(&text).expect("fixture weights")
	}

	fn frame() -> State {
		let mut nodes = vec![[0.0f32; NODE]; 3];
		nodes[0] = [1.0; NODE];
		nodes[1][0] = 0.5;
		nodes[2][1] = 0.7;
		State {
			nodes,
			edges: vec![(0, 1), (1, 0), (0, 2), (2, 0)],
			rules: [0.25; RULES],
			candidates: vec![1, 2],
		}
	}

	#[test]
	fn scores_rank_and_sample_within_candidates() {
		let gnn = fixture_gnn();
		let state = frame();
		let suggestion = suggest(&gnn, &state, 8.0, 0.0).expect("suggestion");
		assert_eq!(suggestion.candidates, vec![1, 2]);
		assert_eq!(suggestion.scores.len(), 2);
		assert!(suggestion.scores.iter().all(|s| s.is_finite()));
		assert!(suggestion.argmax < 2);
		assert!(suggestion.sampled < 2);
		// Deterministic given the unit variate.
		assert_eq!(
			suggest(&gnn, &state, 8.0, 0.0).unwrap().scores,
			suggestion.scores
		);
	}

	#[test]
	fn empty_frames_refuse() {
		let gnn = fixture_gnn();
		let empty = State::default();
		assert!(suggest(&gnn, &empty, 8.0, 0.5).is_none());
	}
}
