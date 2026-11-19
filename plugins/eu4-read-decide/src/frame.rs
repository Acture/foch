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
		rules: rules_row(input),
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

/// The war-level rules row: v1 carries the structural facts the net
/// can consume today; the rest of the 26 slots stay zero with the
/// same encoding the simulator uses for "not in a state".
fn rules_row(_input: &FrameInput) -> [f32; RULES] {
	[0.0; RULES]
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
	fn missing_dynamics_refuse_the_whole_frame() {
		let graph = graph();
		let dynamics = |id: u32| {
			(id != 2).then_some(DynamicProvince {
				development_mille: 14_000,
				fort_level: 2,
				controller_index: Some(3),
			})
		};
		assert!(assemble(&graph, &dynamics, &input()).is_none());
	}
}
