//! Read-only EU4 territory decide: GNN suggestions over live state.
//!
//! Pipeline: the static province graph loads from the extracted
//! game-file asset ([`graph`]); the dynamic per-province fields come
//! through the gated readers of `foch-eu4-state::view` (VerifiedGame:
//! host-computed digest -> shared build gate -> Option reads); the
//! assembled 29-column frame ([`frame`]) feeds the pure-Rust forward
//! of `foch-eu4-gnn`, and [`decide`] turns scores into a logged
//! suggestion. Nothing in this crate writes game memory or takes any
//! action; army composition/morale and terrain columns are documented
//! as pending RE in [`frame`].

pub mod decide;
pub mod frame;
pub mod graph;
mod native;

pub use native::foch_plugin_get_api;

pub const READ_DECIDE_PLUGIN_ID: &str = "dev.foch.read-decide";
