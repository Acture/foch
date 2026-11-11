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
	/// The global country-manager pointer variable, `image+0x233d8d0`,
	/// dereferenced by the trade distribution loop (`*(manager+0x118)`
	/// indexed by country index) inside the per-node update.
	pub const COUNTRY_MANAGER_GLOBAL_RVA: usize = 0x233d8d0;
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
		/// Current total trade value of the node (int32, thousandths).
		pub const VALUE: usize = 0xb0;
		/// The amount the engine transferred out this tick (int32,
		/// thousandths), read by PushValue and subtracted from VALUE
		/// when any steering weight is active. The engine's own
		/// retention rate is `VALUE / (VALUE + TRANSFERRED)`.
		pub const TRANSFERRED: usize = 0xbc;
		/// Total trade power of the node (int32, thousandths); a second
		/// near-identical total sits at +0xd0 and the provincial-only
		/// share at +0xd8.
		pub const POWER: usize = 0xc8;
		/// Local trade value: the production-side injection (goods
		/// produced × their prices), from the trade UI's `local_value`
		/// read. This is what the engine itself feeds into trade
		/// propagation; it already reflects price changes.
		pub const LOCAL_VALUE: usize = 0xb4;
		/// Per-country entry table: `entry = *(node+0x18) + country_index * 0xc0`.
		/// The distribution loop reads each participant's entry fields
		/// (power, merchant-mode flags) from this table.
		pub const ENTRY_TABLE: usize = 0x18;
		/// Stride of one country's entry in the table.
		pub const ENTRY_SIZE: usize = 0xc0;
	}

	/// One country's presence in a trade node, from the distribution loop.
	pub mod entry {
		/// This country's trade power contribution to the node (int32,
		/// thousandths): the value the loop accumulates into the node's
		/// per-country power subtotal.
		pub const POWER: usize = 0x28;
		/// Merchant-mode flags read before distributing: collecting at
		/// +0xac, transferring at +0xad, a third mode at +0xae. The fee
		/// pays every power holder regardless of mode, so the plugin
		/// keeps them documented here without reading them yet.
		#[allow(dead_code)]
		pub const MODE_COLLECT: usize = 0xac;
		#[allow(dead_code)]
		pub const MODE_TRANSFER: usize = 0xad;
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

	/// Province-side access (tax aggregation input).
	pub mod province {
		/// Offset of the `CProvince` array base pointer inside the game
		/// context: contiguous elements indexed by province id, exposed
		/// by the `Set base tax to` console command.
		pub const CPROVINCES: usize = 0x1ca8;
		/// `CProvince` element stride.
		pub const CPROVINCE_SIZE: usize = 0x2e10;
		/// Base tax in thousandths, from `CProvince::SetBaseTax`.
		pub const TAX: usize = 0x3e4;
		/// Base manpower in thousandths (potential consumption), at
		/// `+0x3e0` per the live dump correlation.
		pub const MANPOWER: usize = 0x3e0;
	}

	/// The country manager behind `image+0x233d8d0`.
	pub mod country_manager {
		/// `CCountry**` indexed by country index: the trade distribution
		/// loop resolves every participant through this array.
		pub const INDEX_ARRAY: usize = 0x118;
		/// `std::vector<CCountry*>` begin; the update iterates it to walk
		/// every country. The plugin reads the count only.
		#[allow(dead_code)]
		pub const LIST_BEGIN: usize = 0x1d08;
		/// Element count of the country list.
		pub const LIST_COUNT: usize = 0x1d14;
	}

	/// `CCountry`, reached through the manager's index array.
	pub mod country {
		/// Packed identity: the low bytes hold the tag (printed as `%s`
		/// by the ChangeTreasury console log), and bits 32..48 of the
		/// same qword hold the country index.
		pub const TAG: usize = 0x20;
		/// The treasury (int32), added to by `ChangeTreasury` at
		/// `country.cpp:0x5882` and clamped to ±1,000,000,000.
		pub const TREASURY: usize = 0xbb0;
		/// `ChangeTreasury` clamps the treasury to this magnitude.
		pub const TREASURY_CLAMP: i64 = 1_000_000_000;
	}

	/// Player-facing UI refresh, mirrored from ChangeTreasury's tail.
	pub mod ui {
		/// The UI controller object inside the game context; its +0x388
		/// flag word carries the treasury-dirty bit.
		pub const CONTROLLER: usize = 0x1e00;
		/// The flag word holding per-widget dirty bits.
		pub const FLAGS: usize = 0x388;
		/// The bit ChangeTreasury sets when the player's treasury moved.
		pub const TREASURY_DIRTY: i32 = 0x40;
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
