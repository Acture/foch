//! Directed pact policy. Engine integration must supply an authoritative war bridge.

pub mod command;
mod native;
pub mod pact;
pub mod receipt;
pub use foch_runtime::war;
pub use native::foch_plugin_get_api;

pub const PACT_PLUGIN_ID: &str = "dev.foch.directed-pacts";
pub const ROSTER_PROBE_PLUGIN_ID: &str = "dev.foch.directed-pacts.roster-probe";
