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

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
	/// Master switch; `false` leaves the plugin an inert observer.
	pub enabled: bool,
	pub mode: Mode,
	/// Engine node name for `force_toggle`, e.g. `lubeck_trade_node`.
	pub force_node: String,
	/// Write the monthly observer dump to the plugin log directory.
	pub observer_log: bool,
	/// Percent weights of the node-wealth aggregate.
	pub wealth_tax_pct: i64,
	pub wealth_prod_pct: i64,
	pub wealth_tp_pct: i64,
	/// Edge judgment thresholds and the basin distance discount, in percent.
	pub converge_pct: i64,
	pub hysteresis_pct: i64,
	pub decay_pct: i64,
	/// Monthly flip budgets.
	pub max_flips_per_node: usize,
	pub max_flips_total: usize,
}

impl Default for Config {
	fn default() -> Self {
		let params = policy::Params::default();
		Self {
			enabled: true,
			mode: Mode::Observe,
			force_node: "Great Lakes".to_string(),
			observer_log: true,
			wealth_tax_pct: params.wealth_tax_pct,
			wealth_prod_pct: params.wealth_prod_pct,
			wealth_tp_pct: params.wealth_tp_pct,
			converge_pct: params.converge_pct,
			hysteresis_pct: params.hysteresis_pct,
			decay_pct: params.decay_pct,
			max_flips_per_node: params.max_flips_per_node,
			max_flips_total: params.max_flips_total,
		}
	}
}

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
			.filter(|node| {
				!node.is_empty() && node.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
			}) {
			config.force_node = node.to_string();
		}
		if let Some(log) = object.get("observer_log").and_then(|v| v.as_bool()) {
			config.observer_log = log;
		}
		for (key, target) in [
			("wealth_tax_pct", &mut config.wealth_tax_pct),
			("wealth_prod_pct", &mut config.wealth_prod_pct),
			("wealth_tp_pct", &mut config.wealth_tp_pct),
			("converge_pct", &mut config.converge_pct),
			("hysteresis_pct", &mut config.hysteresis_pct),
			("decay_pct", &mut config.decay_pct),
		] {
			if let Some(number) = object.get(key).and_then(|v| v.as_i64()) {
				*target = number;
			}
		}
		if let Some(per_node) = object.get("max_flips_per_node").and_then(|v| v.as_u64()) {
			config.max_flips_per_node = per_node.min(usize::MAX as u64) as usize;
		}
		if let Some(total) = object.get("max_flips_total").and_then(|v| v.as_u64()) {
			config.max_flips_total = total.min(usize::MAX as u64) as usize;
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
			br#"{"mode":"policy","converge_pct":150,"force_node":"beijing_trade_node"}"#,
		);
		assert_eq!(config.mode, Mode::Policy);
		assert_eq!(config.converge_pct, 150);
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
		let config = Config::from_json(br#"{"converge_pct":50,"hysteresis_pct":120}"#);
		assert!(!config.enabled);
	}

	#[test]
	fn a_node_name_is_sanitized() {
		let config = Config::from_json(br#"{"force_node":"bad name!"}"#);
		assert_eq!(config.force_node, Config::default().force_node);
	}
}
