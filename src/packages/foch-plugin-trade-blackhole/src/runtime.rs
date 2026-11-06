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

/// Reads and mutates the engine's trade structures. Every access happens
/// on the game thread inside the recalculation hook, so the database and
/// its nodes are stable for the whole call; reads still guard against
/// implausible addresses instead of trusting the layout blindly.
pub struct LiveBackend {
	db: usize,
	/// The global game context, kept for the date-anchor scan.
	context: usize,
	/// Original route-vector end per definition pointer, captured before
	/// the first closure so reopening restores the vanilla topology.
	original_ends: HashMap<usize, usize>,
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
			original_ends: HashMap::new(),
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
		let mut nodes = Vec::with_capacity(count);
		for index in 0..count {
			let node = self.node_at(index).expect("validated above");
			let def = self.definition_of(node).expect("validated above");
			self.names
				.push((def, self.read_name(def).unwrap_or_default()));
			// Route targets the mirror cannot resolve to a known node are
			// dropped from the mirror; the engine keeps its own entry.
			let (begin, end) = self.routes(def).ok_or("implausible route vector")?;
			let route_count = (end - begin) / layout::route::SIZE;
			let outgoing: Vec<u32> = (begin..end)
				.step_by(layout::route::SIZE)
				.filter_map(|slot| {
					let target = self.read_qword(slot + layout::route::TARGET)? as usize;
					index_by_def.get(&target).copied()
				})
				.collect();
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
			if !self.entry_logged
				&& let Some((begin, end)) = self.routes(def)
				&& end > begin
			{
				self.entry_logged = true;
				let words: Vec<String> = (0..15)
					.filter_map(|i| self.read_qword(begin + i * 8))
					.map(|w| format!("{w:x}"))
					.collect();
				crate::logging::write(
					crate::logging::DEBUG,
					&format!("route entry qwords: {}", words.join(" ")),
				);
			}
			nodes.push(NodeSnapshot {
				id: index as u32,
				tax: 0.0,
				production: 0.0,
				trade_power: 0.0,
				current_value: self.read_i32(node + layout::node::VALUE).unwrap_or(0) as f64,
				outgoing,
			});
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
		_edge: usize,
		closed: bool,
	) -> Result<(), &'static str> {
		let address = self
			.node_at(node as usize)
			.ok_or("implausible node pointer")?;
		let def = self
			.definition_of(address)
			.ok_or("implausible definition")?;
		let (begin, end) = self.routes(def).ok_or("implausible route vector")?;
		let open = end > begin;
		if closed != open {
			return Ok(());
		}
		if closed {
			self.original_ends.insert(def, end);
			self.write_qword(def + layout::def::ROUTES_END, begin as u64)
				.ok_or("end write refused")?;
		} else {
			let original = self.original_ends.get(&def).copied().unwrap_or(begin);
			self.write_qword(def + layout::def::ROUTES_END, original as u64)
				.ok_or("end write refused")?;
		}
		Ok(())
	}
}
