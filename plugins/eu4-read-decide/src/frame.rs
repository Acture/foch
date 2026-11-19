//! Frame assembly: static graph + dynamic reads -> the 29-column plane.
//!
//! Everything here is a pure function over VALUES — the memory edge
//! (VerifiedGame) stays thin and untestable offline by design. Columns
//! the live path cannot fill yet are explicit below: army composition
//! and morale arrive as [`FrameInput`] snapshots for now (regiment RE
//! pending), terrain one-hots and supply stay zero until the template
//! reads land, and a siege column of zero means "no siege in progress"
//! exactly as the simulator encodes it.

use foch_eu4_state::plane::{NODE, RULES, State};

/// The simulator's terrain buckets, column order 22..26.
const TERRAIN_SLOTS: [&str; 5] = ["plains", "forest", "hills", "mountains", "marsh"];
/// Real map terrain names folded into a simulator bucket verbatim —
/// extend only with a training-distribution decision, never silently.
const TERRAIN_ALIASES: [(&str, usize); 0] = [];

use crate::graph::StaticGraph;

/// One province's dynamic fields, already extracted through the
/// gated readers.
#[derive(Clone, Copy, Debug, Default)]
pub struct DynamicProvince {
	/// Development thousandths, (tax + production + manpower) as one
	/// number — the caller sums the three ledger reads.
	pub development_mille: i32,
	/// Fort level.
	pub fort_level: i32,
	/// Controlling country index, when read.
	pub controller_index: Option<i32>,
	/// Terrain display name ("hills", "marsh", ...). The frame maps
	/// exactly the simulator's five terrain names; anything else
	/// refuses the frame rather than guessing a bucket.
	pub terrain_name: Option<&'static str>,
}

/// Our stack for this decision, from the caller's snapshot.
#[derive(Clone, Copy, Debug)]
pub struct ArmySnapshot {
	pub province: u32,
	pub infantry: f32,
	pub cavalry: f32,
	pub artillery: f32,
	pub morale: f32,
}

/// War-level facts for the rules row, mapped one-to-one onto the
/// simulator's 26-slot layout. Fields the live path cannot fill yet
/// keep the simulator's neutral encoding and are listed in
/// [`rules_row`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WarFacts {
	/// Campaign day over the simulator's five-year horizon (1825).
	pub day: Option<i32>,
	/// Fraction of graph provinces we control.
	pub our_held_fraction: Option<f32>,
	/// Fraction the enemy controls.
	pub their_held_fraction: Option<f32>,
	/// Clamped engine warscore total (candidate read ÷ engine scale).
	pub war_score: Option<i32>,
	/// Manpower pool fraction (candidate country reads).
	pub manpower_fraction: Option<f32>,
	/// We are the declaring side.
	pub we_declared: bool,
	/// A conquest wargoal province exists.
	pub has_wargoal: bool,
}

/// The decision's inputs beyond the per-province dynamics.
#[derive(Clone, Debug, Default)]
pub struct FrameInput {
	/// Country index we play as (drives the ours/theirs/neutral split).
	pub our_index: i32,
	/// Our tag as written in the extraction (drives the baseline-owner
	/// column 7).
	pub our_tag: String,
	/// Enemy country index for the war, when one is on.
	pub enemy_index: Option<i32>,
	/// Where we stood before this march, if anywhere.
	pub our_last: Option<u32>,
	/// The war goal province, if set.
	pub wargoal: Option<u32>,
	/// Our capital province.
	pub our_capital: Option<u32>,
	/// Their capital province.
	pub their_capital: Option<u32>,
	/// Our stack.
	pub our: Option<ArmySnapshot>,
	/// War-level facts for the rules row.
	pub war: WarFacts,
}

/// Assemble the 29-column frame. Missing dynamics for a province are
/// an error — the plane never fabricates a value — except where the
/// simulator itself encodes an absence (armies, sieges).
pub fn assemble(
	graph: &StaticGraph,
	dynamics: &dyn Fn(u32) -> Option<DynamicProvince>,
	input: &FrameInput,
) -> Option<State> {
	let count = graph.provinces.len();
	let mut nodes = vec![[0.0f32; NODE]; count];
	let our = input.our?;
	let our_index = graph.index_of(our.province)?;

	for (slot, (&id, record)) in graph.provinces.iter().enumerate() {
		let dynamic = dynamics(id)?;
		let row = &mut nodes[slot];
		row[0] = record.x / 5000.0;
		row[1] = record.y / 5000.0;
		row[2] = dynamic.development_mille as f32 / 1000.0 / 35.0;
		row[3] = dynamic.fort_level as f32 / 3.0;
		let controller = dynamic.controller_index?;
		row[4] = (controller == input.our_index).into();
		row[5] = input
			.enemy_index
			.is_some_and(|enemy| controller == enemy)
			.into();
		row[6] = (row[4] == 0.0 && row[5] == 0.0).into();
		row[7] = (record.owner == input.our_tag).into();
		// Terrain one-hot, columns 22..26: the simulator's five buckets
		// only — an unknown name refuses the frame, never a guess.
		let name = dynamic.terrain_name?;
		let slot = TERRAIN_SLOTS
			.iter()
			.position(|&known| known == name)
			.or_else(|| {
				TERRAIN_ALIASES
					.iter()
					.find(|&&(from, _)| from == name)
					.map(|&(_, to)| to)
			})?;
		row[22 + slot] = 1.0;
		if slot == our_index {
			row[8] = our.infantry / 14.0;
			row[9] = our.cavalry / 14.0;
			row[10] = our.artillery / 14.0;
			row[11] = our.morale;
		}
		row[20] = (input.wargoal == Some(id)).into();
		row[21] = if input.our_capital == Some(id) {
			1.0
		} else if input.their_capital == Some(id) {
			-1.0
		} else {
			0.0
		};
		row[28] = (input.our_last == Some(id) && id != our.province).into();
	}

	Some(State {
		nodes,
		edges: graph.index_edges.clone(),
		rules: rules_row(input, our.morale),
		// Candidate NODE INDICES (the plane contract), in neighbor
		// order — ids recover through the graph.
		candidates: graph
			.provinces
			.get(&our.province)?
			.neighbors
			.iter()
			.filter(|&&n| n != our.province && graph.provinces.contains_key(&n))
			.filter_map(|n| graph.index_of(*n).map(|i| i as u32))
			.collect(),
	})
}

/// The war-level rules row, in the simulator's exact 26-slot order:
/// define ratios (0-2, 12-14, 16-17) are static and stay at the
/// simulator's defaults here; battle/occupation/ticking parts
/// (20-22), supply (10), siege fraction (11), ally regiments (15)
/// and blood ledger (24-25) are pending their engine reads and keep
/// the neutral encoding. Live today: day (3), held fractions (4-5),
/// our regiments and morale (6, 8), warscore (9), declaration and
/// wargoal flags (18-19), manpower fraction (23).
fn rules_row(input: &FrameInput, our_morale: f32) -> [f32; RULES] {
	let war = &input.war;
	let mut row = [0.0f32; RULES];
	// Define ratios the simulator trained on (battles cap 40,
	// wargoal bonus 5, ticking 0.4, recoveries, width 15, flank 2).
	row[0] = 1.0;
	row[1] = 1.0;
	row[2] = 1.0;
	row[12] = 1.0;
	row[13] = 1.0;
	row[14] = 1.0;
	row[16] = 1.0;
	row[17] = 1.0;
	row[3] = war.day.map(|d| d as f32 / 1825.0).unwrap_or(0.0);
	row[4] = war.our_held_fraction.unwrap_or(0.0);
	row[5] = war.their_held_fraction.unwrap_or(0.0);
	if let Some(stack) = &input.our {
		row[6] = (stack.infantry + stack.cavalry + stack.artillery) / 14.0;
	}
	row[8] = our_morale;
	row[9] = war.war_score.map(|s| s as f32 / 50.0).unwrap_or(0.0);
	row[18] = f32::from(war.we_declared);
	row[19] = f32::from(war.has_wargoal);
	row[23] = war.manpower_fraction.unwrap_or(0.0);
	row
}

#[cfg(test)]
mod tests {
	use super::*;

	fn graph() -> StaticGraph {
		StaticGraph::from_json(
			r#"{"provinces": {
			"1": {"x": 10.0, "y": 20.0, "development": 35, "owner": "SWE", "neighbors": [2, 3]},
			"2": {"x": 30.0, "y": 40.0, "development": 7, "owner": "DAN", "neighbors": [1, 3]},
			"3": {"x": 50.0, "y": 60.0, "development": 14, "owner": "NOR", "neighbors": [1, 2]}
		}}"#,
		)
		.expect("parse")
	}

	fn input() -> FrameInput {
		FrameInput {
			our_index: 3,
			our_tag: "SWE".to_string(),
			enemy_index: Some(6),
			our: Some(ArmySnapshot {
				province: 1,
				infantry: 14.0,
				cavalry: 2.0,
				artillery: 2.0,
				morale: 0.8,
			}),
			..FrameInput::default()
		}
	}

	#[test]
	fn assembles_the_plane_with_our_stand_column() {
		let graph = graph();
		let dynamics = |_| {
			Some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
				terrain_name: Some("hills"),
			})
		};
		let state = assemble(&graph, &dynamics, &input()).expect("frame");
		assert_eq!(state.nodes.len(), 3);
		assert_eq!(state.nodes_flat().len(), 3 * NODE);
		assert_eq!(state.here(), 0);
		// Candidates are node indices of our position's neighbors.
		assert_eq!(state.candidates, vec![1, 2]);
		let row = &state.nodes[0];
		assert_eq!(row[7], 1.0, "static owner SWE == our tag");
		assert_eq!(row[2], 14000.0 / 1000.0 / 35.0);
		assert_eq!(row[3], 2.0 / 3.0);
		assert_eq!(row[4], 1.0);
		assert_eq!(row[5], 0.0);
	}

	#[test]
	fn rules_row_fills_the_live_war_slots_in_simulator_order() {
		let graph = graph();
		let dynamics = |_| {
			Some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
				terrain_name: Some("hills"),
			})
		};
		let mut input = input();
		input.war = WarFacts {
			day: Some(365),
			our_held_fraction: Some(0.4),
			their_held_fraction: Some(0.6),
			war_score: Some(12),
			manpower_fraction: Some(0.5),
			we_declared: true,
			has_wargoal: true,
		};
		let state = assemble(&graph, &dynamics, &input).expect("frame");
		let rules = &state.rules;
		assert_eq!(rules[3], 365.0 / 1825.0);
		assert_eq!(rules[4], 0.4);
		assert_eq!(rules[5], 0.6);
		assert_eq!(rules[9], 12.0 / 50.0);
		assert_eq!(rules[18], 1.0);
		assert_eq!(rules[19], 1.0);
		assert_eq!(rules[23], 0.5);
		// Our stack feeds regiments and morale.
		assert_eq!(rules[6], 18.0 / 14.0);
		assert_eq!(rules[8], 0.8);
	}

	#[test]
	fn missing_dynamics_refuse_the_whole_frame() {
		let graph = graph();
		let dynamics = |id: u32| {
			(id != 2).then_some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
				terrain_name: Some("hills"),
			})
		};
		assert!(assemble(&graph, &dynamics, &input()).is_none());
	}

	#[test]
	fn terrain_outside_the_simulator_buckets_refuses_the_frame() {
		let graph = graph();
		let hills = |_| {
			Some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
				terrain_name: Some("hills"),
			})
		};
		let lava = |_| {
			Some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
				terrain_name: Some("lava"),
			})
		};
		let input = input();
		let state = assemble(&graph, &hills, &input).expect("hills frame");
		assert_eq!(&state.nodes[0][22..27], &[0.0, 0.0, 1.0, 0.0, 0.0]);
		assert!(
			assemble(&graph, &lava, &input).is_none(),
			"unmapped terrain refuses"
		);
	}
}
