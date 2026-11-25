//! End-to-end offline inference: the committed extraction sample in,
//! GNN fixture weights on, a suggestion out. Everything runs without a
//! game process — the memory edge stays a thin VerifiedGame call in
//! the live path.

use foch_eu4_decide::DecisionForward;
use foch_eu4_gnn::GeneralGnn;
use foch_plugin_read_decide::decide::{residual_telemetry, suggest};
use foch_plugin_read_decide::frame::{ArmySnapshot, DynamicProvince, FrameInput, assemble};
use foch_plugin_read_decide::graph::{self, StaticGraph};
use foch_plugin_read_decide::probe;
use foch_plugin_read_decide::terrain::TerrainCodec;

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
			terrain_bucket: Some(0),
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
	let mut dynamics = dynamics;
	let state = assemble(&graph, &mut dynamics, &input).expect("frame");
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
			terrain_bucket: Some(0),
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
	let mut dynamics = dynamics;
	let state = assemble(&graph, &mut dynamics, &input).expect("frame");
	let suggestion = suggest(&gnn, &state, 8.0, 0.5).expect("real-weights suggestion");
	assert!(suggestion.scores.iter().all(|s| s.is_finite()));
	assert!(suggestion.sampled < state.candidates.len());
}

#[test]
fn an_overhaul_playset_replaces_the_vanilla_map_and_still_scores() {
	let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/probe");
	let game = graph::load_layer(&root.join("game")).expect("game layer");
	let overhaul = graph::load_layer(&root.join("overhaul")).expect("overhaul layer");
	assert_eq!(overhaul.replace_paths, vec!["map".to_string()]);
	let files = graph::resolve_playset(&[game, overhaul]);
	assert!(files.contains_key("history/provinces/1-Alpha.txt"));
	let graph = StaticGraph::from_map_files(&files).expect("playset graph");
	assert!(graph.provinces.contains_key(&10));
	assert!(
		!graph.provinces.contains_key(&1),
		"replace_path=map drops the game provinces"
	);
	assert_eq!(graph.provinces[&10].neighbors, vec![11]);
	assert_eq!(
		(graph.provinces[&10].x, graph.provinces[&10].y),
		(100.0, 200.0)
	);

	let terrain = String::from_utf8(files["map/terrain.txt"].clone()).expect("utf-8");
	let mut codec = TerrainCodec::from_sources(&[terrain]);
	assert_eq!(codec.bucket("haunted_bog"), 4);
	assert!(codec.drain_unknown().is_empty());

	let bucket = codec.bucket("haunted_bog");
	let dynamics = |id: u32| {
		graph.provinces.get(&id).map(|record| DynamicProvince {
			development_mille: (record.development.max(1) * 1000) as i32,
			fort_level: 1,
			controller_index: Some(1500),
			terrain_bucket: Some(bucket),
		})
	};
	let input = FrameInput {
		our_index: 1500,
		our_tag: "SWE".to_string(),
		our: Some(
			ArmySnapshot {
				province: 10,
				infantry: 0.0,
				cavalry: 0.0,
				artillery: 0.0,
				morale: 0.5,
			}
			.with_regiments(foch_eu4_state::view::RegimentCounts {
				infantry: 14,
				cavalry: 0,
				artillery: 0,
			}),
		),
		..FrameInput::default()
	};
	let mut dynamics = dynamics;
	let state = assemble(&graph, &mut dynamics, &input).expect("frame");
	assert_eq!(state.nodes[0][22 + 4], 1.0);
	assert_eq!(graph.id_at(state.here()), Some(10));
	let gnn = GeneralGnn::from_json(&weights_text()).expect("fixture weights");
	let suggestion = suggest(&gnn, &state, 8.0, 0.25).expect("suggestion");
	assert!(suggestion.scores.iter().all(|score| score.is_finite()));
	assert!(suggestion.sampled < state.candidates.len());
}

#[test]
fn the_probe_manifest_declares_the_overhaul_settings() {
	let text = include_str!("../foch-plugin.toml.in");
	for key in [
		"[config.probe]",
		"[config.playset]",
		"[config.weights]",
		"[config.our_index]",
		"max = 32767",
		"@DLL_SHA256@",
	] {
		assert!(text.contains(key), "{key}");
	}
	let config = include_str!("../fixtures/probe/config.json");
	assert!(config.contains("\"our_index\": 1500"));
	assert!(config.contains("fixtures/probe/overhaul"));
}

/// The committed ablation export from the Python trainer: both heads,
/// vanilla and overhaul distributions. The plugin consumes the same
/// document `export_document` writes.
const RESIDUAL_ABLATION: &str =
	include_str!("../../../src/packages/foch-eu4-decide/tests/fixtures/residual_ablation.json");

fn five(value: &serde_json::Value) -> [f32; 5] {
	let flat: Vec<f32> = value
		.as_array()
		.expect("modifiers")
		.iter()
		.map(|item| item.as_f64().expect("number") as f32)
		.collect();
	assert_eq!(flat.len(), 5, "five mechanics");
	[flat[0], flat[1], flat[2], flat[3], flat[4]]
}

#[test]
fn the_real_residual_export_loads_and_scores_vanilla_and_overhaul() {
	let forward = DecisionForward::from_json(RESIDUAL_ABLATION).expect("both heads");
	assert!(
		forward.cb_loaded(),
		"the ablation export carries the cb head"
	);
	let root: serde_json::Value = serde_json::from_str(RESIDUAL_ABLATION).expect("fixture json");
	let cases = root["cases"].as_array().expect("cases");
	assert!(!cases.is_empty());
	let mut distributions = std::collections::BTreeSet::new();
	for case in cases {
		let distribution = case["distribution"].as_str().expect("distribution");
		distributions.insert(distribution);
		let head = case["head"].as_str().expect("head");
		let name = case["name"].as_str().expect("name");
		let modifiers = five(case.get("modifiers").expect("modifiers"));
		let expected = case["score"].as_array().expect("score");
		let model = match head {
			"terrain" => forward.terrain().expect("terrain head"),
			"cb" => forward.cb().expect("cb head"),
			other => panic!("unknown head {other}"),
		};
		let got = model.score(name, modifiers);
		assert_eq!(got.len(), expected.len(), "{distribution} {head} {name}");
		for (index, (left, right)) in got.iter().zip(expected).enumerate() {
			let right = right.as_f64().expect("number") as f32;
			assert!(
				(left - right).abs() < 1.0e-5,
				"{distribution} {head} {name}[{index}]: {left} vs {right}"
			);
		}
	}
	for required in [
		"vanilla_id",
		"vanilla_stat_modified",
		"overhaul_unseen_terrain",
		"overhaul_unseen_cb",
	] {
		assert!(distributions.contains(required), "missing {required}");
	}
}

#[test]
fn residual_telemetry_stays_finite_across_vanilla_and_overhaul_names() {
	let forward = DecisionForward::from_json(RESIDUAL_ABLATION).expect("both heads");
	let root: serde_json::Value = serde_json::from_str(RESIDUAL_ABLATION).expect("fixture json");
	let first = |distribution: &str, head: &str| {
		root["cases"]
			.as_array()
			.expect("cases")
			.iter()
			.find(|case| {
				case["distribution"].as_str() == Some(distribution)
					&& case["head"].as_str() == Some(head)
			})
			.expect("fixture case")
			.clone()
	};
	let vanilla_terrain = first("vanilla_id", "terrain");
	let vanilla_cb = first("vanilla_id", "cb");
	let telemetry = residual_telemetry(
		&forward,
		Some((
			vanilla_terrain["name"].as_str().expect("name"),
			five(&vanilla_terrain["modifiers"]),
		)),
		Some((
			vanilla_cb["name"].as_str().expect("name"),
			five(&vanilla_cb["modifiers"]),
		)),
	);
	assert!(telemetry.residual_loaded);
	assert!(telemetry.residual_cb_loaded);
	let score = telemetry.top_terrain_residual.expect("vanilla scalar");
	assert!(score.is_finite());
	// The scalar decomposes into the head means, matching the trainer:
	// mean(f_t + E_t) + mean(f_c + E_c).
	let mean_of = |model: &foch_eu4_decide::ResidualCategory, case: &serde_json::Value| {
		let vector = model.score(
			case["name"].as_str().expect("name"),
			five(case.get("modifiers").expect("modifiers")),
		);
		vector.iter().sum::<f32>() / vector.len() as f32
	};
	let terrain_mean = mean_of(forward.terrain().expect("terrain head"), &vanilla_terrain);
	let cb_mean = mean_of(forward.cb().expect("cb head"), &vanilla_cb);
	assert!((score - (terrain_mean + cb_mean)).abs() < 1.0e-5);

	// An overhaul playset's unseen terrain and casus belli stay finite:
	// E_id = 0, the mechanics row still scores.
	let unseen_terrain = first("overhaul_unseen_terrain", "terrain");
	let unseen_cb = first("overhaul_unseen_cb", "cb");
	let overhaul = residual_telemetry(
		&forward,
		Some((
			unseen_terrain["name"].as_str().expect("name"),
			five(&unseen_terrain["modifiers"]),
		)),
		Some((
			unseen_cb["name"].as_str().expect("name"),
			five(&unseen_cb["modifiers"]),
		)),
	);
	assert!(overhaul.residual_loaded);
	assert!(overhaul.residual_cb_loaded);
	assert!(overhaul.top_terrain_residual.is_some_and(f32::is_finite));
}

#[test]
fn residual_weights_config_validates_and_refuses_traversal() {
	let absent = probe::parse_config(
		"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":3}",
	)
	.expect("valid config")
	.expect("probe requested");
	assert_eq!(absent.residual_weights, "", "absent means disabled");
	let valid = probe::parse_config(
		"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"residual_weights\":\"residual.json\",\"our_tag\":\"SWE\",\"our_index\":3}",
	)
	.expect("valid config")
	.expect("probe requested");
	assert_eq!(valid.residual_weights, "residual.json");
	assert!(
		probe::parse_config(
			"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"residual_weights\":\"../escape.json\",\"our_tag\":\"SWE\",\"our_index\":3}",
		)
		.is_err(),
		"traversal in residual_weights must refuse"
	);
}
