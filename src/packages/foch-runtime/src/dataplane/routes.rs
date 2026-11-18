//! The reversible route-table mutation primitive.
//!
//! Closing a node rewrites its live route and steering arrays from the
//! captured vanilla originals — open entries first in vanilla order,
//! closed entries parked at the tail — then shrinks the route vector's
//! `end` to the open prefix. Any open subset is a contiguous, legal
//! vector (exactly what vanilla end nodes look like), all writes stay
//! inside the original capacity, and restoring every flag rebuilds the
//! byte-exact vanilla layout.

use super::Memory;
use super::layout::{def, node, route};

/// The per-node mutation state: the vanilla route entries and steering
/// weights captured before the first mutation, plus the live closed
/// flags.
pub struct RouteMutation {
	def: usize,
	routes_begin: usize,
	original_routes: Vec<u8>,
	weights_begin: Option<usize>,
	original_weights: Vec<i32>,
	closed: Vec<bool>,
}

/// The live slot order for vanilla edges with the given closed flags:
/// open vanilla slots first in vanilla order, closed slots parked at
/// the tail. Pure, so the ordering is unit-tested without game memory.
pub fn live_order(closed: &[bool]) -> Vec<usize> {
	let open = (0..closed.len()).filter(|i| !closed[*i]);
	let shut = (0..closed.len()).filter(|i| closed[*i]);
	open.chain(shut).collect()
}

impl RouteMutation {
	/// Capture a node's vanilla routes and weights before any mutation.
	pub fn capture(
		memory: &Memory,
		node: usize,
		definition: usize,
		routes: (usize, usize),
	) -> Option<RouteMutation> {
		let (begin, end) = routes;
		let count = end.checked_sub(begin)? / route::SIZE;
		let mut original_routes = Vec::with_capacity(count * route::SIZE);
		for offset in 0..count * route::SIZE {
			original_routes.push(memory.read_u8(begin + offset)?);
		}
		// The steering weights pair with the routes entry for entry;
		// when they do not, route reordering alone still holds and the
		// weights keep their live order.
		let weights_begin = memory.read_ptr(node + node::WEIGHTS);
		let weights_end = memory.read_ptr(node + node::WEIGHTS + 8);
		let paired = matches!((weights_begin, weights_end), (Some(a), Some(e)) if e >= a && (e - a) / 4 == count);
		let (weights_begin, original_weights) =
			if let Some(begin) = paired.then_some(weights_begin).flatten() {
				let mut values = Vec::with_capacity(count);
				for slot in 0..count {
					values.push(memory.read_i32(begin + slot * 4)?);
				}
				(Some(begin), values)
			} else {
				(None, Vec::new())
			};
		Some(RouteMutation {
			def: definition,
			routes_begin: begin,
			original_routes,
			weights_begin,
			original_weights,
			closed: vec![false; count],
		})
	}

	/// Whether the vanilla edge slot is currently closed.
	pub fn is_closed(&self, edge: usize) -> bool {
		self.closed.get(edge).copied().unwrap_or(false)
	}

	/// Set one vanilla edge slot and rewrite the live arrays.
	///
	/// # Safety
	///
	/// The rewrite writes the route and steering arrays whose addresses
	/// [`capture`](Self::capture) was given — capture is a read, so the
	/// provenance contract lands here. The caller must guarantee those
	/// arrays are still alive and engine-owned, that the call runs on
	/// the engine thread where the route tables are stable, and that no
	/// Rust reference or validity invariant aliases them.
	pub unsafe fn set_closed(&mut self, memory: &Memory, edge: usize, closed: bool) -> Option<()> {
		let flag = self.closed.get_mut(edge)?;
		*flag = closed;
		self.apply(memory)
	}

	/// Called only from the unsafe [`set_closed`](Self::set_closed)
	/// boundary, which carries the liveness/thread/no-alias contract.
	fn apply(&self, memory: &Memory) -> Option<()> {
		// SAFETY: every address below was captured from the engine's own
		// route and steering vectors (never caller-supplied), the writes
		// stay inside the captured original capacity, and the caller
		// runs on the game thread where the route tables are stable.
		unsafe {
			let stride = route::SIZE;
			for (live_slot, vanilla_slot) in live_order(&self.closed).iter().enumerate() {
				let source = vanilla_slot * stride;
				let target = self.routes_begin + live_slot * stride;
				memory.write_bytes(target, &self.original_routes[source..source + stride])?;
				if let (Some(weights_begin), value) = (
					self.weights_begin,
					self.original_weights.get(*vanilla_slot).copied(),
				) {
					let Some(value) = value else { continue };
					memory.write_i32(weights_begin + live_slot * 4, value)?;
				}
			}
			// The paired steering-weight vector's end is left untouched:
			// the engine walks the weights by the route count, and the
			// parked tail keeps observe-side permutation recovery simple.
			let open_count = self.closed.iter().filter(|c| !**c).count();
			let new_end = self.routes_begin + open_count * stride;
			memory.write_qword(self.def + def::ROUTES_END, new_end as u64)
		}
	}
}
