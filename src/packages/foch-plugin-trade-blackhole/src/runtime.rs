//! The live engine backend over the reversed trade structures, plus the
//! recalculation entry the detour drives.
//!
//! The hook hands the database pointer to [`monthly`]; the backend walks
//! `CTradeNodeDataBase` exactly as the RE spike documented it (see
//! [`crate::offsets::layout`]). M1 mutation granularity is whole-node:
//! closing swaps the route vector's end back to its begin (an empty vector
//! is the engine's own representation of an end node) and reopening
//! restores the cached original end. Per-edge flips with entry reordering
//! arrive with the policy milestone.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::Config;
use crate::engine::{Engine, EngineBackend};
use crate::offsets::layout;
use crate::policy::NodeSnapshot;

type LiveEngine = Engine<LiveBackend, fn(i32, &str)>;

static ENGINE: OnceLock<Mutex<LiveEngine>> = OnceLock::new();
static EXE_BASE: AtomicUsize = AtomicUsize::new(0);

/// Install the engine the economy-update detour drives. `exe_base` is the
/// host-reported image base, used to cross-check the context pointer.
pub fn install(config: Config, exe_base: usize) {
	EXE_BASE.store(exe_base, Ordering::Release);
	let _ = ENGINE.set(Mutex::new(Engine::new(
		LiveBackend::new(),
		config,
		crate::logging::write,
	)));
}

/// One economy-update step on the game thread. `context` is the hook's
/// first argument and `db` the embedded trade database; both are validated
/// against the context global before the engine touches anything.
pub fn monthly(context: usize, db: usize) {
	let Some(engine) = ENGINE.get() else { return };
	let base = EXE_BASE.load(Ordering::Acquire);
	if base > 0x1_0000 {
		let stored =
			unsafe { std::ptr::read((base + layout::CONTEXT_GLOBAL_RVA) as *const u64) } as usize;
		if stored != context {
			crate::logging::write(
				crate::logging::WARN,
				&format!(
					"context 0x{context:x} is not the global 0x{stored:x}; skipping this update"
				),
			);
			return;
		}
	}
	if let Ok(mut engine) = engine.lock() {
		engine.set_source(context, db);
		engine.tick();
	}
}

/// The per-node mutation state: the vanilla route entries and steering
/// weights captured before the first mutation, plus the live closed flags.
/// Applying a state rewrites the live arrays from the originals with the
/// open entries first and parks the closed ones at the tail, then shrinks
/// the vector's end — so any open subset is a contiguous, legal vector
/// and restoring every flag rebuilds the exact vanilla layout.
struct NodeMutation {
	def: usize,
	routes_begin: usize,
	original_routes: Vec<u8>,
	weights_begin: Option<usize>,
	original_weights: Vec<i32>,
	closed: Vec<bool>,
}

/// The live slot order for vanilla edges with the given closed flags:
/// open vanilla slots first in vanilla order, closed slots parked at the
/// tail. Pure, so the ordering is unit-tested without engine memory.
fn live_order(closed: &[bool]) -> Vec<usize> {
	let open = (0..closed.len()).filter(|i| !closed[*i]);
	let shut = (0..closed.len()).filter(|i| closed[*i]);
	open.chain(shut).collect()
}

/// Reads and mutates the engine's trade structures. Every access happens
/// on the game thread inside the recalculation hook, so the database and
/// its nodes are stable for the whole call; reads still guard against
/// implausible addresses instead of trusting the layout blindly.
pub struct LiveBackend {
	db: usize,
	/// The global game context, kept for the context cross-check.
	context: usize,
	/// Per-node mutation state, captured before the first mutation.
	mutations: HashMap<u32, NodeMutation>,
	/// Vanilla outgoing targets per node index, frozen at the first
	/// observe and reported from cache afterwards.
	vanilla_outgoing: HashMap<u32, Vec<u32>>,
	/// Definition pointer and name per node index, refreshed on observe.
	names: Vec<(usize, String)>,
	/// Whether the weights-pairing diagnostic was already logged.
	pairing_logged: bool,
	/// Whether the node-field diagnostic was already logged.
	fields_logged: bool,
	/// Observes served, for the periodic state summary.
	observes: u32,
	/// Whether the route-entry dump was already logged.
	entry_logged: bool,
}

impl LiveBackend {
	pub fn new() -> Self {
		Self {
			db: 0,
			context: 0,
			mutations: HashMap::new(),
			vanilla_outgoing: HashMap::new(),
			names: Vec::new(),
			pairing_logged: false,
			fields_logged: false,
			observes: 0,
			entry_logged: false,
		}
	}

	fn read_qword(&self, addr: usize) -> Option<u64> {
		(addr > 0x1_0000 && addr & 7 == 0).then(|| unsafe { std::ptr::read(addr as *const u64) })
	}

	fn read_i32(&self, addr: usize) -> Option<i32> {
		(addr > 0x1_0000).then(|| unsafe { std::ptr::read_unaligned(addr as *const i32) })
	}

	fn write_qword(&self, addr: usize, value: u64) -> Option<()> {
		(addr > 0x1_0000 && addr & 7 == 0)
			.then(|| unsafe { std::ptr::write(addr as *mut u64, value) })
	}

	fn write_bytes(&self, addr: usize, bytes: &[u8]) -> Option<()> {
		(addr > 0x1_0000).then(|| unsafe {
			std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len())
		})
	}

	/// Capture a node's vanilla routes and weights before the first
	/// mutation so every later state is a rewrite of the originals.
	fn capture(&mut self, node: u32) -> Result<(), &'static str> {
		if self.mutations.contains_key(&node) {
			return Ok(());
		}
		let address = self
			.node_at(node as usize)
			.ok_or("implausible node pointer")?;
		let def = self
			.definition_of(address)
			.ok_or("implausible definition")?;
		let (begin, end) = self.routes(def).ok_or("implausible route vector")?;
		let count = (end - begin) / layout::route::SIZE;
		let mut original_routes = Vec::with_capacity(count * layout::route::SIZE);
		for offset in 0..count * layout::route::SIZE {
			let Some(byte) = self.read_i32(begin + offset) else {
				return Err("route bytes unreadable");
			};
			original_routes.push(byte as u8);
		}
		// The steering weights pair with the routes entry for entry; when
		// they do not, route reordering alone still holds and the weights
		// keep their live order (the pairing diagnostic covers the case).
		let weights_begin = self
			.read_qword(address + layout::node::WEIGHTS)
			.map(|p| p as usize);
		let weights_end = self
			.read_qword(address + layout::node::WEIGHTS + 8)
			.map(|p| p as usize);
		let paired = matches!((weights_begin, weights_end), (Some(a), Some(e)) if e >= a && (e - a) / 4 == count);
		let (weights_begin, original_weights) = if paired {
			let Some(begin) = weights_begin else {
				unreachable!("paired implies begin")
			};
			let mut values = Vec::with_capacity(count);
			for slot in 0..count {
				match self.read_i32(begin + slot * 4) {
					Some(value) => values.push(value),
					None => return Err("weight bytes unreadable"),
				}
			}
			(Some(begin), values)
		} else {
			(None, Vec::new())
		};
		self.mutations.insert(
			node,
			NodeMutation {
				def,
				routes_begin: begin,
				original_routes,
				weights_begin,
				original_weights,
				closed: vec![false; count],
			},
		);
		Ok(())
	}

	/// Rewrite a node's live route and weight arrays from the captured
	/// originals: open entries first, closed entries parked at the tail,
	/// and the vector end shrunk to the open prefix.
	fn apply(&self, node: u32) -> Result<(), &'static str> {
		let Some(state) = self.mutations.get(&node) else {
			return Ok(());
		};
		let stride = layout::route::SIZE;
		let order = live_order(&state.closed);
		for (live_slot, vanilla_slot) in order.iter().enumerate() {
			let source = vanilla_slot * stride;
			let target = state.routes_begin + live_slot * stride;
			let bytes = &state.original_routes[source..source + stride];
			self.write_bytes(target, bytes)
				.ok_or("route write refused")?;
			if let (Some(weights_begin), value) = (
				state.weights_begin,
				state.original_weights.get(*vanilla_slot).copied(),
			) {
				let Some(value) = value else { continue };
				let addr = weights_begin + live_slot * 4;
				(addr > 0x1_0000)
					.then(|| unsafe { std::ptr::write_unaligned(addr as *mut i32, value) })
					.ok_or("weight write refused")?;
			}
		}
		let open_count = state.closed.iter().filter(|c| !**c).count();
		let new_end = state.routes_begin + open_count * stride;
		self.write_qword(state.def + layout::def::ROUTES_END, new_end as u64)
			.ok_or("end write refused")?;
		Ok(())
	}

	fn node_count(&self) -> Option<usize> {
		let count = self.read_i32(self.db + layout::db::NODE_COUNT)? as usize;
		(20..=512).contains(&count).then_some(count)
	}

	fn nodes_base(&self) -> Option<usize> {
		let base = self.read_qword(self.db + layout::db::NODES)? as usize;
		(base > 0x1_0000 && base & 7 == 0).then_some(base)
	}

	/// The node pointer array dereference: element `index` is a heap
	/// `CTradeNode*`, loaded through one indirection.
	fn node_at(&self, index: usize) -> Option<usize> {
		let slot = self.nodes_base()? + index * 8;
		let node = self.read_qword(slot)? as usize;
		(node > 0x1_0000 && node & 7 == 0).then_some(node)
	}

	fn definition_of(&self, node: usize) -> Option<usize> {
		let def = self.read_qword(node + layout::node::DEFINITION)? as usize;
		(def > 0x1_0000 && def & 7 == 0).then_some(def)
	}

	/// MSVC SSO `std::string`: inline buffer unless capacity exceeds 0xF.
	fn read_name(&self, def: usize) -> Option<String> {
		let capacity = self.read_qword(def + layout::def::NAME + 0x18)?;
		let length = self.read_qword(def + layout::def::NAME + 0x10)? as usize;
		if length == 0 || length > 64 {
			return None;
		}
		let data = if capacity > 0xF {
			self.read_qword(def + layout::def::NAME)? as usize
		} else {
			def + layout::def::NAME
		};
		let mut bytes = Vec::with_capacity(length);
		for i in 0..length {
			let byte = self.read_i32(data + i)? as u8;
			if !(byte == b' ' || byte.is_ascii_graphic() || byte >= 0x80) {
				return None;
			}
			bytes.push(byte);
		}
		Some(String::from_utf8_lossy(&bytes).into_owned())
	}

	/// The route vector. An empty vector is legal and common: vanilla end
	/// nodes have no outgoing routes at all (`begin == end`, both null in
	/// a default-constructed MSVC vector).
	fn routes(&self, def: usize) -> Option<(usize, usize)> {
		let begin = self.read_qword(def + layout::def::ROUTES)? as usize;
		let end = self.read_qword(def + layout::def::ROUTES_END)? as usize;
		if begin == end {
			return Some((begin, end));
		}
		if begin == 0 || end < begin || !(end - begin).is_multiple_of(layout::route::SIZE) {
			return None;
		}
		Some((begin, end))
	}
}

impl Default for LiveBackend {
	fn default() -> Self {
		Self::new()
	}
}

impl EngineBackend for LiveBackend {
	fn describe(&self) -> &'static str {
		"live EU4 1.37.5 trade database"
	}

	fn set_source(&mut self, context: usize, db: usize) {
		self.context = context;
		self.db = db;
	}

	fn observe(&mut self) -> Result<Vec<NodeSnapshot>, &'static str> {
		if self.db == 0 {
			return Err("no database yet");
		}
		let count = self.node_count().ok_or("implausible node count")?;
		self.nodes_base().ok_or("implausible node array")?;
		for index in 0..count {
			let node = self.node_at(index).ok_or("implausible node pointer")?;
			self.definition_of(node)
				.ok_or("implausible definition pointer")?;
		}
		// Definition pointer to engine index: a route entry's target is
		// the destination node's *definition* pointer (the live dump
		// showed entry+0x30 inside the definition heap range).
		let mut index_by_def: HashMap<usize, u32> = HashMap::with_capacity(count);
		for index in 0..count {
			let node = self.node_at(index).expect("validated above");
			let def = self.definition_of(node).expect("validated above");
			index_by_def.insert(def, index as u32);
		}
		self.names.clear();
		// The vanilla outgoing edges, captured at the first observe (which
		// always precedes any mutation) and reported from the cache after
		// that: mutations reorder and shrink the live vector, so the live
		// order cannot serve as a stable vanilla-slot index for the policy
		// mirror. The live vector is still counted below as a diagnostic.
		if self.vanilla_outgoing.is_empty() {
			for index in 0..count {
				let node = self.node_at(index).expect("validated above");
				let def = self.definition_of(node).expect("validated above");
				let Some((begin, end)) = self.routes(def) else {
					return Err("implausible route vector");
				};
				let outgoing: Vec<u32> = (begin..end)
					.step_by(layout::route::SIZE)
					.filter_map(|slot| {
						let target = self.read_qword(slot + layout::route::TARGET)? as usize;
						index_by_def.get(&target).copied()
					})
					.collect();
				self.vanilla_outgoing.insert(index as u32, outgoing);
			}
		}
		let mut nodes = Vec::with_capacity(count);
		for index in 0..count {
			let node = self.node_at(index).expect("validated above");
			let def = self.definition_of(node).expect("validated above");
			self.names
				.push((def, self.read_name(def).unwrap_or_default()));
			let outgoing = self
				.vanilla_outgoing
				.get(&(index as u32))
				.cloned()
				.unwrap_or_default();
			let Some((begin, end)) = self.routes(def) else {
				return Err("implausible route vector");
			};
			let route_count = (end - begin) / layout::route::SIZE;
			// After mutations the live vector holds exactly the open
			// entries; a drift from the plugin's own closed flags would
			// mean an external writer touched the topology.
			if let Some(state) = self.mutations.get(&(index as u32)) {
				let expected = state.closed.iter().filter(|c| !**c).count();
				if route_count != expected {
					crate::logging::write(
						crate::logging::WARN,
						&format!(
							"node {index}: live routes {route_count} but {expected} expected open"
						),
					);
				}
			}
			// The steering weights are expected to pair with the routes
			// entry for entry, but that pairing is a spike hypothesis, not
			// a pinned fact: log the first mismatch as a diagnostic and
			// keep observing, so a live session tells us the real shape.
			let weights = self
				.read_qword(node + layout::node::WEIGHTS)
				.ok_or("implausible weights vector")? as usize;
			let weights_end = self
				.read_qword(node + layout::node::WEIGHTS + 8)
				.ok_or("implausible weights vector")? as usize;
			if !self.pairing_logged
				&& (weights_end < weights || (weights_end - weights) / 4 != route_count)
			{
				self.pairing_logged = true;
				crate::logging::write(
					crate::logging::DEBUG,
					&format!(
						"weights pairing differs on node {index}: {} weight slots vs {route_count} routes",
						weights_end.saturating_sub(weights) / 4
					),
				);
			}
			// The index fields are spike hypotheses, so the first observe
			// logs them as diagnostics instead of trusting them; the node
			// names are the real proof the definitions were reversed right.
			if !self.fields_logged {
				self.fields_logged = true;
				for probe in 0..count.min(3) {
					let node = self.node_at(probe).expect("validated above");
					let def = self.definition_of(node).expect("validated above");
					let routes_begin = self.read_qword(def + layout::def::ROUTES);
					let routes_end = self.read_qword(def + layout::def::ROUTES_END);
					let routes_cap = self.read_qword(def + layout::def::ROUTES_END + 8);
					let name_bytes = (0..16)
						.filter_map(|i| self.read_i32(def + layout::def::NAME + i))
						.map(|v| v as u8)
						.take_while(|b| *b != 0)
						.collect::<Vec<u8>>();
					let name = String::from_utf8_lossy(&name_bytes);
					crate::logging::write(
						crate::logging::DEBUG,
						&format!(
							"node {probe}: ptr 0x{node:x}, def 0x{def:x}, routes {routes_begin:?}..{routes_end:?} cap {routes_cap:?}, inline name {name:?}"
						),
					);
				}
			}
			// Engine-cached aggregates, all in thousandths: goods produced
			// as the production-side wealth input, total trade power, and
			// the current value. A per-node tax aggregate does not exist
			// in the trade node object; tax stays zero until the province
			// walk lands (see the README's known limits).
			nodes.push(NodeSnapshot {
				id: index as u32,
				tax: 0.0,
				production: self.read_i32(node + layout::node::GOODS).unwrap_or(0) as f64,
				trade_power: self.read_i32(node + layout::node::POWER).unwrap_or(0) as f64,
				current_value: self.read_i32(node + layout::node::VALUE).unwrap_or(0) as f64,
				outgoing,
			});
		}
		// M2 semantics survey, once, after every name is collected:
		// per-node engine-cached candidates across a dozen nodes, to
		// tell trade power from tax-like aggregates by correlation.
		if !self.entry_logged {
			self.entry_logged = true;
			let mut lines = Vec::new();
			for probe in 0..count.min(14) {
				let Some(probe_node) = self.node_at(probe) else {
					continue;
				};
				let value = self.read_i32(probe_node + layout::node::VALUE);
				let c8 = self.read_i32(probe_node + 0xc8);
				let d0 = self.read_i32(probe_node + 0xd0);
				let d8 = self.read_i32(probe_node + 0xd8);
				let e4 = self.read_i32(probe_node + 0xe4);
				lines.push(format!(
					"{}: value {value:?} c8 {c8:?} d0 {d0:?} d8 {d8:?} e4 {e4:?}",
					self.names[probe].1
				));
			}
			crate::logging::write(
				crate::logging::DEBUG,
				&format!("node field survey:\n  {}", lines.join("\n  ")),
			);
		}
		// A periodic summary shows whether a late observe sees a database
		// the first one could not (the update can fire mid-load).
		self.observes += 1;
		if self.observes == 1 || self.observes.is_multiple_of(100) {
			let edges: usize = nodes.iter().map(|n| n.outgoing.len()).sum();
			let sample: Vec<&str> = self
				.names
				.iter()
				.map(|(_, name)| name.as_str())
				.filter(|name| !name.is_empty())
				.take(3)
				.collect();
			crate::logging::write(
				crate::logging::DEBUG,
				&format!(
					"observe #{}: {} nodes, {} edges, names [{}]",
					self.observes,
					nodes.len(),
					edges,
					sample.join(", ")
				),
			);
		}
		Ok(nodes)
	}

	fn node_index(&self, name: &str) -> Option<u32> {
		self.names
			.iter()
			.position(|(_, node)| node.eq_ignore_ascii_case(name))
			.map(|i| i as u32)
	}

	fn set_edge_closed(
		&mut self,
		node: u32,
		edge: usize,
		closed: bool,
	) -> Result<(), &'static str> {
		self.capture(node)?;
		let Some(state) = self.mutations.get_mut(&node) else {
			return Ok(());
		};
		if edge >= state.closed.len() {
			return Err("edge slot beyond the vanilla set");
		}
		state.closed[edge] = closed;
		self.apply(node)
	}
}

#[cfg(test)]
mod tests {
	use super::live_order;

	#[test]
	fn open_entries_stay_first_in_vanilla_order() {
		assert_eq!(live_order(&[false, false]), vec![0, 1]);
		// Closing the middle of three parks vanilla slot 1 at the tail.
		assert_eq!(live_order(&[false, true, false]), vec![0, 2, 1]);
		// Closed slots keep their vanilla order at the tail.
		assert_eq!(live_order(&[true, true]), vec![0, 1]);
		assert_eq!(live_order(&[true, false, true]), vec![1, 0, 2]);
	}

	#[test]
	fn an_empty_node_has_an_empty_order() {
		assert!(live_order(&[]).is_empty());
	}
}
