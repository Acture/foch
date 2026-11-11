//! Monthly orchestration: observe the live trade graph, decide, mutate.
//!
//! The live-memory side is isolated behind [`EngineBackend`] so the RE
//! spike's layout lands in one place; the orchestration itself — mirror
//! maintenance, force-toggle, policy planning, logging — is engine-free and
//! covered by tests with a scripted backend.

use crate::config::{Config, Mode};
use crate::logging;
use crate::policy::{self, FlipAction, NodeSnapshot};

/// Reads and mutates the engine's trade structures on the game thread.
///
/// Every method runs inside the monthly detour and must not panic; the
/// detour additionally catches unwinds at its own ABI boundary.
pub trait EngineBackend {
	/// One snapshot of every node, in engine order, with vanilla outgoing
	/// edges. Error strings are short and static; they surface as
	/// `EngineError` details in the log.
	fn observe(&mut self) -> Result<Vec<NodeSnapshot>, &'static str>;

	/// The engine index of a node name such as `lubeck_trade_node`.
	fn node_index(&self, name: &str) -> Option<u32>;

	/// Close or reopen one vanilla edge (node, engine edge slot).
	fn set_edge_closed(&mut self, node: u32, edge: usize, closed: bool)
	-> Result<(), &'static str>;

	/// One log line about the applied state of the backend.
	fn describe(&self) -> &'static str;

	/// The countries holding trade power in one node, as `(country
	/// index, power)` pairs, for the transit-fee distribution.
	fn node_participants(&mut self, _node: u32) -> Vec<(u32, f64)> {
		Vec::new()
	}

	/// Add to one country's treasury (engine thousandths). The transit
	/// fee's payment primitive.
	fn pay_country(&mut self, _country: u32, _amount: f64) -> Result<(), &'static str> {
		Ok(())
	}

	/// Hand the engine-side sources of this tick to the backend: the
	/// global context and the trade database embedded in it.
	fn set_source(&mut self, _context: usize, _db: usize) {}
}

/// Per-edge closed mirror over the frozen vanilla graph, plus the monthly
/// counters the modes report.
pub struct Engine<B: EngineBackend, F: Fn(i32, &str)> {
	backend: B,
	config: Config,
	/// `None` until the first successful observe freezes the graph shape.
	closed: Option<Vec<Vec<bool>>>,
	/// Per-edge sustained-evidence counters, parallel to `closed`.
	evidence: Option<Vec<Vec<policy::Evidence>>>,
	/// Ticks seen; each one is one monthly-cadence step of the hook.
	ticks: u32,
	force_closed: bool,
	log: F,
}

impl<B: EngineBackend, F: Fn(i32, &str)> Engine<B, F> {
	pub fn new(backend: B, config: Config, log: F) -> Self {
		Self {
			backend,
			config,
			closed: None,
			evidence: None,
			ticks: 0,
			force_closed: false,
			log,
		}
	}

	/// Hand the engine-side sources of this tick (the hook's context and
	/// the trade database embedded in it) to the backend.
	pub fn set_source(&mut self, context: usize, db: usize) {
		self.backend.set_source(context, db);
	}

	/// One hook tick on the game thread. The hooked economy update runs
	/// on the game's monthly cadence, so every tick is one monthly step;
	/// the tick log carries the event timestamps that prove the cadence.
	pub fn tick(&mut self) {
		self.ticks += 1;
		if self.ticks <= 10 || self.ticks.is_multiple_of(100) {
			(self.log)(logging::INFO, &format!("tick {}", self.ticks));
		}
		let nodes = match self.backend.observe() {
			Ok(nodes) => nodes,
			Err(reason) => {
				(self.log)(logging::WARN, &format!("observe failed: {reason}"));
				return;
			}
		};
		if self.closed.is_none() {
			let mirror = nodes
				.iter()
				.map(|n| vec![false; n.outgoing.len()])
				.collect();
			self.closed = Some(mirror);
			self.evidence = Some(
				nodes
					.iter()
					.map(|n| vec![policy::Evidence::default(); n.outgoing.len()])
					.collect(),
			);
			(self.log)(
				logging::INFO,
				&format!(
					"graph frozen: {} nodes, {} edges, backend {}",
					nodes.len(),
					nodes.iter().map(|n| n.outgoing.len()).sum::<usize>(),
					self.backend.describe()
				),
			);
		}
		if self.config.observer_log {
			self.log_observer(&nodes);
		}
		if !self.config.enabled {
			return;
		}
		match self.config.mode {
			Mode::Observe => {}
			Mode::ForceToggle => self.tick_force_toggle(),
			Mode::Policy => self.tick_policy(&nodes),
		}
	}

	fn log_observer(&self, nodes: &[NodeSnapshot]) {
		for node in nodes {
			(self.log)(
				logging::DEBUG,
				&format!(
					"tick {} node {} tax {:.1} prod {:.1} power {:.1} value {:.1} retention {:.3} edges {}",
					self.ticks,
					node.id,
					node.tax,
					node.production,
					node.trade_power,
					node.current_value,
					node.retention,
					node.outgoing.len()
				),
			);
		}
	}

	/// Prove the mutation primitive: on every in-game month, toggle every
	/// edge of one configured node, alternating close and reopen.
	fn tick_force_toggle(&mut self) {
		let Some(index) = self.backend.node_index(&self.config.force_node) else {
			(self.log)(
				logging::WARN,
				&format!("force_toggle: node {} not found", self.config.force_node),
			);
			return;
		};
		self.force_closed = !self.force_closed;
		// Iterate the frozen mirror's vanilla edge count, not the live
		// snapshot: a closed node's own snapshot no longer lists its
		// edges, so reopening from the snapshot would loop zero times.
		let vanilla_edges = self
			.closed
			.as_ref()
			.and_then(|flags| flags.get(index as usize))
			.map(|flags| flags.len())
			.unwrap_or_default();
		let mut flipped = 0usize;
		for edge in 0..vanilla_edges {
			match self.backend.set_edge_closed(index, edge, self.force_closed) {
				Ok(()) => {
					flipped += 1;
					if let Some(flag) = self
						.closed
						.as_mut()
						.and_then(|flags| flags.get_mut(index as usize))
						.and_then(|flags| flags.get_mut(edge))
					{
						*flag = self.force_closed;
					}
				}
				Err(reason) => {
					(self.log)(
						logging::WARN,
						&format!("force_toggle edge {edge} failed: {reason}"),
					);
				}
			}
		}
		(self.log)(
			logging::INFO,
			&format!(
				"force_toggle tick {}: node {} {} {} edges",
				self.ticks,
				index,
				if self.force_closed {
					"closed"
				} else {
					"reopened"
				},
				flipped
			),
		);
	}

	fn tick_policy(&mut self, nodes: &[NodeSnapshot]) {
		let Some(closed) = self.closed.as_mut() else {
			return;
		};
		let Some(evidence) = self.evidence.as_mut() else {
			return;
		};
		let params = policy::Params::from(self.config.clone());
		let Ok((flips, _)) = policy::plan(nodes, closed, evidence, &params) else {
			(self.log)(logging::WARN, "policy rejected its parameters");
			return;
		};
		for flip in &flips {
			let closed = matches!(flip.action, FlipAction::Close);
			if let Err(reason) = self.backend.set_edge_closed(flip.node, flip.edge, closed) {
				(self.log)(
					logging::WARN,
					&format!("flip failed ({}): {reason}", flip.describe(nodes)),
				);
			} else {
				(self.log)(
					logging::INFO,
					&format!("tick {}: {}", self.ticks, flip.describe(nodes)),
				);
			}
		}
		// Transit intelligence: which corridors carry the most flow and
		// where the steering effort is mismatched with the potential.
		// This is the trade-company layer — it tells a hegemon where to
		// invest for maximum carriage return.
		for line in policy::transit_intelligence(nodes, self.closed.as_ref().unwrap(), &params) {
			(self.log)(logging::INFO, &line);
		}
		// Reopen diagnostics: every closed edge's net flow against the
		// threshold band, so a stuck reopen is visible month by month.
		for line in policy::closed_edge_report(nodes, self.closed.as_ref().unwrap(), &params) {
			(self.log)(logging::DEBUG, &line);
		}
		// The role incomes: transit fees pay the volume a node forwards
		// for others (trade-power role, pure relays earn the most) and
		// source bounties pay the local injection that leaves the node
		// (production role, farming upstream pays). Sinks keep their
		// vanilla collection income and earn neither.
		if self.config.transit_fee {
			self.pay_fee_pools(nodes, &params);
		}
	}

	/// Distribute this month's fee pools by trade-power share.
	fn pay_fee_pools(&mut self, nodes: &[NodeSnapshot], params: &policy::Params) {
		let pools = policy::fee_pools(
			nodes,
			self.closed.as_ref().unwrap(),
			params,
			self.config.transit_fee_pct,
			self.config.source_fee_pct,
		);
		for pool in pools {
			let participants = self.backend.node_participants(pool.node);
			let total: f64 = participants.iter().map(|(_, power)| power).sum();
			if total <= 0.0 || participants.is_empty() {
				continue;
			}
			let mut paid = Vec::new();
			for (country, power) in participants {
				let amount = pool.pool * power / total;
				if amount < 1.0 {
					continue;
				}
				match self.backend.pay_country(country, amount) {
					Ok(()) => paid.push(format!(
						"country {country} {amount:.0} ({:.0}%)",
						100.0 * power / total
					)),
					Err(reason) => (self.log)(
						logging::WARN,
						&format!(
							"{} to country {country} failed: {reason}",
							pool.kind.label()
						),
					),
				}
			}
			if !paid.is_empty() {
				(self.log)(
					logging::INFO,
					&format!(
						"{} node {}: volume {:.0}, pool {:.0} -> {}",
						pool.kind.label(),
						pool.node,
						pool.volume,
						pool.pool,
						paid.join(", ")
					),
				);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::RefCell;
	use std::rc::Rc;

	/// Scripted backend over a small graph, sharing its state so tests keep
	/// reading the mutation log while the engine owns the backend value.
	struct FakeBackend {
		inner: Rc<RefCell<Inner>>,
	}

	struct Inner {
		nodes: Vec<NodeSnapshot>,
		names: Vec<&'static str>,
		fail_set: bool,
		mutations: Vec<String>,
		participants: Vec<(u32, Vec<(u32, f64)>)>,
		payments: Vec<(u32, f64)>,
	}

	impl FakeBackend {
		fn new(inner: Rc<RefCell<Inner>>) -> Self {
			Self { inner }
		}
	}

	fn inner() -> Rc<RefCell<Inner>> {
		// lubeck(0, rich) -> english_channel(1); venice(2) is an end node.
		let node = |id: u32, tax: f64, outgoing: Vec<u32>| {
			let steering = vec![0.0; outgoing.len()];
			NodeSnapshot {
				id,
				tax,
				retention: 0.5,
				production: 0.0,
				trade_power: 0.0,
				current_value: 0.0,
				outgoing,
				steering,
			}
		};
		Rc::new(RefCell::new(Inner {
			nodes: vec![
				node(0, 100.0, vec![1]),
				node(1, 10.0, vec![]),
				node(2, 5.0, vec![]),
			],
			names: vec![
				"lubeck_trade_node",
				"english_channel_trade_node",
				"venice_trade_node",
			],
			fail_set: false,
			mutations: Vec::new(),
			participants: Vec::new(),
			payments: Vec::new(),
		}))
	}

	impl EngineBackend for FakeBackend {
		fn observe(&mut self) -> Result<Vec<NodeSnapshot>, &'static str> {
			Ok(self.inner.borrow().nodes.clone())
		}

		fn node_index(&self, name: &str) -> Option<u32> {
			self.inner
				.borrow()
				.names
				.iter()
				.position(|n| *n == name)
				.map(|i| i as u32)
		}

		fn set_edge_closed(
			&mut self,
			node: u32,
			edge: usize,
			closed: bool,
		) -> Result<(), &'static str> {
			let mut inner = self.inner.borrow_mut();
			if inner.fail_set {
				return Err("mutation refused");
			}
			inner
				.mutations
				.push(format!("{node}:{edge}:{}", closed as u8));
			Ok(())
		}

		fn describe(&self) -> &'static str {
			"fake"
		}

		fn node_participants(&mut self, node: u32) -> Vec<(u32, f64)> {
			self.inner
				.borrow()
				.participants
				.iter()
				.find(|(n, _)| *n == node)
				.map(|(_, list)| list.clone())
				.unwrap_or_default()
		}

		fn pay_country(&mut self, country: u32, amount: f64) -> Result<(), &'static str> {
			self.inner.borrow_mut().payments.push((country, amount));
			Ok(())
		}
	}

	fn recording_logger(sink: &RefCell<Vec<String>>) -> impl Fn(i32, &str) + '_ {
		move |level, message| {
			sink.borrow_mut().push(format!("{level} {message}"));
		}
	}

	fn make(
		config: Config,
		state: &Rc<RefCell<Inner>>,
		logs: &RefCell<Vec<String>>,
	) -> Engine<FakeBackend, impl Fn(i32, &str)> {
		Engine::new(
			FakeBackend::new(Rc::clone(state)),
			config,
			move |level, message| recording_logger(logs)(level, message),
		)
	}

	#[test]
	fn observe_mode_never_mutates() {
		let state = inner();
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::Observe,
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		engine.tick();
		assert!(state.borrow().mutations.is_empty());
	}

	#[test]
	fn force_toggle_alternates_every_month() {
		let state = inner();
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::ForceToggle,
				force_node: "lubeck_trade_node".to_string(),
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		assert_eq!(state.borrow().mutations, vec!["0:0:1"]);
		engine.tick();
		assert_eq!(state.borrow().mutations, vec!["0:0:1", "0:0:0"]);
		engine.tick();
		assert_eq!(state.borrow().mutations, vec!["0:0:1", "0:0:0", "0:0:1"]);
	}

	#[test]
	fn policy_mode_closes_the_rich_nodes_edge_and_holds() {
		let state = inner();
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::Policy,
				// One-month evidence: unit tests exercise the mechanism,
				// not the sustained-evidence patience.
				evidence_months: 1,
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		assert_eq!(state.borrow().mutations, vec!["0:0:1"]);
		// The next month holds: the edge is closed and below reopen.
		engine.tick();
		assert_eq!(state.borrow().mutations, vec!["0:0:1"]);
	}

	#[test]
	fn mutation_failures_are_logged_not_fatal() {
		let state = inner();
		state.borrow_mut().fail_set = true;
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::ForceToggle,
				force_node: "lubeck_trade_node".to_string(),
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		let messages = logs.borrow().join("\n");
		assert!(messages.contains("failed"), "logged: {messages}");
		assert!(state.borrow().mutations.is_empty());
	}

	#[test]
	fn transit_fees_pay_participants_by_power_share() {
		// A producer feeds node 1 (the transit hub): its pool is shared
		// by two countries 3:1 by trade power.
		let state = inner();
		{
			let mut inner = state.borrow_mut();
			inner.nodes = vec![
				NodeSnapshot {
					id: 0,
					tax: 1000.0,
					retention: 0.1,
					production: 8000.0,
					trade_power: 1000.0,
					current_value: 0.0,
					outgoing: vec![1],
					steering: vec![0.0],
				},
				NodeSnapshot {
					id: 1,
					tax: 1000.0,
					retention: 0.1,
					production: 0.0,
					trade_power: 5000.0,
					current_value: 0.0,
					outgoing: vec![],
					steering: vec![],
				},
			];
			inner.participants = vec![(0, vec![(7, 3000.0), (9, 1000.0)])];
		}
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::Policy,
				evidence_months: 1,
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		let payments = state.borrow().payments.clone();
		assert_eq!(payments.len(), 2, "both participants paid");
		let total: f64 = payments.iter().map(|(_, a)| a).sum();
		let by7 = payments.iter().find(|(c, _)| *c == 7).unwrap().1;
		let by9 = payments.iter().find(|(c, _)| *c == 9).unwrap().1;
		assert!(
			(by7 - 3.0 * by9).abs() < 1e-6,
			"3:1 by power, got {by7} vs {by9}"
		);
		assert!(total > 0.0);
		let messages = logs.borrow().join("\n");
		assert!(
			messages.contains("source bounty node 0"),
			"the producer's export pays its holders: {messages}"
		);
	}

	#[test]
	fn transit_fees_can_be_disabled() {
		let state = inner();
		{
			let mut inner = state.borrow_mut();
			inner.nodes = vec![NodeSnapshot {
				id: 0,
				tax: 1000.0,
				retention: 0.1,
				production: 8000.0,
				trade_power: 1000.0,
				current_value: 0.0,
				outgoing: vec![],
				steering: vec![],
			}];
			inner.participants = vec![(0, vec![(7, 100.0)])];
		}
		let logs = RefCell::new(Vec::new());
		let mut engine = make(
			Config {
				mode: Mode::Policy,
				transit_fee: false,
				..Config::default()
			},
			&state,
			&logs,
		);
		engine.tick();
		assert!(state.borrow().payments.is_empty());
	}

	#[test]
	fn the_first_tick_freezes_the_mirror_from_the_graph() {
		let state = inner();
		let logs = RefCell::new(Vec::new());
		let mut engine = make(Config::default(), &state, &logs);
		engine.tick();
		let messages = logs.borrow().join("\n");
		assert!(
			messages.contains("graph frozen: 3 nodes, 1 edges"),
			"logged: {messages}"
		);
	}
}
