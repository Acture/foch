//! Pinned game build and the pattern signature the plugin resolves through
//! the host's `find_pattern`.
//!
//! The RE spike (see the package README) reversed EU4 1.37.5 Steam x64
//! (`eu4.exe` sha256 below) with Ghidra 12.1.3. The exe digest is checked
//! first and any other build is refused before a pattern scan runs.

/// The Steam Windows x64 `eu4.exe` build this plugin was reversed against:
/// EU4 1.37.5, sha256 `c9ec55a4...`.
pub const EXPECTED_EXE_SHA256: &str =
	"c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e";

/// The engine layout the spike verified for that build. Every value is a
/// byte offset from the base of the structure the module is named after.
pub mod layout {
	/// Hook target: the economy update that ends by recalculating every
	/// trade node (`... CTradeNodeDataBase::UpdateAll(ctx + 0x2198)`), at
	/// image+0x75d690. Its prologue tiles exactly the 14 whole bytes the
	/// host's hook gate steals.
	pub const TICK_RVA: usize = 0x75d690;
	/// The global game-context pointer variable, `image+0x233fe78`: the
	/// value stored there is the hook's first argument and the base the
	/// trade database is embedded in.
	pub const CONTEXT_GLOBAL_RVA: usize = 0x233fe78;
	/// Offset of `CTradeNodeDataBase` inside the game context.
	pub const DB_IN_CONTEXT: usize = 0x2198;

	/// `CTradeNodeDataBase`, the recalculation's `this` argument.
	pub mod db {
		/// Pointer to the node array: a pointer vector (stride 8), each
		/// element a heap `CTradeNode*`, confirmed by the per-node update
		/// loop (`MOV RCX,[RBX]` before its call) at image+0xb4bef8.
		pub const NODES: usize = 0x30;
		/// Element count of the node pointer array.
		pub const NODE_COUNT: usize = 0x3c;
	}

	/// Runtime `CTradeNode`, reached through the database's pointer
	/// array, so no array stride applies.
	pub mod node {
		/// `std::vector<int32>`: per-outgoing-edge steering weights, in
		/// thousandths, index-aligned with the definition's route vector.
		pub const WEIGHTS: usize = 0x88;
		/// `CTradeNodeDefinition*`; the definition carries the route
		/// vector and the node name.
		pub const DEFINITION: usize = 0xa8;
		/// Current total trade value of the node (int32).
		pub const VALUE: usize = 0xb0;
	}

	/// `CTradeNodeDefinition`, reached through `node::DEFINITION`.
	pub mod def {
		/// MSVC SSO `std::string` node name, e.g. `lubeck_trade_node`:
		/// 16-byte buffer-or-pointer, size at +0x10, capacity at +0x18.
		pub const NAME: usize = 0x30;
		/// `std::vector<CTradeRoute>`: begin pointer; end pointer at
		/// [`ROUTES_END`]. Element stride 0x78 ([`route::SIZE`]).
		pub const ROUTES: usize = 0x98;
		/// End pointer of the route vector. Closing a node writes `begin`
		/// here; reopening restores the cached original.
		pub const ROUTES_END: usize = 0xa0;
	}

	/// `CTradeRoute`, stride 0x78 inside the definition's route vector.
	pub mod route {
		pub const SIZE: usize = 0x78;
		/// Target `CTradeNodeDefinition*` of the destination node.
		pub const TARGET: usize = 0x30;
	}
}

/// `find_pattern` signatures for one pinned build. A signature matches
/// exactly one address in the executable sections or resolution fails.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sigs {
	/// The economy update whose tail recalculates every trade node. Its
	/// first argument is the global game context, which both identifies
	/// the hook (cross-checked against [`layout::CONTEXT_GLOBAL_RVA`]) and
	/// embeds the trade database at [`layout::DB_IN_CONTEXT`].
	pub tick: &'static str,
}

impl Sigs {
	/// The verified signatures for [`EXPECTED_EXE_SHA256`], or `None` for
	/// any other build.
	pub fn for_build(exe_sha256: &str) -> Option<Self> {
		(exe_sha256 == EXPECTED_EXE_SHA256).then(Self::pinned)
	}

	/// The signature table for the reversed build.
	pub const fn pinned() -> Self {
		Self {
			tick: "88 54 24 10 55 53 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 28 F1 FF FF 48 81 EC D8 0F 00 00 4C 8B F9 BA FF FF FF FF",
		}
	}

	pub fn is_pending(&self) -> bool {
		self.tick.is_empty()
	}
}
