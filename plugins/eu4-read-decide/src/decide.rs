//! Scoring and suggestion output — read-only by construction.
//!
//! The GNN scores the candidate provinces; this module turns scores
//! into a suggestion record for the log. Choice policy (argmax vs
//! temperature sampling) is the caller's: the record carries both the
//! full ranking and one sampled pick under an explicit temperature,
//! and nothing here writes game memory or takes any action.

use foch_eu4_decide::DecisionForward;
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

/// Residual-head telemetry for one suggestion receipt. Every number is
/// finite or its slot is `None`: telemetry never logs a NaN.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResidualTelemetry {
	/// The residual document loaded (terrain head at minimum).
	pub residual_loaded: bool,
	/// The document also carries the casus belli head.
	pub residual_cb_loaded: bool,
	/// The residual scalar of the picked candidate's terrain, when the
	/// terrain was known and scored finite.
	pub top_terrain_residual: Option<f32>,
}

/// Score one candidate's terrain — plus the war's casus belli when the
/// forward carries that head and the caller can name it — through the
/// residual decision forward. Pure and read-only: no game memory, no
/// hidden state, and `None` instead of a non-finite score.
pub fn score_residual_candidate(
	forward: &DecisionForward,
	terrain_name: &str,
	terrain_axes: [f32; 5],
	cb: Option<(&str, [f32; 5])>,
) -> Option<f32> {
	let (cb_name, cb_axes) = cb.unwrap_or(("", [0.0; 5]));
	let score = forward.score(terrain_name, terrain_axes, cb_name, cb_axes);
	score.is_finite().then_some(score)
}

/// The receipt telemetry for a loaded residual forward. `cb` is the
/// war's known casus belli, if any; without one a loaded cb head
/// scores its neutral-mechanics row, the same contract as
/// [`crate::frame::WarFacts::default`].
pub fn residual_telemetry(
	forward: &DecisionForward,
	top_terrain: Option<(&str, [f32; 5])>,
	cb: Option<(&str, [f32; 5])>,
) -> ResidualTelemetry {
	ResidualTelemetry {
		residual_loaded: true,
		residual_cb_loaded: forward.cb_loaded(),
		top_terrain_residual: top_terrain
			.and_then(|(name, axes)| score_residual_candidate(forward, name, axes, cb)),
	}
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

	/// Width-2 terrain-only forward; embedding row 1 is [1, 0].
	fn residual_forward() -> DecisionForward {
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
		DecisionForward::from_json(text).expect("terrain-only forward")
	}

	#[test]
	fn residual_telemetry_reports_the_document_shape_and_finite_scores() {
		let forward = residual_forward();
		let telemetry =
			residual_telemetry(&forward, Some(("plains", [1.0, 0.0, 0.0, 4.0, 0.0])), None);
		assert!(telemetry.residual_loaded);
		assert!(!telemetry.residual_cb_loaded);
		assert_eq!(telemetry.top_terrain_residual, Some(1.0));

		// An unseen category scores through f(mechanics) with E_id = 0
		// and stays finite.
		let unseen = residual_telemetry(
			&forward,
			Some(("haunted_bog", [3.0, 1.0, 0.0, 1.0, 2.0])),
			None,
		);
		assert_eq!(unseen.top_terrain_residual, Some(2.0));

		// No terrain known: the score slot stays empty, never NaN.
		let unknown = residual_telemetry(&forward, None, None);
		assert_eq!(unknown.top_terrain_residual, None);
		assert!(unknown.residual_loaded);
	}

	#[test]
	fn score_residual_candidate_never_emits_a_non_finite_number() {
		let forward = residual_forward();
		let score = score_residual_candidate(&forward, "plains", [1.0, 0.0, 0.0, 4.0, 0.0], None);
		assert_eq!(score, Some(1.0));
		// The forward's ReLU absorbs a NaN mechanic into 0.0; the
		// finiteness guard keeps any non-finite result from reaching
		// the receipt either way.
		let nan_axes = [f32::NAN, 0.0, 0.0, 4.0, 0.0];
		let score = score_residual_candidate(&forward, "plains", nan_axes, None);
		assert!(score.is_none_or(f32::is_finite), "{score:?}");
	}
}
