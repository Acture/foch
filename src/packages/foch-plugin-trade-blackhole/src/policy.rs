//! Propagation-network trade policy, pure and engine-free.
//!
//! The model treats the vanilla edges as **bidirectional**: value may flow
//! either way between two trading partners, and the topology is *discovered*
//! by convergence rather than declared:
//!
//! - every month each node **injects** its production;
//! - a node **absorbs** a share of whatever pools in it, rising with its
//!   tax intensity (purchasing power): `a = t / (t + λ)`, so λ is the tax
//!   level at which a node keeps half of what arrives;
//! - the rest **relays** to open partners, split by attractiveness
//!   `1 + power/β` — trade power is the transit role, favoring hubs
//!   without ever blending into another trait's comparison;
//! - after the relaxation, each vanilla edge is judged by its **net flow**
//!   (forward minus the relayed reverse): edges that carry ~nothing, or
//!   whose net runs against the vanilla direction, close a few per month
//!   ("一点点 close 掉"); an engine-loadable edge never appears where the
//!   vanilla graph had none, so the engine still sees a subgraph of its
//!   own DAG;
//! - black holes are the outcome, not the action: nodes whose absorption
//!   drains their region.
//!
//! Hysteresis: an edge reopens only when its net flow clearly recovers
//! (`reopen_pct` of the node's pool), so the map cannot flap month to
//! month. All magnitudes arrive as engine thousandths and are only ever
//! compared to each other, never to absolute ducats.

pub const DEFAULT_ABSORB_REF: i64 = 3000;
pub const DEFAULT_RELAY_REF: i64 = 3000;
pub const DEFAULT_FLOW_EPSILON_PCT: i64 = 5;
pub const DEFAULT_REOPEN_PCT: i64 = 15;
/// Consecutive months of evidence before an edge flips. Trade is a
/// long-term behavior: one month of low flow is noise, half a year
/// of sustained low flow is a trend worth acting on.
pub const DEFAULT_EVIDENCE_MONTHS: u32 = 6;
pub const DEFAULT_MAX_FLIPS_TOTAL: usize = 16;
const RELAXATION_ROUNDS: usize = 8;

/// One node as observed from the engine, identified by its engine index.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSnapshot {
	pub id: u32,
	/// Per-province mean tax in thousandths: the sink intensity.
	pub tax: f64,
	/// Aggregated production of the node's provinces: the injection.
	pub production: f64,
	/// The node's total trade power as the trade map shows it: the relay
	/// attractiveness.
	pub trade_power: f64,
	/// The node's current total trade value.
	pub current_value: f64,
	/// Vanilla outgoing edges in engine order; each entry is the target
	/// node id. The plugin never adds edges, so this is constant per game.
	pub outgoing: Vec<u32>,
	/// Per-edge steering weights (thousandths), index-aligned with
	/// `outgoing`: the engine's own record of how much trade power is
	/// actively steering toward each target. This is the "effort" —
	/// merchant placements, trade-company investments, light ships —
	/// and it is directional: the reverse direction carries zero.
	pub steering: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct Params {
	/// Tax intensity (thousandths) at which a node absorbs half of what
	/// pools in it; higher means only richer nodes sink.
	pub absorb_ref: i64,
	/// Trade power (thousandths) at which a partner doubles its relay
	/// attractiveness; higher flattens hub preference.
	pub relay_ref: i64,
	/// An edge whose net flow is below this percent of its source's pool
	/// counts as carrying nothing.
	pub flow_epsilon_pct: i64,
	/// A closed edge reopens only above this percent of the pool.
	pub reopen_pct: i64,
	/// Consecutive months of sustained evidence before any edge flips.
	/// Trade is long-term: a single month of low flow is noise.
	pub evidence_months: u32,
	/// Edge flips the whole graph may apply per month.
	pub max_flips_total: usize,
}

impl Default for Params {
	fn default() -> Self {
		Self {
			absorb_ref: DEFAULT_ABSORB_REF,
			relay_ref: DEFAULT_RELAY_REF,
			flow_epsilon_pct: DEFAULT_FLOW_EPSILON_PCT,
			reopen_pct: DEFAULT_REOPEN_PCT,
			max_flips_total: DEFAULT_MAX_FLIPS_TOTAL,
			evidence_months: DEFAULT_EVIDENCE_MONTHS,
		}
	}
}

impl Params {
	/// The judgment subset of a [`crate::config::Config`].
	pub fn from(config: crate::config::Config) -> Self {
		Self {
			absorb_ref: config.absorb_ref,
			relay_ref: config.relay_ref,
			flow_epsilon_pct: config.flow_epsilon_pct,
			reopen_pct: config.reopen_pct,
			max_flips_total: config.max_flips_total,
			evidence_months: config.evidence_months,
		}
	}

	pub fn validate(&self) -> Result<(), &'static str> {
		if self.absorb_ref <= 0 || self.relay_ref <= 0 {
			return Err("absorb_ref and relay_ref must be positive");
		}
		if !(0..=50).contains(&self.flow_epsilon_pct) {
			return Err("flow_epsilon_pct must be within 0..=50");
		}
		if self.reopen_pct < self.flow_epsilon_pct || self.reopen_pct > 100 {
			return Err("reopen_pct must be within flow_epsilon_pct..=100");
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
	/// Net forward flow and the source node's pool it is judged against.
	pub net_flow: f64,
	pub pool: f64,
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
			"{verb} edge {} -> {} (net flow {:+.0} of pool {:.0})",
			self.node, target, self.net_flow, self.pool
		)
	}
}

/// One month of propagation. Returns, per node, its final pool (what the
/// node retained) so callers can log the emergent sinks.
pub fn propagate(
	nodes: &[NodeSnapshot],
	closed: &[Vec<bool>],
	params: &Params,
) -> Result<Vec<f64>, &'static str> {
	let n = nodes.len();
	if nodes.len() != closed.len() {
		return Err("closed mirror must match the node count");
	}
	params.validate()?;
	for (node, flags) in nodes.iter().zip(closed.iter()) {
		if flags.len() != node.outgoing.len() {
			return Err("closed mirror must match each node's edge count");
		}
	}
	// An undirected model edge is usable when at least one direction is a
	// vanilla edge that is not closed. Each direction carries the engine's
	// steering effort (the "push" from merchants and investments); the
	// reverse direction has zero steering, so active effort biases the
	// flow forward without blocking the bidirectional model.
	let mut usable: Vec<Vec<(u32, bool, f64)>> = vec![Vec::new(); n];
	for node in nodes {
		for (edge, target) in node.outgoing.iter().enumerate() {
			if !closed[node.id as usize][edge] {
				let effort = node.steering.get(edge).copied().unwrap_or(0.0);
				usable[node.id as usize].push((*target, true, effort));
				usable[*target as usize].push((node.id, false, 0.0));
			}
		}
	}
	for list in &mut usable {
		list.sort_unstable_by_key(|entry| entry.0);
		list.dedup_by(|a, b| a.0 == b.0);
	}
	let absorb = |tax: f64| tax / (tax + params.absorb_ref as f64);
	let mut pool: Vec<f64> = nodes.iter().map(|node| node.production).collect();
	let mut retained: Vec<f64> = vec![0.0; n];
	for _ in 0..RELAXATION_ROUNDS {
		let mut incoming = vec![0.0_f64; n];
		for node in nodes {
			let i = node.id as usize;
			let share = pool[i] * (1.0 - absorb(nodes[i].tax));
			if share <= 0.0 {
				continue;
			}
			let list = &usable[i];
			if list.is_empty() {
				retained[i] += share;
				continue;
			}
			let weights: Vec<f64> = list
				.iter()
				.map(|&(partner, _, effort)| {
					let other = &nodes[partner as usize];
					// Active steering effort (merchants, hegemony)
					// pulls flow in its direction; tax and trade power
					// provide the passive wealth gradient.
					1.0 + effort / 1000.0
						+ other.trade_power / params.relay_ref as f64
						+ other.tax / params.absorb_ref as f64
				})
				.collect();
			let total: f64 = weights.iter().sum();
			for (slot, &(partner, _, _)) in list.iter().enumerate() {
				incoming[partner as usize] += share * weights[slot] / total;
			}
		}
		for (i, p) in pool.iter().enumerate() {
			let keep = absorb(nodes[i].tax);
			retained[i] += p * keep;
		}
		std::mem::swap(&mut pool, &mut incoming);
		incoming.iter_mut().for_each(|v| *v = 0.0);
	}
	for (r, p) in retained.iter_mut().zip(pool.iter()) {
		*r += p;
	}
	Ok(retained)
}

/// Net flow through one vanilla edge (source -> target) during the last
/// relaxation: the relayed value is not tracked per edge by `propagate`,
/// so the planner approximates the net by comparing the retained pools —
/// an edge matters when the partner retained far more than the source
/// kept AND the vanilla direction agrees with that pull.
fn edge_net_flow(source: u32, target: u32, retained: &[f64], nodes: &[NodeSnapshot]) -> f64 {
	// An edge carries flow when the target absorbs more than the source
	// (it is a better sink downstream) or when the target relays more
	// (it is a better hub). When both retained zero, the relayed value
	// has already passed through; the edge was used.
	let sink_pull = retained[target as usize] - retained[source as usize];
	let hub_pull = nodes[target as usize].trade_power - nodes[source as usize].trade_power;
	sink_pull.max(hub_pull).max(0.0)
}

/// Per-edge evidence accumulator: how many consecutive months the net
/// flow has stayed under the epsilon (for closing) or above the reopen
/// threshold (for reopening). Trade is a long-term behavior; a single
/// month of noise is not enough to flip a route.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
	pub below_epsilon: u32,
	pub above_reopen: u32,
}

/// A new reverse edge the engine should grow: when a vanilla edge A→B
/// closes because the flow runs backward, and B produces value that A
/// would absorb, the model asks the engine to grow B→A so the next
/// propagation round sees the corrected direction.
#[derive(Clone, Debug, PartialEq)]
pub struct ReverseAdd {
	/// The node that gets the new outgoing edge (was the vanilla target).
	pub source: u32,
	/// Where the new edge points (was the vanilla source, now the sink).
	pub target: u32,
}

/// Plan this month's flips and reverse-edge additions using sustained
/// evidence. An edge closes only after `evidence_months` consecutive
/// months of below-epsilon flow, and reopens only after the same
/// sustained period above the reopen threshold. When a close happens on
/// an edge whose flow ran backward (the source retains far more than the
/// target and the target produces), a ReverseAdd is emitted so the
/// engine grows the correct direction.
pub fn plan(
	nodes: &[NodeSnapshot],
	closed: &mut [Vec<bool>],
	evidence: &mut [Vec<Evidence>],
	params: &Params,
) -> Result<(Vec<Flip>, Vec<ReverseAdd>), &'static str> {
	if nodes.len() != closed.len() || evidence.len() != closed.len() {
		return Err("mirrors must match the node count");
	}
	params.validate()?;
	for ((node, flags), ev) in nodes.iter().zip(closed.iter()).zip(evidence.iter()) {
		if flags.len() != node.outgoing.len() || ev.len() != node.outgoing.len() {
			return Err("mirrors must match each node's edge count");
		}
	}
	let retained = propagate(nodes, closed, params)?;
	let mut flips = Vec::new();
	let mut additions = Vec::new();
	let mut total = 0usize;
	for node in nodes {
		if node.outgoing.is_empty() || total >= params.max_flips_total {
			continue;
		}
		let i = node.id as usize;
		let reference = retained[i].max(node.production).max(1.0);
		for edge in 0..node.outgoing.len() {
			if total >= params.max_flips_total {
				break;
			}
			let target = node.outgoing[edge];
			let net = edge_net_flow(node.id, target, &retained, nodes);
			let is_closed = closed[i][edge];
			let below = net < reference * params.flow_epsilon_pct as f64 / 100.0;
			let above = net >= reference * params.reopen_pct as f64 / 100.0;

			// Accumulate evidence; reset the counter on any reversal.
			let ev = &mut evidence[i][edge];
			ev.below_epsilon = if below { ev.below_epsilon + 1 } else { 0 };
			ev.above_reopen = if above { ev.above_reopen + 1 } else { 0 };

			// Only flip after sustained evidence, never on a spike.
			let should_close = !is_closed && ev.below_epsilon >= params.evidence_months;
			let should_reopen = is_closed && ev.above_reopen >= params.evidence_months;
			if !should_close && !should_reopen {
				continue;
			}
			let want_closed = should_close;
			closed[i][edge] = want_closed;
			// Reset both counters on any flip.
			evidence[i][edge] = Evidence::default();
			flips.push(Flip {
				node: node.id,
				edge,
				action: if want_closed {
					FlipAction::Close
				} else {
					FlipAction::Reopen
				},
				net_flow: net,
				pool: reference,
			});
			total += 1;

			// Reverse-edge detection: when we close A→B because the flow
			// ran backward, and B produces value that A would absorb,
			// grow B→A so the next round routes correctly.
			if should_close {
				let target_node = &nodes[target as usize];
				let source_retained = retained[i];
				let target_retained = retained[target as usize];
				let reverse_viable =
					target_node.production > 0.0 && source_retained > target_retained * 2.0;
				if reverse_viable {
					additions.push(ReverseAdd {
						source: target,
						target: node.id,
					});
				}
			}
		}
	}
	Ok((flips, additions))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn node(id: u32, tax: f64, production: f64, power: f64, outgoing: Vec<u32>) -> NodeSnapshot {
		let steering = vec![0.0; outgoing.len()];
		NodeSnapshot {
			id,
			tax,
			production,
			trade_power: power,
			current_value: 0.0,
			outgoing,
			steering,
		}
	}

	#[allow(dead_code)]
	fn steered(
		id: u32,
		tax: f64,
		production: f64,
		power: f64,
		outgoing: Vec<u32>,
		steering: Vec<f64>,
	) -> NodeSnapshot {
		NodeSnapshot {
			id,
			tax,
			production,
			trade_power: power,
			current_value: 0.0,
			outgoing,
			steering,
		}
	}

	fn mirror(nodes: &[NodeSnapshot]) -> Vec<Vec<bool>> {
		nodes
			.iter()
			.map(|n| vec![false; n.outgoing.len()])
			.collect()
	}

	fn evidence(nodes: &[NodeSnapshot]) -> Vec<Vec<Evidence>> {
		nodes
			.iter()
			.map(|n| vec![Evidence::default(); n.outgoing.len()])
			.collect()
	}

	/// Run the planner for `months` iterations, feeding the same snapshot
	/// each time, to test sustained-evidence behavior.
	fn run_months(
		nodes: &[NodeSnapshot],
		closed: &mut [Vec<bool>],
		params: &Params,
		months: u32,
	) -> Vec<Flip> {
		let mut ev = evidence(nodes);
		let mut all = Vec::new();
		for _ in 0..months {
			all.extend(plan(nodes, closed, &mut ev, params).unwrap().0);
		}
		all
	}

	#[test]
	fn a_rich_sink_keeps_its_region() {
		// Two producers relay into a rich sink; the sink's one outgoing
		// edge (toward a poor node) dies, the feeding edges stay.
		let nodes = vec![
			node(0, 1000.0, 5000.0, 1000.0, vec![2]),
			node(1, 1000.0, 5000.0, 1000.0, vec![2]),
			node(2, 5000.0, 0.0, 1000.0, vec![3]),
			node(3, 500.0, 0.0, 1000.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let flips = run_months(&nodes, &mut closed, &Params::default(), 6);
		assert_eq!(flips.len(), 1);
		assert_eq!(flips[0].node, 2);
		assert_eq!(flips[0].action, FlipAction::Close);
		assert!(!closed[0][0] && !closed[1][0], "feeding edges stay open");
	}

	#[test]
	fn a_dead_edge_closes() {
		// A rich producer has one edge to a poor sink that absorbs
		// almost nothing; the pool stays at the producer, so the edge
		// carries under the epsilon and closes.
		let nodes = vec![
			node(0, 5000.0, 8000.0, 1000.0, vec![1]),
			node(1, 500.0, 0.0, 1000.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let params = Params::default();
		let flips = run_months(&nodes, &mut closed, &params, 6);
		assert!(!flips.is_empty());
		assert_eq!(flips[0].node, 0);
		assert_eq!(flips[0].action, FlipAction::Close);
	}

	#[test]
	fn the_sink_retains_more_than_the_hub() {
		// In a producer -> hub -> sink chain, the sink (highest tax)
		// retains the most value even with bidirectional backflow.
		let nodes = vec![
			node(0, 1000.0, 8000.0, 1000.0, vec![1]),
			node(1, 1000.0, 0.0, 5000.0, vec![2]),
			node(2, 5000.0, 0.0, 1000.0, vec![]),
		];
		let closed = mirror(&nodes);
		let retained = propagate(&nodes, &closed, &Params::default()).unwrap();
		assert!(
			retained[2] > retained[1],
			"sink ({:.0}) must retain more than hub ({:.0})",
			retained[2],
			retained[1]
		);
	}

	#[test]
	fn closure_is_gradual_and_budgeted() {
		// A chain of five dead edges closes at most max_flips_total per
		// month.
		let mut nodes = Vec::new();
		for i in 0..6 {
			let outgoing = if i == 5 { vec![] } else { vec![i + 1] };
			nodes.push(node(
				i,
				500.0,
				if i == 0 { 1000.0 } else { 0.0 },
				100.0,
				outgoing,
			));
		}
		let mut closed = mirror(&nodes);
		let params = Params {
			max_flips_total: 2,
			..Params::default()
		};
		let first = run_months(&nodes, &mut closed, &params, 6);
		assert_eq!(first.len(), 2);
	}

	#[test]
	fn hysteresis_holds_a_closed_edge() {
		// Once closed by low flow, an edge only reopens well above the
		// epsilon band.
		let nodes = vec![
			node(0, 1000.0, 8000.0, 1000.0, vec![1]),
			node(1, 5000.0, 0.0, 1000.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let _first = run_months(&nodes, &mut closed, &Params::default(), 6);
		let flips = run_months(&nodes, &mut closed, &Params::default(), 6);
		assert!(flips.is_empty(), "no month-to-month flapping");
	}

	#[test]
	fn mirrors_must_match_the_graph() {
		let nodes = vec![
			node(0, 1.0, 1.0, 1.0, vec![1]),
			node(1, 1.0, 1.0, 1.0, vec![]),
		];
		let mut mismatched = vec![vec![false, false], vec![]];
		assert!(
			plan(
				&nodes,
				&mut mismatched,
				&mut evidence(&nodes),
				&Params::default()
			)
			.is_err()
		);
	}

	#[test]
	fn invalid_params_are_rejected() {
		let nodes = vec![node(0, 1.0, 1.0, 1.0, vec![])];
		let mut closed = mirror(&nodes);
		let crossed = Params {
			reopen_pct: 0,
			..Params::default()
		};
		assert!(plan(&nodes, &mut closed, &mut evidence(&nodes), &crossed).is_err());
	}
}
