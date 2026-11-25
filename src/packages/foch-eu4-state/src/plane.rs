//! The 29-column province feature representation.
//!
//! Rows retain the simulator and decision-scoring tools' column order.
//! This crate provides the representation and candidate readers; it does
//! not yet assemble complete live game-state rows.

/// Observables per province row.
pub const NODE: usize = 29;

/// War-level rule features, one row per decision.
pub const RULES: usize = 26;

/// Column semantics, in board order. `live` names the game-memory
/// source the reader fills the column from (candidate offsets in [`crate::candidates`], verified ones through `foch_runtime::dataplane` views).
pub const COLUMNS: [&str; NODE] = [
	"x",                   // 0: map position ÷5000
	"y",                   // 1: map position ÷5000
	"development", // 2: (tax+prod+manpower) ÷1000 ÷35 — tax/manpower verified, +0x3e8 candidate
	"fort_level",  // 3: ÷3 — live: candidate prov+0x2de0
	"controller_ours", // 4: live: candidate prov+0x470 (candidates::province::CONTROLLER)
	"controller_theirs", // 5
	"controller_neutral", // 6
	"original_owner_ours", // 7: peace-deal owner, not current occupier
	"our_infantry", // 8: ÷14, only where our stack stands — the GNN's "here" argmax column
	"our_cavalry", // 9: ÷14
	"our_artillery", // 10: ÷14
	"our_morale",  // 11
	"allied_infantry", // 12: ÷14, summed allied stacks in the province
	"allied_cavalry", // 13: ÷14
	"allied_artillery", // 14: ÷14
	"allied_morale", // 15: average over allied stacks
	"enemy_infantry", // 16: ÷14, vision-gated in the sim
	"enemy_cavalry", // 17: ÷14
	"enemy_artillery", // 18: ÷14
	"siege_progress", // 19: ÷SIEGE_WIN(20)
	"is_wargoal",  // 20
	"capital",     // 21: +1 ours, −1 theirs
	"terrain_plains", // 22..26: one-hot
	"terrain_forest", // 23
	"terrain_hills", // 24
	"terrain_mountains", // 25
	"terrain_marsh", // 26
	"supply",      // 27: ÷35
	"our_last",    // 28: 1 where we stood before this march
];

/// One decision's world, ready for the GNN.
#[derive(Clone, Debug, Default)]
pub struct State {
	/// `[province][NODE]`, row-major.
	pub nodes: Vec<[f32; NODE]>,
	/// Undirected adjacency as (src, dst) pairs — both directions listed.
	pub edges: Vec<(u32, u32)>,
	/// The war-level rules row.
	pub rules: [f32; RULES],
	/// Province indices the player's stack may move to.
	pub candidates: Vec<u32>,
}

impl State {
	/// The node-feature matrix flattened row-major, the layout
	/// `GeneralGnn::scores` takes.
	pub fn nodes_flat(&self) -> Vec<f32> {
		self.nodes.concat()
	}

	/// Our stack's province: column 8's argmax, matching the Python
	/// forward exactly (first index on ties).
	pub fn here(&self) -> usize {
		let mut here = 0;
		let mut top = f32::NEG_INFINITY;
		for (i, row) in self.nodes.iter().enumerate() {
			if row[8] > top {
				top = row[8];
				here = i;
			}
		}
		here
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn columns_and_widths_match_the_sim_contract() {
		assert_eq!(COLUMNS.len(), NODE);
		assert_eq!(NODE, foch_eu4_gnn::NODE);
		assert_eq!(RULES, foch_eu4_gnn::RULES);
	}

	#[test]
	fn here_finds_the_stand_column_argmax() {
		let mut state = State {
			nodes: vec![[0.0; NODE]; 4],
			..State::default()
		};
		state.nodes[2][8] = 0.7;
		state.nodes[3][8] = 0.7; // tie: first index wins, like torch argmax
		assert_eq!(state.here(), 2);
		assert_eq!(state.nodes_flat().len(), 4 * NODE);
	}
}
