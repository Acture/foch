//! End-to-end offline inference: the committed extraction sample in,
//! GNN fixture weights on, a suggestion out. Everything runs without a
//! game process — the memory edge stays a thin VerifiedGame call in
//! the live path.

use foch_eu4_gnn::GeneralGnn;
use foch_plugin_read_decide::decide::suggest;
use foch_plugin_read_decide::frame::{ArmySnapshot, DynamicProvince, FrameInput, assemble};
use foch_plugin_read_decide::graph::StaticGraph;

const GRAPH: &str = include_str!("data/sample-graph.json");

fn weights_text() -> String {
	let path = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../../src/packages/foch-eu4-gnn/tests/data/parity.json"
	);
	std::fs::read_to_string(path)
		.unwrap_or_else(|e| panic!("gnn parity fixture missing ({e}); regenerate per foch-eu4-gnn"))
}

#[test]
fn the_committed_sample_yields_a_deterministic_suggestion() {
	let graph = StaticGraph::from_json(GRAPH).expect("sample graph");
	assert!(graph.provinces.len() >= 8);

	// Synthetic dynamics: the extraction's baseline development as
	// thousandths, one fort level, our country in control everywhere.
	let dynamics = |id: u32| {
		graph.provinces.get(&id).map(|record| DynamicProvince {
			development_mille: (record.development * 1000) as i32,
			fort_level: 1,
			controller_index: Some(3),
			terrain_name: Some("plains"),
		})
	};
	let input = FrameInput {
		our_index: 3,
		our_tag: "SWE".to_string(),
		enemy_index: Some(6),
		wargoal: Some(1977),
		our_capital: Some(1),
		their_capital: Some(25),
		our: Some(ArmySnapshot {
			province: 1,
			infantry: 14.0,
			cavalry: 4.0,
			artillery: 2.0,
			morale: 0.75,
		}),
		..FrameInput::default()
	};
	let state = assemble(&graph, &dynamics, &input).expect("frame");
	assert_eq!(state.nodes.len(), graph.provinces.len());
	assert!(!state.candidates.is_empty());
	// We stand on Stockholm.
	assert_eq!(graph.id_at(state.here()), Some(1));

	let gnn = GeneralGnn::from_json(&weights_text()).expect("fixture weights");
	let first = suggest(&gnn, &state, 8.0, 0.25).expect("suggestion");
	assert_eq!(first.scores.len(), state.candidates.len());
	assert!(first.scores.iter().all(|s| s.is_finite()));
	assert!(first.sampled < state.candidates.len());
	// Deterministic: same frame, same variate, same record.
	let second = suggest(&gnn, &state, 8.0, 0.25).expect("suggestion");
	assert_eq!(first.scores, second.scores);
	assert_eq!(first.sampled, second.sampled);
}

#[test]
fn the_real_exported_weights_load_and_score_when_present() {
	// A local-only artifact by policy — trained weights stay out of
	// git — exported with `gen_parity.py export CKPT OUT.json`. This
	// documents the skip honestly instead of counting it as run.
	let path = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../../target/territory-gnn-ent-r3.json"
	);
	let Ok(text) = std::fs::read_to_string(path) else {
		eprintln!("skipping: local weight export target/territory-gnn-ent-r3.json not present");
		return;
	};
	let gnn = GeneralGnn::from_json(&text).expect("the real model parses and validates");
	let graph = StaticGraph::from_json(GRAPH).expect("sample graph");
	let dynamics = |id: u32| {
		graph.provinces.get(&id).map(|record| DynamicProvince {
			development_mille: (record.development * 1000) as i32,
			fort_level: 1,
			controller_index: Some(3),
			terrain_name: Some("plains"),
		})
	};
	let input = FrameInput {
		our_index: 3,
		our_tag: "SWE".to_string(),
		our: Some(ArmySnapshot {
			province: 1,
			infantry: 14.0,
			cavalry: 4.0,
			artillery: 2.0,
			morale: 0.75,
		}),
		..FrameInput::default()
	};
	let state = assemble(&graph, &dynamics, &input).expect("frame");
	let suggestion = suggest(&gnn, &state, 8.0, 0.5).expect("real-weights suggestion");
	assert!(suggestion.scores.iter().all(|s| s.is_finite()));
	assert!(suggestion.sampled < state.candidates.len());
}
