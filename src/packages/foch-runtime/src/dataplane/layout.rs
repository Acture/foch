//! The pinned EU4 1.37.5 Steam x64 layout the data plane walks.
//!
//! Every constant was reversed against the build with sha256
//! `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`.
//! Most constants are live-verified from automated sessions; any
//! statically identified fields that have not yet passed a live Foch
//! session probe are explicitly marked in their doc comments.

/// Exact executable identity used by every consumer of this pinned layout.
pub const EXPECTED_EXE_SHA256: &str =
	"c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e";

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
/// `std::vector<CCountry*>` begin inside the game context; the
/// distribution loop walks it (`local_170 = DAT_14233fe78`). The
/// plugin reads the count only.
#[allow(dead_code)]
pub const COUNTRIES_BEGIN: usize = 0x1d08;
/// Element count of the country vector, an int in the context.
pub const COUNTRIES_COUNT: usize = 0x1d14;

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
	/// MSVC `std::vector<int32>` of 1-based member province IDs belonging
	/// to this trade node (`members = { ... }`, token `0x2aa9`). Statically
	/// identified for EU4 1.37.5 via `CTradeNodeDefinition` parser/writer
	/// (`[rcx+0x50]` / `[rsi+0x50]`, stride 4); pending live Foch session
	/// verification.
	pub const MEMBERS: usize = 0x50;
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

/// The country registry behind `image+0x233d8d0`.
pub mod country_manager {
	/// `CCountry**` indexed by country index: the trade distribution
	/// loop resolves every participant through this array.
	pub const INDEX_ARRAY: usize = 0x118;
}

/// `CCountry`, reached through the manager's index array.
pub mod country {
	/// Packed identity: the low bytes hold the tag (printed as `%s`
	/// by the ChangeTreasury console log), and bits 32..48 of the
	/// same qword hold the country index. Live-verified.
	pub const TAG: usize = 0x20;
	/// Monthly income bank (`i32`, thousandths) used by tribute and
	/// economy calculations. Statically identified for EU4 1.37.5;
	/// pending live Foch game-session verification.
	pub const MONTHLY_INCOME_BANK: usize = 0x68;
	/// The treasury (int32), added to by `ChangeTreasury` at
	/// `country.cpp:0x5882` and clamped to ±1,000,000,000. Live-verified.
	pub const TREASURY: usize = 0xbb0;
	/// `ChangeTreasury` clamps the treasury to this magnitude.
	pub const TREASURY_CLAMP: i64 = 1_000_000_000;
	/// Packed overlord country identity (same 8-byte layout as [`TAG`],
	/// with byte 7 non-zero when this country is a subject). Statically
	/// identified for EU4 1.37.5; shared with `war::eu4`.
	pub const OVERLORD: usize = 0x18f0;
	/// MSVC `std::vector` header of this country's subjects. Statically
	/// identified for EU4 1.37.5; pending live Foch game-session
	/// verification.
	pub const SUBJECTS_VEC: usize = 0x24b0;
	/// Tributary tribute-type discriminator (`u8`). Statically identified
	/// for EU4 1.37.5; pending live Foch game-session verification.
	pub const TRIBUTE_TYPE: usize = 0x28a6;
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

/// Campaign identity and calendar clock embedded in the game context.
///
/// Statically identified for EU4 1.37.5 via save/load paths; pending live
/// Foch game-session verification.
pub mod campaign {
	/// 16-byte campaign GUID stored in the game context.
	pub const GUID: usize = 0x24d4;
	/// Current game clock in hours (`i32`) stored in the game context.
	pub const CURRENT_HOURS: usize = 0x1dd0;
	/// Engine calendar epoch in hours (`1444`-compatible day-zero origin).
	pub const CALENDAR_EPOCH_HOURS: i64 = 43_800_000;
}

/// Active-war registry in the game context and `CWar` object layout.
///
/// Statically identified for EU4 1.37.5; pending live Foch game-session
/// verification.
pub mod war {
	/// Pointer to the active `CWar*` array inside the game context.
	pub const ACTIVE_WARS: usize = 0x1e30;
	/// Packed `(capacity: i32, count: i32)` qword for the active-war array.
	pub const ARRAY_COUNTS: usize = 0x1e38;
	/// Upper bound on active wars accepted before refusing a corrupt count.
	pub const MAX_WARS: usize = 4096;
	/// Declaration hour inside `CWar`. `CWar::CWar` (`FUN_140d859b0`) stores
	/// [`UNSET_HOUR`] here; declaration (`FUN_140501290`) copies
	/// [`campaign::CURRENT_HOURS`] over it.
	pub const STARTED: usize = 0x24;
	/// Sentinel value stored at [`STARTED`] before a war is declared.
	pub const UNSET_HOUR: i32 = 0x029c_77f8;
	/// No `CWar` serial field was identified in EU4 1.37.5 (`+0x138` stays
	/// zero); serial remains `0` until a real field is proven.
	pub const UNIDENTIFIED_SERIAL: u32 = 0;
	/// MSVC `std::vector` of packed attacker identities inside `CWar`.
	pub const ATTACKERS: usize = 0x48;
	/// MSVC `std::vector` of packed defender identities inside `CWar`.
	pub const DEFENDERS: usize = 0x78;
	/// Packed country identity of the original attacker inside `CWar`.
	pub const ORIGINAL_ATTACKER: usize = 0x150;
	/// Packed country identity of the original defender inside `CWar`.
	pub const ORIGINAL_DEFENDER: usize = 0x158;
}

/// Per-country diplomacy relation array (`CCountry + COUNTRY_RELATIONS`).
///
/// Statically identified for EU4 1.37.5 via the `truce_with` trigger;
/// pending live Foch game-session verification.
pub mod diplomacy {
	/// Pointer to the per-target-country relation array inside `CCountry`.
	pub const COUNTRY_RELATIONS: usize = 0x1510;
	/// Byte stride of one target country's relation record.
	pub const RELATION_SIZE: usize = 0x48;
	/// Flag byte inside a relation record whose bit `0` marks an active truce.
	pub const TRUCE_FLAGS: usize = 0x45;
}
