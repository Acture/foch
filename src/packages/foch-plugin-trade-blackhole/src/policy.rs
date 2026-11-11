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
	/// Per-province mean tax in thousandths: the economic wealth
	/// indicator for the neighborhood-peak check (is this node the
	/// wealth center of its region?).
	pub tax: f64,
	/// The engine's own retention rate: retained / (retained +
	/// transferred), from node+0xb0 and +0xbc. This reflects the actual
	/// game mechanics — trade power shares, merchant modes, modifiers.
	/// Used by the propagation to determine how much value stays.
	pub retention: f64,
	/// Local trade value (production × price): the injection.
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
			relay_ref: config.relay_ref,
			flow_epsilon_pct: config.flow_epsilon_pct,
			reopen_pct: config.reopen_pct,
			max_flips_total: config.max_flips_total,
			evidence_months: config.evidence_months,
		}
	}

	pub fn validate(&self) -> Result<(), &'static str> {
		if self.relay_ref <= 0 {
			return Err("relay_ref must be positive");
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

/// The result of one month's propagation: what each node retained, and
/// the net flow through each vanilla edge (positive = forward, matching
/// the vanilla direction; negative = reverse).
pub struct PropagationResult {
	pub retained: Vec<f64>,
	/// Per-node, per-vanilla-edge net flow accumulated over all rounds.
	pub edge_flows: Vec<Vec<f64>>,
}

/// One month of propagation. Tracks the actual value relayed through
/// each vanilla edge (forward minus reverse over all relaxation rounds),
/// so the planner judges edges by real flow volume rather than a
/// retained-value difference that misses transit entirely.
pub fn propagate(
	nodes: &[NodeSnapshot],
	closed: &[Vec<bool>],
	params: &Params,
) -> Result<PropagationResult, &'static str> {
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
	// Build a map from (node, partner) to the vanilla edge index and
	// direction, so per-edge flow can be tracked during the relaxation.
	// forward_edges[i][partner] = edge index in i's outgoing pointing
	// at partner. reverse_edges[i][partner] = edge index in partner's
	// outgoing pointing at i.
	let mut forward_edges: Vec<std::collections::HashMap<u32, usize>> =
		vec![std::collections::HashMap::new(); n];
	// The propagation ALWAYS uses the full vanilla graph: every edge is
	// open in the model regardless of what we've closed in the engine.
	// This means (a) a closed edge's flow can recover and trigger a
	// reopen, and (b) the model never sees its own closures, so no
	// cascade/feedback effect — the topology emerges from where value
	// wants to flow, not from iteratively pruning and re-converging.
	for node in nodes {
		for (edge, target) in node.outgoing.iter().enumerate() {
			forward_edges[node.id as usize].insert(*target, edge);
		}
	}
	let mut usable: Vec<Vec<(u32, f64, bool, usize)>> = vec![Vec::new(); n];
	for node in nodes {
		for (edge, target) in node.outgoing.iter().enumerate() {
			{
				let effort = node.steering.get(edge).copied().unwrap_or(0.0);
				// Forward for the source: (target, effort, true, edge)
				usable[node.id as usize].push((*target, effort, true, edge));
				// Reverse for the target: (source, 0, false, edge_in_source)
				usable[*target as usize].push((node.id, 0.0, false, edge));
			}
		}
	}
	for list in &mut usable {
		list.sort_unstable_by_key(|entry| entry.0);
		list.dedup_by(|a, b| a.0 == b.0);
	}
	// The engine's own retention rate: no artificial parameters. Each
	// node absorbs this fraction of whatever pools in it; the rest
	// relays onward. The relay weight uses trade power (multi-hop hub
	// pull) and steering effort (directional push); retention is the
	// engine's answer, not ours.
	let retention: Vec<f64> = nodes.iter().map(|n| n.retention).collect();
	let mut pool: Vec<f64> = nodes.iter().map(|node| node.production).collect();
	let mut retained: Vec<f64> = vec![0.0; n];
	// Per-vanilla-edge accumulated net flow: positive = forward.
	let mut edge_flows: Vec<Vec<f64>> = nodes.iter().map(|n| vec![0.0; n.outgoing.len()]).collect();
	for _ in 0..RELAXATION_ROUNDS {
		let mut incoming = vec![0.0_f64; n];
		for node in nodes {
			let i = node.id as usize;
			let share = pool[i] * (1.0 - retention[i]);
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
				.map(|&(partner, effort, _, _)| {
					let other = &nodes[partner as usize];
					// Relay: trade power is the hub pull, steering is the
					// directional push. Retention (the engine's own rate)
					// determines what stays; this determines where the
					// rest goes.
					1.0 + effort / 1000.0 + other.trade_power / params.relay_ref as f64
				})
				.collect();
			let total: f64 = weights.iter().sum();
			for (slot, &(partner, _, is_forward, edge_idx)) in list.iter().enumerate() {
				let amount = share * weights[slot] / total;
				incoming[partner as usize] += amount;
				// Accumulate per-edge flow: forward adds, reverse subtracts.
				if is_forward {
					edge_flows[i][edge_idx] += amount;
				} else {
					// The vanilla edge lives in the partner's outgoing;
					// subtract from its net (reverse direction).
					edge_flows[partner as usize][edge_idx] -= amount;
				}
			}
		}
		for (i, p) in pool.iter().enumerate() {
			retained[i] += p * retention[i];
		}
		std::mem::swap(&mut pool, &mut incoming);
		incoming.iter_mut().for_each(|v| *v = 0.0);
	}
	for (r, p) in retained.iter_mut().zip(pool.iter()) {
		*r += p;
	}
	Ok(PropagationResult {
		retained,
		edge_flows,
	})
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

/// Monthly trade intelligence: where value converges and where the
/// steering effort is mismatched with the flow potential. This is the
/// transit-fee and hegemony-recommendation layer: it tells the player
/// (and the model) which nodes are the high-value transit corridors and
/// which are under-steered relative to their potential.
pub fn transit_intelligence(
	nodes: &[NodeSnapshot],
	closed: &[Vec<bool>],
	params: &Params,
) -> Vec<String> {
	let Ok(result) = propagate(nodes, closed, params) else {
		return Vec::new();
	};
	// Transit volume: the actual value relayed through this node from
	// upstream (the sum of positive inflows on incoming edges).
	let mut rows: Vec<(u32, f64, f64, f64)> = Vec::new();
	for node in nodes {
		// Sum the positive incoming flows from the per-edge tracking:
		// any node j whose edge to us carries positive net flow.
		let mut transit = 0.0;
		for other in nodes {
			if other.id == node.id {
				continue;
			}
			for (edge, target) in other.outgoing.iter().enumerate() {
				if *target == node.id {
					let flow = result.edge_flows[other.id as usize][edge];
					if flow > 0.0 {
						transit += flow;
					}
				}
			}
		}
		let steering: f64 = node.steering.iter().sum();
		rows.push((node.id, transit, steering, node.trade_power));
	}
	rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
	let mut lines = Vec::new();
	for (id, transit, steering, power) in rows.iter().take(10) {
		if *transit < 100.0 {
			continue;
		}
		let gap = if *steering < transit * 0.1 {
			" ← under-steered"
		} else {
			""
		};
		lines.push(format!(
			"transit node {id}: volume {transit:.0}, steering {steering:.0}, power {power:.0}{gap}"
		));
	}
	lines
}

/// One node's monthly transit-fee pool: the reward for a node whose
/// through-flow runs a surplus (顺差) — more value flows in than flows
/// out, so countries holding trade power there are relaying real trade
/// and earn a cut of it without collecting either end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitFeePool {
	pub node: u32,
	/// Net through-flow this month: positive inflow on incoming edges
	/// minus positive outflow on outgoing edges. Local production is
	/// neither imported nor exported, so it stays out of the balance.
	pub surplus: f64,
	/// The pool to distribute: `surplus × fee_pct/100`.
	pub fee_pool: f64,
}

/// Compute every node's transit-fee pool from the propagation's per-edge
/// flows. Deficit nodes (逆差, outflow exceeds inflow) get nothing: the
/// flow is leaving them and the sink downstream already profits by
/// retaining it.
pub fn transit_fee_pools(
	nodes: &[NodeSnapshot],
	closed: &[Vec<bool>],
	params: &Params,
	fee_pct: i64,
) -> Vec<TransitFeePool> {
	let Ok(result) = propagate(nodes, closed, params) else {
		return Vec::new();
	};
	// Per-edge nets are forward-positive by convention: an edge's source
	// holds it, so outflow sums a node's own edges and inflow sums the
	// edges of everyone pointing at it.
	let mut inflow = vec![0.0_f64; nodes.len()];
	let mut outflow = vec![0.0_f64; nodes.len()];
	for node in nodes {
		for (edge, &target) in node.outgoing.iter().enumerate() {
			let net = result.edge_flows[node.id as usize][edge];
			if net > 0.0 {
				outflow[node.id as usize] += net;
				inflow[target as usize] += net;
			}
		}
	}
	let mut pools = Vec::new();
	for node in nodes {
		let surplus = inflow[node.id as usize] - outflow[node.id as usize];
		if surplus > 0.0 {
			pools.push(TransitFeePool {
				node: node.id,
				surplus,
				fee_pool: surplus * fee_pct as f64 / 100.0,
			});
		}
	}
	pools.sort_by(|a, b| {
		b.fee_pool
			.partial_cmp(&a.fee_pool)
			.unwrap_or(std::cmp::Ordering::Equal)
	});
	pools
}

/// Monthly report line for every closed edge: its current net flow against
/// the reference the thresholds use. This is the reopen diagnostics — the
/// evidence counters are invisible otherwise, and a closed edge that never
/// reopens can only be debugged by watching how close its net comes to the
/// `reopen_pct` band month after month.
pub fn closed_edge_report(
	nodes: &[NodeSnapshot],
	closed: &[Vec<bool>],
	params: &Params,
) -> Vec<String> {
	let Ok(result) = propagate(nodes, closed, params) else {
		return Vec::new();
	};
	let mut lines = Vec::new();
	for node in nodes {
		let i = node.id as usize;
		let reference = result.retained[i].max(node.production).max(1.0);
		for (edge, &target) in node.outgoing.iter().enumerate() {
			if !closed[i][edge] {
				continue;
			}
			let net = result.edge_flows[i][edge];
			lines.push(format!(
				"closed edge {} -> {}: net {net:+.0} / ref {reference:.0} ({:+.1}%)",
				node.id,
				target,
				100.0 * net / reference
			));
		}
	}
	lines
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
	let result = propagate(nodes, closed, params)?;
	let mut flips = Vec::new();
	let mut additions = Vec::new();
	let mut total = 0usize;
	for node in nodes {
		if node.outgoing.is_empty() || total >= params.max_flips_total {
			continue;
		}
		let i = node.id as usize;
		let reference = result.retained[i].max(node.production).max(1.0);
		for edge in 0..node.outgoing.len() {
			if total >= params.max_flips_total {
				break;
			}
			let target = node.outgoing[edge];
			let target_node = &nodes[target as usize];
			// Actual per-edge net flow from the propagation, not a
			// retained-value difference that conflates all downstream.
			let net = result.edge_flows[i][edge];
			let is_closed = closed[i][edge];
			let below = net < reference * params.flow_epsilon_pct as f64 / 100.0;
			let above = net >= reference * params.reopen_pct as f64 / 100.0;
			// Per-edge wealth gradient: value should not flow from a
			// richer node to a poorer one. Each edge is judged on its
			// own source-vs-target comparison, not against the entire
			// neighborhood — Beijing can close its edge to a poor target
			// even when a richer neighbor sends value to Beijing.
			let wealth_gradient = node.tax >= target_node.tax;

			// Accumulate evidence; reset the counter on any reversal.
			let ev = &mut evidence[i][edge];
			ev.below_epsilon = if below { ev.below_epsilon + 1 } else { 0 };
			ev.above_reopen = if above { ev.above_reopen + 1 } else { 0 };

			// Close requires BOTH conditions: sustained flow evidence
			// AND the node is the neighborhood tax peak (economic
			// justification). Reopen only needs flow recovery.
			let should_close =
				!is_closed && ev.below_epsilon >= params.evidence_months && wealth_gradient;
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
				let source_retained = result.retained[i];
				let target_retained = result.retained[target as usize];
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

	fn node(
		id: u32,
		tax: f64,
		retention: f64,
		production: f64,
		power: f64,
		outgoing: Vec<u32>,
	) -> NodeSnapshot {
		let steering = vec![0.0; outgoing.len()];
		NodeSnapshot {
			id,
			tax,
			retention,
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
		retention: f64,
		production: f64,
		power: f64,
		outgoing: Vec<u32>,
		steering: Vec<f64>,
	) -> NodeSnapshot {
		NodeSnapshot {
			id,
			tax,
			retention,
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
		// A node with high retention (engine says 95% stays) AND the
		// highest tax in its neighborhood closes its outgoing edge.
		let nodes = vec![
			node(0, 1000.0, 0.3, 5000.0, 1000.0, vec![2]),
			node(1, 1000.0, 0.3, 5000.0, 1000.0, vec![2]),
			node(2, 5000.0, 0.95, 0.0, 1000.0, vec![3]),
			node(3, 2000.0, 0.3, 0.0, 1000.0, vec![]),
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
		// Retention 98%: relay 2% < epsilon 5%, plus tax-peak.
		let nodes = vec![
			node(0, 5000.0, 0.98, 8000.0, 1000.0, vec![1]),
			node(1, 500.0, 0.3, 0.0, 1000.0, vec![]),
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
			node(0, 1000.0, 0.3, 8000.0, 1000.0, vec![1]),
			node(1, 1000.0, 0.2, 0.0, 5000.0, vec![2]),
			node(2, 5000.0, 0.9, 0.0, 1000.0, vec![]),
		];
		let closed = mirror(&nodes);
		let result = propagate(&nodes, &closed, &Params::default()).unwrap();
		assert!(
			result.retained[2] > result.retained[1],
			"sink ({:.0}) must retain more than hub ({:.0})",
			result.retained[2],
			result.retained[1]
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
				200000.0,
				0.95,
				if i == 0 { 10000.0 } else { 0.0 },
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
			node(0, 1000.0, 0.5, 8000.0, 1000.0, vec![1]),
			node(1, 5000.0, 0.5, 0.0, 1000.0, vec![]),
		];
		let mut closed = mirror(&nodes);
		let _first = run_months(&nodes, &mut closed, &Params::default(), 6);
		let flips = run_months(&nodes, &mut closed, &Params::default(), 6);
		assert!(flips.is_empty(), "no month-to-month flapping");
	}

	#[test]
	fn transit_intelligence_finds_corridors() {
		// A hub between a producer and a sink carries transit flow.
		let nodes = vec![
			node(0, 1000.0, 0.3, 8000.0, 1000.0, vec![1]),
			node(1, 1000.0, 0.2, 0.0, 5000.0, vec![2]),
			node(2, 5000.0, 0.9, 0.0, 1000.0, vec![]),
		];
		let closed = mirror(&nodes);
		let report = transit_intelligence(&nodes, &closed, &Params::default());
		assert!(!report.is_empty(), "sink should appear as transit target");
	}

	#[test]
	fn a_relay_node_earns_a_fee_and_the_producer_pays_the_flow() {
		// Producer -> hub -> sink: the hub relays more than it sources,
		// so it runs a through-flow surplus and earns a fee pool; the
		// producer's own production does not count as imported value.
		let nodes = vec![
			node(0, 1000.0, 0.1, 8000.0, 1000.0, vec![1]),
			node(1, 1000.0, 0.1, 0.0, 5000.0, vec![2]),
			node(2, 5000.0, 0.95, 0.0, 1000.0, vec![]),
		];
		let closed = mirror(&nodes);
		let pools = transit_fee_pools(&nodes, &closed, &Params::default(), 20);
		let hub = pools
			.iter()
			.find(|p| p.node == 1)
			.expect("hub earns a pool");
		assert!(hub.surplus > 0.0);
		assert!((hub.fee_pool - hub.surplus * 0.2).abs() < 1e-6);
		// The producer exports everything it makes: deficit, no pool.
		assert!(pools.iter().all(|p| p.node != 0), "producer gets nothing");
	}

	#[test]
	fn a_deficit_node_gets_no_pool() {
		// A node whose outflow exceeds inflow (逆差) earns nothing.
		let nodes = vec![
			node(0, 1000.0, 0.1, 9000.0, 1000.0, vec![1]),
			node(1, 1000.0, 0.9, 0.0, 1000.0, vec![]),
		];
		let closed = mirror(&nodes);
		let pools = transit_fee_pools(&nodes, &closed, &Params::default(), 20);
		assert!(pools.iter().all(|p| p.node != 0));
	}

	#[test]
	fn mirrors_must_match_the_graph() {
		let nodes = vec![
			node(0, 1.0, 0.5, 1.0, 1.0, vec![1]),
			node(1, 1.0, 0.5, 1.0, 1.0, vec![]),
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
		let nodes = vec![node(0, 1.0, 0.5, 1.0, 1.0, vec![])];
		let mut closed = mirror(&nodes);
		let crossed = Params {
			reopen_pct: 0,
			..Params::default()
		};
		assert!(plan(&nodes, &mut closed, &mut evidence(&nodes), &crossed).is_err());
	}
}
