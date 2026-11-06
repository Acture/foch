//! Wealth-gradient trade-topology policy, pure and engine-free.
//!
//! The policy mirrors the vanilla trade graph (nodes with fixed outgoing
//! edges) plus a per-edge `closed` flag, and every month plans a bounded set
//! of edge flips:
//!
//! - a node's **wealth** is a weighted sum of its tax base (purchasing
//!   power), production and trade power (weights in percent);
//! - each outgoing edge is judged against the **basin wealth** reachable
//!   through it on currently open edges, discounted by distance;
//! - an open edge closes when the node is at least `converge_pct` richer
//!   than its basin; a closed edge reopens only below `hysteresis_pct`,
//!   so a band between the two thresholds cannot flap;
//! - a node whose edges all close becomes an end node: an emergent black
//!   hole. Reopening restores the vanilla edge, so the graph is always a
//!   subgraph of the vanilla DAG and stays acyclic.
//!
//! Closing at most `max_flips_per_node` edges per node and month (and
//! `max_flips_total` overall) keeps monthly deltas observable and the
//! trade map stable.

pub const DEFAULT_CONVERGE_PCT: i64 = 120;
pub const DEFAULT_HYSTERESIS_PCT: i64 = 80;
pub const DEFAULT_DECAY_PCT: i64 = 85;
pub const DEFAULT_WEALTH_TAX_PCT: i64 = 100;
pub const DEFAULT_WEALTH_PROD_PCT: i64 = 50;
pub const DEFAULT_WEALTH_TP_PCT: i64 = 25;
pub const DEFAULT_MAX_FLIPS_PER_NODE: usize = 1;
pub const DEFAULT_MAX_FLIPS_TOTAL: usize = 8;

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
	/// Close an open edge when node wealth ≥ basin wealth × converge_pct/100.
	pub converge_pct: i64,
	/// Reopen a closed edge only when node wealth < basin wealth × hysteresis_pct/100.
	pub hysteresis_pct: i64,
	/// Per-hop basin discount: basin wealth × (decay_pct/100)^distance.
	pub decay_pct: i64,
	/// Node-wealth weight of the tax aggregate, in percent.
	pub wealth_tax_pct: i64,
	/// Node-wealth weight of the production aggregate, in percent.
	pub wealth_prod_pct: i64,
	/// Node-wealth weight of total trade power, in percent.
	pub wealth_tp_pct: i64,
	pub max_flips_per_node: usize,
	pub max_flips_total: usize,
}

impl Default for Params {
	fn default() -> Self {
		Self {
			converge_pct: DEFAULT_CONVERGE_PCT,
			hysteresis_pct: DEFAULT_HYSTERESIS_PCT,
			decay_pct: DEFAULT_DECAY_PCT,
			wealth_tax_pct: DEFAULT_WEALTH_TAX_PCT,
			wealth_prod_pct: DEFAULT_WEALTH_PROD_PCT,
			wealth_tp_pct: DEFAULT_WEALTH_TP_PCT,
			max_flips_per_node: DEFAULT_MAX_FLIPS_PER_NODE,
			max_flips_total: DEFAULT_MAX_FLIPS_TOTAL,
		}
	}
}

impl Params {
	/// The judgment subset of a [`crate::config::Config`], for callers that
	/// keep configuration and policy parameters in one place.
	pub fn from(config: crate::config::Config) -> Self {
		Self {
			converge_pct: config.converge_pct,
			hysteresis_pct: config.hysteresis_pct,
			decay_pct: config.decay_pct,
			wealth_tax_pct: config.wealth_tax_pct,
			wealth_prod_pct: config.wealth_prod_pct,
			wealth_tp_pct: config.wealth_tp_pct,
			max_flips_per_node: config.max_flips_per_node,
			max_flips_total: config.max_flips_total,
		}
	}

	pub fn validate(&self) -> Result<(), &'static str> {
		if self.converge_pct <= 0 || self.hysteresis_pct <= 0 {
			return Err("converge_pct and hysteresis_pct must be positive");
		}
		if self.hysteresis_pct > self.converge_pct {
			return Err("hysteresis_pct must not exceed converge_pct");
		}
		if !(0..=100).contains(&self.decay_pct) || self.decay_pct == 0 {
			return Err("decay_pct must be within 1..=100");
		}
		if self.max_flips_per_node == 0 || self.max_flips_total == 0 {
			return Err("flip budgets must be positive");
		}
		Ok(())
	}

	fn wealth(&self, node: &NodeSnapshot) -> f64 {
		let tax = node.tax * self.wealth_tax_pct as f64 / 100.0;
		let prod = node.production * self.wealth_prod_pct as f64 / 100.0;
		let power = node.trade_power * self.wealth_tp_pct as f64 / 100.0;
		tax + prod + power
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
	pub node_wealth: f64,
	pub basin_wealth: f64,
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
			"{} edge {} -> {} (wealth {:.1} vs basin {:.1})",
			verb, self.node, target, self.node_wealth, self.basin_wealth
		)
	}
}

/// Basin wealth reachable from `start` over open edges only: the maximum
/// discounted wealth of any reachable node, with the first hop at full
/// weight and each further hop discounted once more, so the same wealth
/// weighs less the further downstream it sits. A through node counts
/// because a wealthy through node becomes terminal under this same policy
/// later on.
fn basin_wealth(start: u32, nodes: &[NodeSnapshot], closed: &[Vec<bool>], params: &Params) -> f64 {
	let decay = params.decay_pct as f64 / 100.0;
	// (node id, hops-from-start) queue with the best discount seen per
	// node; the graph is a DAG, but the guard also tolerates a stale view.
	let mut best = vec![0.0f64; nodes.len()];
	let mut queue = std::collections::VecDeque::new();
	let mut reachable_max = 0.0f64;
	if (start as usize) < nodes.len() {
		// The edge target itself is the basin's first hop at full weight.
		reachable_max = params.wealth(&nodes[start as usize]);
		best[start as usize] = 1.0;
		queue.push_back((start, 0usize));
	}
	while let Some((id, hops)) = queue.pop_front() {
		let node = &nodes[id as usize];
		for (edge, target) in node.outgoing.iter().enumerate() {
			if closed[id as usize].get(edge).copied().unwrap_or(false) {
				continue;
			}
			let target = *target as usize;
			if target >= nodes.len() {
				continue;
			}
			let discount = decay.powi((hops + 1) as i32);
			let wealth = params.wealth(&nodes[target]) * discount;
			if wealth > reachable_max {
				reachable_max = wealth;
			}
			if best[target] < discount {
				best[target] = discount;
				queue.push_back((target as u32, hops + 1));
			}
		}
	}
	reachable_max
}

/// Plan this month's flips against a shared pre-tick snapshot.
///
/// `closed` mirrors every node's vanilla edges (`closed[node][edge]`); the
/// planned flips are applied to it so callers see the post-plan state.
/// Judgments are deterministic: nodes in id order, edges in engine order.
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
	for id in 0..nodes.len() as u32 {
		if total >= params.max_flips_total {
			break;
		}
		let node = &nodes[id as usize];
		if node.outgoing.is_empty() {
			continue;
		}
		let wealth = params.wealth(node);
		let mut candidates: Vec<(usize, FlipAction, f64)> = node
			.outgoing
			.iter()
			.enumerate()
			.map(|(edge, target)| (edge, basin_wealth(*target, nodes, closed, params)))
			.filter_map(|(edge, basin)| {
				let close = wealth >= basin * params.converge_pct as f64 / 100.0;
				let reopen = wealth < basin * params.hysteresis_pct as f64 / 100.0;
				let action = match closed[id as usize][edge] {
					false => close.then_some(FlipAction::Close),
					true => (reopen && !close).then_some(FlipAction::Reopen),
				};
				action.map(|action| (edge, action, basin))
			})
			.collect();
		// Closures drain the poorest basins first and reopens restore the
		// richest first, so a bounded month always moves the worst edge.
		candidates.sort_by(|a, b| match (a.1, b.1) {
			(FlipAction::Close, FlipAction::Close) => a.2.total_cmp(&b.2),
			(FlipAction::Reopen, FlipAction::Reopen) => b.2.total_cmp(&a.2),
			(FlipAction::Close, _) => std::cmp::Ordering::Less,
			_ => std::cmp::Ordering::Greater,
		});
		for (edge, action, basin) in candidates.into_iter().take(params.max_flips_per_node) {
			if total >= params.max_flips_total {
				break;
			}
			closed[id as usize][edge] = matches!(action, FlipAction::Close);
			flips.push(Flip {
				node: id,
				edge,
				action,
				node_wealth: wealth,
				basin_wealth: basin,
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
	fn a_wealthy_peak_becomes_a_black_hole() {
		// A(100) -> B(10) -> C(10, end node). A dwarfs its basin.
		let nodes = vec![
			node(0, 100.0, vec![1]),
			node(1, 10.0, vec![2]),
			node(2, 10.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].node, 0);
		assert_eq!(flips[0].action, FlipAction::Close);
		assert!(closed[0][0]);
		// Next month nothing more flips: A is already terminal.
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert!(flips.is_empty());
	}

	#[test]
	fn a_poor_node_keeps_its_outflow() {
		// A(5) -> B(100, end node). The basin dwarfs A.
		let nodes = vec![node(0, 5.0, vec![1]), node(1, 100.0, vec![])];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert!(flips.is_empty());
		assert!(!closed[0][0]);
	}

	#[test]
	fn the_hysteresis_band_cannot_flap() {
		// Wealth 50 against basin 45: close needs >= 54, reopen needs < 36.
		let nodes = vec![node(0, 50.0, vec![1]), node(1, 45.0, vec![])];
		let mut closed = mirror(&nodes);
		assert!(
			plan(&nodes, &mut closed, &Params::default())
				.unwrap()
				.is_empty()
		);
		// From the closed side the same numbers stay closed.
		closed[0][0] = true;
		assert!(
			plan(&nodes, &mut closed, &Params::default())
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn a_closed_edge_reopens_when_the_basin_gets_rich() {
		let nodes = vec![node(0, 50.0, vec![1]), node(1, 100.0, vec![])];
		let mut closed = mirror(&nodes);
		closed[0][0] = true;
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].action, FlipAction::Reopen);
		assert!(!closed[0][0]);
	}

	#[test]
	fn the_per_node_budget_spreads_closures_over_months() {
		// A(100) with three poor branches; only one closes per month.
		let nodes = vec![
			node(0, 100.0, vec![1, 2, 3]),
			node(1, 10.0, vec![]),
			node(2, 10.0, vec![]),
			node(3, 10.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let one = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(one.len(), 1);
		let two = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(two.len(), 1);
		let three = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(three.len(), 1);
		assert!(
			plan(&nodes, &mut closed, &Params::default())
				.unwrap()
				.is_empty()
		);
		assert!(closed[0].iter().all(|c| *c));
	}

	#[test]
	fn the_poorest_basin_closes_first() {
		// Two closeable branches: the poorer far one closes first and the
		// richer near one survives the month.
		let nodes = vec![
			node(0, 50.0, vec![1, 2]),
			node(1, 45.0, vec![]),
			node(2, 30.0, vec![3]),
			node(3, 30.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].edge, 1, "the poorer far branch closes first");
		assert!(!closed[0][0], "the richer near branch survives");
		// The far basin discounts beyond its first hop: max(30, 30*0.85)=30.
		assert_eq!(flips[0].basin_wealth, 30.0);
	}

	#[test]
	fn the_first_hop_counts_at_full_weight() {
		// 50 against 45: no close at 120%, no reopen at 80% — the neighbor
		// is weighed undiscounted or this band would flip.
		let nodes = vec![node(0, 50.0, vec![1]), node(1, 45.0, vec![])];
		let mut closed = mirror(&nodes);
		let flips = plan(&nodes, &mut closed, &Params::default()).unwrap();
		assert_eq!(flips.len(), 0);
		assert_eq!(basin_wealth(1, &nodes, &closed, &Params::default()), 45.0);
	}

	#[test]
	fn trade_power_raises_a_downstream_hubs_attractiveness() {
		// Without the weight A closes its edge; a heavily powered hub keeps it.
		let mut nodes = vec![node(0, 100.0, vec![1]), node(1, 60.0, vec![])];
		let mut closed = mirror(&nodes);
		let plain = Params::default();
		assert_eq!(
			plan(&nodes, &mut closed, &plain).unwrap()[0].action,
			FlipAction::Close
		);
		nodes[1].trade_power = 500.0;
		let mut closed = mirror(&nodes);
		let hub = Params {
			wealth_tp_pct: 100,
			..Params::default()
		};
		assert!(plan(&nodes, &mut closed, &hub).unwrap().is_empty());
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
