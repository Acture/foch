//! Role-based trade-topology policy, pure and engine-free.
//!
//! Each trait plays its own role over a node's *neighborhood* (the node
//! plus its direct vanilla trading partners, in both directions) — no
//! trait is ever blended into another:
//!
//! - **tax base** (purchasing power) decides the *sink*: the node with
//!   the neighborhood's highest tax keeps everything and closes all its
//!   outgoing edges — an emergent black hole;
//! - **trade power** decides the *transit*: a hub keeps flowing (its
//!   edges stay open; producers' edges into it are vanilla and stay
//!   open too);
//! - **production** decides the *upstream*: a producer keeps exporting
//!   (edges open) — the engine already turns its output into local
//!   trade value.
//!
//! Tax is the only closing signal. Until the per-node tax aggregate is
//! read from the provinces it is zero everywhere, no node is a tax peak,
//! and the policy closes nothing — an honest idle, never a guess from
//! the other traits.
//!
//! A peak holds its closures until its tax falls below
//! `hysteresis_pct` of its neighborhood's best (with the close itself
//! requiring `converge_pct` of it), so month-to-month noise cannot flap
//! a black hole. Reopening restores the vanilla edge, so the live graph
//! is always a subgraph of the vanilla DAG.

pub const DEFAULT_CONVERGE_PCT: i64 = 100;
pub const DEFAULT_HYSTERESIS_PCT: i64 = 95;
pub const DEFAULT_MAX_FLIPS_TOTAL: usize = 16;

/// One node as observed from the engine, identified by its engine index.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSnapshot {
	pub id: u32,
	/// Aggregated tax base of the node's provinces (purchasing power).
	pub tax: f64,
	/// Aggregated production of the node's provinces.
	pub production: f64,
	/// The node's total trade power as the trade map shows it.
	pub trade_power: f64,
	/// The node's current total trade value.
	pub current_value: f64,
	/// Vanilla outgoing edges in engine order; each entry is the target
	/// node id. The plugin never adds edges, so this is constant per game.
	pub outgoing: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct Params {
	/// A node closes all its edges when its tax reaches this percent of
	/// its neighborhood's best other tax (100 = strictly the highest).
	pub converge_pct: i64,
	/// A closed node reopens only when its tax falls below this percent
	/// of the neighborhood's best.
	pub hysteresis_pct: i64,
	/// Edge flips the whole graph may apply per month; a peak's full
	/// closure counts as its edge count.
	pub max_flips_total: usize,
}

impl Default for Params {
	fn default() -> Self {
		Self {
			converge_pct: DEFAULT_CONVERGE_PCT,
			hysteresis_pct: DEFAULT_HYSTERESIS_PCT,
			max_flips_total: DEFAULT_MAX_FLIPS_TOTAL,
		}
	}
}

impl Params {
	/// The judgment subset of a [`crate::config::Config`].
	pub fn from(config: crate::config::Config) -> Self {
		Self {
			converge_pct: config.converge_pct,
			hysteresis_pct: config.hysteresis_pct,
			max_flips_total: config.max_flips_total,
		}
	}

	pub fn validate(&self) -> Result<(), &'static str> {
		if self.converge_pct < 90 || self.converge_pct > 200 {
			return Err("converge_pct must be within 90..=200");
		}
		if self.hysteresis_pct < 50 || self.hysteresis_pct > self.converge_pct {
			return Err("hysteresis_pct must be within 50..=converge_pct");
		}
		if self.max_flips_total == 0 {
			return Err("flip budget must be positive");
		}
		Ok(())
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipAction {
	/// Remove the edge from the live route table.
	Close,
	/// Restore the vanilla edge.
	Reopen,
}

/// One planned edge flip, with the evidence that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Flip {
	pub node: u32,
	/// Index into the node's vanilla `outgoing` list.
	pub edge: usize,
	pub action: FlipAction,
	/// The node's own tax and its neighborhood's best other tax.
	pub node_tax: f64,
	pub neighbor_tax: f64,
}

impl Flip {
	/// Human-readable decision line for the monthly log.
	pub fn describe(&self, nodes: &[NodeSnapshot]) -> String {
		let node = &nodes[self.node as usize];
		let target = node.outgoing[self.edge];
		let verb = match self.action {
			FlipAction::Close => "close",
			FlipAction::Reopen => "reopen",
		};
		format!(
			"{} edge {} -> {} (tax {:.1} vs neighborhood {:.1})",
			verb, self.node, target, self.node_tax, self.neighbor_tax
		)
	}
}

/// Direct vanilla neighbors in both directions, excluding the node.
fn neighborhood(node: &NodeSnapshot, nodes: &[NodeSnapshot]) -> Vec<u32> {
	let mut around: Vec<u32> = node.outgoing.clone();
	for other in nodes {
		if other.id != node.id && other.outgoing.contains(&node.id) {
			around.push(other.id);
		}
	}
	around
}

/// Plan this month's flips against one shared pre-tick snapshot.
///
/// A node whose tax dominates its neighborhood closes every outgoing
/// edge at once (it is the purchasing-power center, so trade terminates
/// there); it reopens them all only inside the hysteresis band. Nodes
/// iterate in id order, so planning is deterministic.
pub fn plan(
	nodes: &[NodeSnapshot],
	closed: &mut [Vec<bool>],
	params: &Params,
) -> Result<Vec<Flip>, &'static str> {
	if nodes.len() != closed.len() {
		return Err("closed mirror must match the node count");
	}
	params.validate()?;
	for (node, flags) in nodes.iter().zip(closed.iter()) {
		if flags.len() != node.outgoing.len() {
			return Err("closed mirror must match each node's edge count");
		}
	}
	let mut flips = Vec::new();
	let mut total = 0usize;
	for node in nodes {
		if node.outgoing.is_empty() {
			continue;
		}
		if total >= params.max_flips_total {
			break;
		}
		let neighbors = neighborhood(node, nodes);
		let best_neighbor = neighbors
			.iter()
			.filter_map(|id| nodes.get(*id as usize))
			.map(|other| other.tax)
			.fold(0.0_f64, f64::max);
		// A terminal needs a positive tax signal; zero tax against a zero
		// neighborhood (the province walk not landed yet) is no peak.
		let is_terminal =
			node.tax > 0.0 && node.tax >= best_neighbor * params.converge_pct as f64 / 100.0;
		let reopen = node.tax < best_neighbor * params.hysteresis_pct as f64 / 100.0;
		let want_closed = if reopen { false } else { is_terminal };
		let all_closed = closed[node.id as usize].iter().all(|flag| *flag);
		if want_closed == all_closed {
			continue; // already in the desired shape
		}
		for (edge, flag) in closed[node.id as usize].iter_mut().enumerate() {
			if *flag == want_closed {
				continue; // per-node shape is all-or-nothing
			}
			if total >= params.max_flips_total {
				break;
			}
			*flag = want_closed;
			flips.push(Flip {
				node: node.id,
				edge,
				action: if want_closed {
					FlipAction::Close
				} else {
					FlipAction::Reopen
				},
				node_tax: node.tax,
				neighbor_tax: best_neighbor,
			});
			total += 1;
		}
	}
	Ok(flips)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn node(id: u32, tax: f64, outgoing: Vec<u32>) -> NodeSnapshot {
		NodeSnapshot {
			id,
			tax,
			production: 0.0,
			trade_power: 0.0,
			current_value: 0.0,
			outgoing,
		}
	}

	fn mirror(nodes: &[NodeSnapshot]) -> Vec<Vec<bool>> {
		nodes
			.iter()
			.map(|n| vec![false; n.outgoing.len()])
			.collect()
	}

	#[test]
	fn the_neighborhood_tax_peak_terminates() {
		// Beijing (50) is the tax peak among itself, Girin and Xi'an;
		// every other trait is ignored.
		let nodes = vec![
			node(0, 30.0, vec![1]),
			node(1, 10.0, vec![2]),
			node(2, 50.0, vec![0]),
		];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].node, 2);
		assert_eq!(flips[0].action, FlipAction::Close);
		assert!(closed[2][0]);
		// The others keep flowing toward the peak.
		assert!(!closed[0][0]);
		assert!(!closed[1][0]);
	}

	#[test]
	fn production_and_power_never_terminate_a_node() {
		// Girin produces and trades like a monster but taxes like a mouse.
		let mut girin = node(0, 1.0, vec![1]);
		girin.production = 100.0;
		girin.trade_power = 500.0;
		let beijing = node(1, 10.0, vec![]);
		let nodes = vec![girin, beijing];
		let mut closed = mirror(&nodes);
		assert!(
			plan(&nodes, &mut closed, &Params::default())
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn zero_tax_closes_nothing() {
		// Until the province walk lands, tax is zero everywhere: the
		// policy stays honestly idle, even with huge production.
		let mut a = node(0, 0.0, vec![1]);
		a.production = 100.0;
		let mut b = node(1, 0.0, vec![]);
		b.production = 1.0;
		let nodes = vec![a, b];
		let mut closed = mirror(&nodes);
		assert!(
			plan(&nodes, &mut closed, &Params::default())
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn a_tie_is_not_a_peak() {
		// Two equally-taxed neighbors: neither strictly dominates, so
		// neither terminates (the default 100% requires reaching the
		// other's best, and an exact tie at 100 does close — pin the
		// intent with a strict-margin run below instead).
		let nodes = vec![node(0, 10.0, vec![1]), node(1, 10.0, vec![])];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1, ">= at 100 percent keeps a tied peak");
	}

	#[test]
	fn the_hysteresis_band_holds_a_peak() {
		// 10 vs 9: close needs >= 9 (100%), reopen needs < 8.55 (95%).
		let nodes = vec![node(0, 10.0, vec![1]), node(1, 9.0, vec![])];
		let mut closed = mirror(&nodes);
		plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert!(closed[0][0]);
		// Still 10 vs 9 next month: no reopen, no flap.
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert!(flips.is_empty());
		// The neighbor overtakes clearly: reopen.
		let nodes = vec![node(0, 10.0, vec![1]), node(1, 12.0, vec![])];
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].action, FlipAction::Reopen);
	}

	#[test]
	fn only_direct_neighbors_judge() {
		// A distant rich node does not speak for the direct neighbor.
		let nodes = vec![
			node(0, 30.0, vec![1]),
			node(1, 25.0, vec![2]),
			node(2, 500.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		// Node 0 is the peak of {0,1}: it closes; node 2 is not node 0's
		// neighbor and cannot keep it open.
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].node, 0);
	}

	#[test]
	fn mirrors_must_match_the_graph() {
		let nodes = vec![node(0, 1.0, vec![1]), node(1, 1.0, vec![])];
		let mut too_many_flags = vec![vec![false, false], vec![]];
		assert!(plan(&nodes, &mut too_many_flags, &Params::default()).is_err());
		let mut too_few_nodes = vec![vec![false]];
		assert!(plan(&nodes, &mut too_few_nodes, &Params::default()).is_err());
	}

	#[test]
	fn invalid_params_are_rejected() {
		let nodes = vec![node(0, 1.0, vec![])];
		let mut closed = mirror(&nodes);
		let crossed = Params {
			hysteresis_pct: 130,
			..Params::default()
		};
		assert!(plan(&nodes, &mut closed, &crossed).is_err());
	}
}
