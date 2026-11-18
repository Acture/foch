//! Parity against the torch/torch_geometric reference, on the
//! committed deterministic fixtures.
//!
//! The fixture is part of the acceptance: if it is missing the test
//! FAILS (with regeneration instructions), it never silently passes.
//! Every compared value is asserted finite on BOTH sides before the
//! difference is taken, and states/candidates are asserted non-empty —
//! a NaN on either side cannot slip through a `max` reduction.

use std::path::Path;

use foch_eu4_gnn::GeneralGnn;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
	states: Vec<State>,
}

#[derive(Deserialize)]
struct State {
	nodes: Vec<Vec<f32>>,
	/// Directed (src, dst) pairs, flat. Includes one explicit self-loop
	/// in the first graph; the Rust side's remove-then-add semantics
	/// must treat it exactly as torch_geometric does.
	edges: Vec<u32>,
	rules: Vec<f32>,
	cands: Vec<usize>,
	scores: Vec<f32>,
}

#[test]
fn rust_forward_matches_the_reference_fixtures() {
	let path = Path::new("tests/data/parity.json");
	assert!(
		path.exists(),
		"missing parity fixture: regenerate with \
		 src/tools/eu4-decide/.venv/Scripts/python \
		 src/packages/foch-eu4-gnn/fixtures/gen_parity.py fixture \
		 src/packages/foch-eu4-gnn/tests/data/parity.json"
	);
	let text = std::fs::read_to_string(path).expect("fixture readable");
	let gnn = GeneralGnn::from_json(&text).expect("fixture weights parse and validate");

	let fixture: Fixture = serde_json::from_str(&text).expect("fixture parses");
	assert!(!fixture.states.is_empty(), "fixture carries no graphs");

	for (index, state) in fixture.states.iter().enumerate() {
		assert!(!state.cands.is_empty(), "graph {index}: no candidates");
		assert_eq!(
			state.cands.len(),
			state.scores.len(),
			"graph {index}: score count"
		);
		assert_eq!(
			state.edges.len() % 2,
			0,
			"graph {index}: edges come in pairs"
		);

		let mut nodes = Vec::with_capacity(state.nodes.len() * 29);
		for row in &state.nodes {
			nodes.extend_from_slice(row);
		}
		// Directed protocol: each stored pair is one PyG edge — push it
		// exactly once, never mirrored again.
		let edges: Vec<(u32, u32)> = state
			.edges
			.chunks_exact(2)
			.map(|pair| (pair[0], pair[1]))
			.collect();

		let scores = gnn.scores(&nodes, &edges, &state.rules, &state.cands);
		for (rust, expected) in scores.iter().zip(&state.scores) {
			assert!(
				rust.is_finite(),
				"graph {index}: rust produced a non-finite score"
			);
			assert!(
				expected.is_finite(),
				"graph {index}: fixture holds a non-finite score"
			);
			assert!(
				(rust - expected).abs() < 1e-4,
				"graph {index}: rust {rust} vs reference {expected}"
			);
		}
	}
}

#[test]
fn malformed_weights_are_refused_not_panicked() {
	// The exact input from the external review that reproduced the
	// division-by-zero panic.
	let review = r#"{"gat1.att":[],"gat1.lin_l.weight":[],"gat1.lin_l.bias":[]}"#;
	assert!(GeneralGnn::from_json(review).is_none());
	for text in [
		"",
		"{}",
		r#"{"gat1.att":[0.0,0.0]}"#,
		r#"{"gat1.att":[1.0,2.0,3.0],"gat1.bias":[0.0],"gat1.lin_l.weight":[0.0],"gat1.lin_l.bias":[0.0,0.0],"gat1.lin_r.weight":[0.0],"gat1.lin_r.bias":[0.0,0.0]}"#,
	] {
		assert!(GeneralGnn::from_json(text).is_none(), "must refuse: {text}");
	}
}
