//! Plugin configuration, parsed from the host's validated `config_json`.
//!
//! The field names and types mirror the `[config.*]` tables of the plugin's
//! `foch-plugin.toml` one for one. The host validates the manifest schema;
//! parsing here still merges defaults so a missing key never aborts the
//! plugin, and an unexpected shape leaves it in observe-only mode.

use crate::policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	/// Read and log the trade graph every month; never mutate.
	Observe,
	/// Toggle one configured node's outgoing edges every month, to prove the
	/// mutation primitive end to end.
	ForceToggle,
	/// Run the wealth-gradient policy.
	Policy,
}

impl Mode {
	fn parse(text: &str) -> Option<Self> {
		match text {
			"observe" => Some(Self::Observe),
			"force_toggle" => Some(Self::ForceToggle),
			"policy" => Some(Self::Policy),
			_ => None,
		}
	}
}

/// How the per-node sink signal is derived from the members' tax:
/// the per-province mean (wealth intensity) or the raw sum (mass).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkBasis {
	Mean,
	Sum,
}

impl SinkBasis {
	fn parse(text: &str) -> Option<Self> {
		match text {
			"mean" => Some(Self::Mean),
			"sum" => Some(Self::Sum),
			_ => None,
		}
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
	/// Master switch; `false` leaves the plugin an inert observer.
	pub enabled: bool,
	pub mode: Mode,
	/// Engine node name for `force_toggle`, e.g. `Great Lakes`.
	pub force_node: String,
	/// Write the monthly observer dump to the plugin log directory.
	pub observer_log: bool,
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
	/// Consecutive months of sustained evidence before an edge flips.
	pub evidence_months: u32,
	/// Sink signal basis: per-province mean tax (intensity) or raw sum.
	pub sink_basis: SinkBasis,
	/// Edge flips the whole graph may apply per month.
	pub max_flips_total: usize,
	/// Pay the role incomes: transit fees on forwarded volume and source
	/// bounties on exported local injection, shared among the countries
	/// holding trade power in each node.
	pub transit_fee: bool,
	/// The transit pool is this percent of the volume the node forwards
	/// for others (the trade-power role's income).
	pub transit_fee_pct: i64,
	/// The source bounty is this percent of the local injection that
	/// leaves the node (the production role's income).
	pub source_fee_pct: i64,
}

impl Default for Config {
	fn default() -> Self {
		let params = policy::Params::default();
		Self {
			enabled: true,
			mode: Mode::Observe,
			force_node: "Great Lakes".to_string(),
			observer_log: true,
			sink_basis: SinkBasis::Mean,
			relay_ref: params.relay_ref,
			flow_epsilon_pct: params.flow_epsilon_pct,
			reopen_pct: params.reopen_pct,
			evidence_months: params.evidence_months,
			max_flips_total: params.max_flips_total,
			transit_fee: true,
			transit_fee_pct: DEFAULT_TRANSIT_FEE_PCT,
			source_fee_pct: DEFAULT_SOURCE_FEE_PCT,
		}
	}
}

/// The share of a node's monthly net through-flow that becomes the
/// transit fee pool: a fifth by default, so passing trade pays the
/// middlemen without draining the flow it rewards.
pub const DEFAULT_TRANSIT_FEE_PCT: i64 = 20;
pub const DEFAULT_SOURCE_FEE_PCT: i64 = 20;

impl Config {
	/// Parse the host's JSON object over the defaults. A shape the manifest
	/// schema should already have rejected downgrades to observe-only.
	pub fn from_json(text: &[u8]) -> Self {
		let mut config = Self::default();
		let Ok(value) = serde_json::from_slice::<serde_json::Value>(text) else {
			return Self::observe_only();
		};
		let Some(object) = value.as_object() else {
			return Self::observe_only();
		};
		if let Some(enabled) = object.get("enabled").and_then(|v| v.as_bool()) {
			config.enabled = enabled;
		}
		if let Some(mode) = object
			.get("mode")
			.and_then(|v| v.as_str())
			.and_then(Mode::parse)
		{
			config.mode = mode;
		} else if object.contains_key("mode") {
			// A mode the schema does not name is a hand-edited mistake;
			// fail safe into observe-only rather than guessing.
			return Self::observe_only();
		}
		if let Some(node) = object
			.get("force_node")
			.and_then(|v| v.as_str())
			.map(str::trim)
			.filter(|node| !node.is_empty() && node.chars().all(|c| !c.is_control()))
		{
			// Display names carry spaces and non-ASCII ("Great Lakes",
			// "Lübeck"), so the guard only rejects emptiness and control
			// characters; the lookup itself compares case-insensitively.
			config.force_node = node.to_string();
		}
		if let Some(log) = object.get("observer_log").and_then(|v| v.as_bool()) {
			config.observer_log = log;
		}
		if let Some(basis) = object
			.get("sink_basis")
			.and_then(|v| v.as_str())
			.and_then(SinkBasis::parse)
		{
			config.sink_basis = basis;
		} else if object.contains_key("sink_basis") {
			return Self::observe_only();
		}
		for (key, target) in [
			("relay_ref", &mut config.relay_ref),
			("flow_epsilon_pct", &mut config.flow_epsilon_pct),
			("reopen_pct", &mut config.reopen_pct),
		] {
			if let Some(number) = object.get(key).and_then(|v| v.as_i64()) {
				*target = number;
			}
		}
		if let Some(total) = object.get("max_flips_total").and_then(|v| v.as_u64()) {
			config.max_flips_total = total.min(usize::MAX as u64) as usize;
		}
		if let Some(fee) = object.get("transit_fee").and_then(|v| v.as_bool()) {
			config.transit_fee = fee;
		}
		if let Some(pct) = object.get("transit_fee_pct").and_then(|v| v.as_i64()) {
			config.transit_fee_pct = pct;
		}
		if let Some(pct) = object.get("source_fee_pct").and_then(|v| v.as_i64()) {
			config.source_fee_pct = pct;
		}
		if !(0..=100).contains(&config.transit_fee_pct)
			|| !(0..=100).contains(&config.source_fee_pct)
		{
			return Self::observe_only();
		}
		if policy::Params::from(config.clone()).validate().is_err() {
			return Self::observe_only();
		}
		config
	}

	fn observe_only() -> Self {
		Self {
			enabled: false,
			mode: Mode::Observe,
			..Self::default()
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn empty_input_keeps_defaults() {
		assert_eq!(Config::from_json(b"{}"), Config::default());
	}

	#[test]
	fn known_keys_override_defaults() {
		let config = Config::from_json(
			br#"{"mode":"policy","relay_ref":5000,"force_node":"beijing_trade_node"}"#,
		);
		assert_eq!(config.mode, Mode::Policy);
		assert_eq!(config.relay_ref, 5000);
		assert_eq!(config.force_node, "beijing_trade_node");
	}

	#[test]
	fn garbage_input_downgrades_to_observe_only() {
		let config = Config::from_json(br#"{"mode":"nonsense"}"#);
		assert!(!config.enabled);
		assert_eq!(config.mode, Mode::Observe);
		assert!(!Config::from_json(b"not json").enabled);
	}

	#[test]
	fn crossed_thresholds_downgrade_to_observe_only() {
		let config = Config::from_json(br#"{"reopen_pct":0}"#);
		assert!(!config.enabled);
	}

	#[test]
	fn a_node_name_accepts_spaces_and_unicode_but_not_controls() {
		// Display names carry spaces and non-ASCII ("Great Lakes",
		// "Lübeck"); only emptiness and control characters are rejected.
		let config = Config::from_json("{\"force_node\":\"Great Lakes\"}".as_bytes());
		assert_eq!(config.force_node, "Great Lakes");
		let config = Config::from_json("{\"force_node\":\"Lübeck\"}".as_bytes());
		assert_eq!(config.force_node, "Lübeck");
		let config = Config::from_json(br#"{"force_node":"bad	name"}"#);
		assert_eq!(config.force_node, Config::default().force_node);
		let config = Config::from_json(br#"{"force_node":"   "}"#);
		assert_eq!(config.force_node, Config::default().force_node);
	}
}
