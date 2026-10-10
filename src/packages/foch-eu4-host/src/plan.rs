//! The frozen load plan Foch writes into a runtime layer for one launch.
//!
//! The host executes exactly this plan: it never scans a directory and never
//! orders plugins itself. The runtime manager must stage the files and write
//! this wire format; a management-side ordering resolution is not this plan.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const FORMAT: u32 = 1;

/// Where the plan lives, relative to the host DLL's directory.
pub const PLAN_FILE: &str = "foch-host\\plan.json";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
	pub format: u32,
	pub run_id: String,
	#[serde(default)]
	pub game_version: String,
	/// Absolute path of the JSONL event stream the host appends to.
	pub events: String,
	pub plugins: Vec<PlannedPlugin>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
	/// Reserved for a verified legacy fallback; this host currently refuses it.
	ProcessAttach,
	/// At eu4.exe's entry point, before any game code, outside the loader lock.
	Entry,
	/// On the host worker thread after the entry phase.
	Deferred,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
	Legacy,
	Native,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedPlugin {
	pub id: String,
	pub version: String,
	pub kind: Kind,
	pub phase: Phase,
	/// Absolute path of the entry DLL inside the runtime layer.
	pub path: String,
	/// Optional expected entry-DLL digest, checked before executing its DllMain.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sha256: Option<String>,
	#[serde(default = "empty_object")]
	pub config_json: String,
	#[serde(default)]
	pub dirs: BTreeMap<String, String>,
	/// How a legacy plugin's state is read. Native plugins report themselves.
	#[serde(default)]
	pub status: Option<LegacyStatus>,
	/// Native ABI major version the manifest declares.
	#[serde(default)]
	pub abi_major: Option<u32>,
}

fn empty_object() -> String {
	"{}".to_string()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StatusMode {
	/// Read once right after the DLL loads.
	Sync,
	/// Poll until the value leaves `pending` or the timeout elapses.
	Poll,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyStatus {
	/// A no-argument export returning a 32-bit integer.
	pub export: String,
	pub mode: StatusMode,
	#[serde(default)]
	pub timeout_ms: u64,
	#[serde(default)]
	pub pending: Vec<i32>,
	pub values: Vec<StatusValue>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusValue {
	pub value: i32,
	pub state: String,
	#[serde(default)]
	pub reason: Option<String>,
}

impl LegacyStatus {
	/// The state and reason a raw export value maps to; unmapped values fail.
	pub fn interpret(&self, value: i32) -> (&str, Option<String>) {
		match self.values.iter().find(|entry| entry.value == value) {
			Some(entry) => (entry.state.as_str(), entry.reason.clone()),
			None => (
				"failed",
				Some(format!("{} returned unmapped value {value}", self.export)),
			),
		}
	}
}

pub fn parse(text: &str) -> Result<Plan, String> {
	let plan: Plan = serde_json::from_str(text).map_err(|error| error.to_string())?;
	if plan.format != FORMAT {
		return Err(format!(
			"plan format {} is not supported (host reads {FORMAT})",
			plan.format
		));
	}
	if plan.run_id.is_empty() || plan.run_id.len() > 256 || plan.run_id.contains('\0') {
		return Err("invalid run_id".into());
	}
	if !absolute_path(&plan.events) {
		return Err("events must be an absolute Windows path without traversal".into());
	}
	let mut ids = BTreeSet::new();
	let mut paths = BTreeSet::new();
	let mut names = BTreeSet::new();
	let mut phase = Phase::Entry;
	for plugin in &plan.plugins {
		if plugin.id.is_empty()
			|| plugin.id.len() > 256
			|| plugin.id.contains('\0')
			|| plugin.version.is_empty()
			|| plugin.version.contains('\0')
			|| !ids.insert(&plugin.id)
		{
			return Err(format!(
				"{}: invalid or duplicate plugin identity",
				plugin.id
			));
		}
		if !absolute_path(&plugin.path) || !plugin.path.to_ascii_lowercase().ends_with(".dll") {
			return Err(format!(
				"{}: entry must be an absolute DLL path without traversal",
				plugin.id
			));
		}
		let path = plugin.path.replace('/', "\\").to_lowercase();
		let name = path.rsplit('\\').next().unwrap_or_default().to_owned();
		if !paths.insert(path) || !names.insert(name) {
			return Err(format!("{}: duplicate entry DLL name or path", plugin.id));
		}
		if plugin.kind == Kind::Native && plugin.phase == Phase::ProcessAttach {
			return Err(format!(
				"{}: native plugins cannot load under the loader lock",
				plugin.id
			));
		}
		if plugin.phase == Phase::ProcessAttach {
			return Err(format!(
				"{}: process_attach loading has not been verified",
				plugin.id
			));
		}
		if plugin.phase < phase {
			return Err(format!(
				"{}: phase order conflicts with frozen load order",
				plugin.id
			));
		}
		phase = plugin.phase;
		if plugin.kind == Kind::Native && plugin.abi_major != Some(foch_plugin_abi::ABI_MAJOR) {
			return Err(format!(
				"{}: unsupported or missing native ABI major",
				plugin.id
			));
		}
		if !serde_json::from_str::<serde_json::Value>(&plugin.config_json)
			.is_ok_and(|value| value.is_object())
		{
			return Err(format!("{}: config_json must encode an object", plugin.id));
		}
		if plugin
			.sha256
			.as_ref()
			.is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
		{
			return Err(format!("{}: invalid SHA-256", plugin.id));
		}
		for (kind, path) in &plugin.dirs {
			if !matches!(kind.as_str(), "data" | "cache" | "log" | "plugin") || !absolute_path(path)
			{
				return Err(format!("{}: invalid directory {kind}", plugin.id));
			}
		}
		if let Some(status) = &plugin.status {
			if plugin.kind != Kind::Legacy
				|| status.export.is_empty()
				|| !status.export.is_ascii()
				|| status.export.contains('\0')
			{
				return Err(format!("{}: invalid legacy status export", plugin.id));
			}
			if status.mode == StatusMode::Poll
				&& (status.timeout_ms == 0
					|| status.timeout_ms > 120_000
					|| status.pending.is_empty())
			{
				return Err(format!(
					"{}: legacy poll needs a bounded timeout and pending values",
					plugin.id
				));
			}
			let mut values = BTreeSet::new();
			for value in &status.values {
				if !values.insert(value.value)
					|| !matches!(
						value.state.as_str(),
						"active" | "inactive" | "refused" | "failed" | "unknown"
					) {
					return Err(format!("{}: invalid legacy status mapping", plugin.id));
				}
			}
		}
	}
	Ok(plan)
}

fn absolute_path(path: &str) -> bool {
	if path.contains('\0') {
		return false;
	}
	let normalized = path.replace('/', "\\");
	let bytes = normalized.as_bytes();
	let drive =
		bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
	let unc = normalized.starts_with("\\\\")
		&& normalized[2..]
			.split('\\')
			.filter(|p| !p.is_empty())
			.count() >= 2;
	(drive || unc)
		&& !normalized
			.split('\\')
			.any(|part| matches!(part, "." | ".." | "?"))
}

#[cfg(test)]
pub(crate) const FIXTURE: &str = r#"{
	"format": 1,
	"run_id": "run-1",
	"game_version": "1.37.5.0",
	"events": "C:\\rt\\foch-host\\events.jsonl",
	"plugins": [
		{
			"id": "io.github.yozoratempest.eu4-unicode-patch",
			"version": "0.1.14",
			"kind": "legacy",
			"phase": "entry",
			"path": "C:\\rt\\plugins\\eu4_unicode_patch.dll",
			"config_json": "{}",
			"dirs": {},
			"status": {
				"export": "Eu4UnicodeProbeEnabled",
				"mode": "sync",
				"timeout_ms": 0,
				"pending": [],
				"values": [
					{ "value": 1, "state": "active" },
					{ "value": 0, "state": "refused", "reason": "see eu4_unicode_patch.log" }
				]
			},
			"abi_major": null
		},
		{
			"id": "dev.foch.sample",
			"version": "0.1.0",
			"kind": "native",
			"phase": "deferred",
			"path": "C:\\rt\\plugins\\foch_plugin_sample.dll",
			"config_json": "{\"greeting\":\"hi\"}",
			"dirs": { "data": "C:\\data", "log": "C:\\rt\\plugins\\logs" },
			"status": null,
			"abi_major": 1
		}
	]
}"#;

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::Value;

	fn modify(change: impl FnOnce(&mut Value)) -> String {
		let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
		change(&mut value);
		value.to_string()
	}

	#[test]
	fn accepts_unc_share_roots_for_directories_but_not_device_paths() {
		let text = modify(|v| v["plugins"][1]["dirs"]["data"] = "\\\\server\\share".into());
		assert!(parse(&text).is_ok());
		let text = modify(|v| v["plugins"][1]["dirs"]["data"] = "\\\\.\\PhysicalDrive0".into());
		assert!(parse(&text).is_err());
	}

	#[test]
	fn refuses_relative_paths_and_duplicate_plugin_identities() {
		for path in [
			"plugins\\sample.dll",
			"C:sample.dll",
			"\\sample.dll",
			"C:\\rt\\..\\sample.dll",
		] {
			let text = modify(|v| v["plugins"][0]["path"] = path.into());
			assert!(parse(&text).is_err(), "accepted {path}");
		}
		let text = modify(|v| v["plugins"][1]["id"] = v["plugins"][0]["id"].clone());
		assert!(parse(&text).is_err(), "duplicate plugin identity");
		let text = modify(|v| v["plugins"][1]["path"] = v["plugins"][0]["path"].clone());
		assert!(parse(&text).is_err(), "duplicate entry DLL");
		let text = modify(|v| v["events"] = "events.jsonl".into());
		assert!(parse(&text).is_err(), "relative event destination");
	}

	#[test]
	fn refuses_invalid_abi_config_and_phase_order_before_loading() {
		let text = modify(|v| v["plugins"][1]["abi_major"] = 9.into());
		assert!(parse(&text).is_err(), "wrong ABI");
		let text = modify(|v| v["plugins"][1]["config_json"] = "{broken".into());
		assert!(parse(&text).is_err(), "invalid config JSON");
		let text = modify(|v| v["plugins"].as_array_mut().unwrap().reverse());
		assert!(
			parse(&text).is_err(),
			"reordering would violate the frozen plan"
		);
		let text = modify(|v| v["plugins"][0]["status"]["mode"] = "poll".into());
		assert!(parse(&text).is_err(), "unbounded legacy poll");
		let text = modify(|v| v["plugins"][0]["status"]["values"][0]["state"] = "healthy".into());
		assert!(parse(&text).is_err(), "unknown state name");
	}

	#[test]
	fn fixture_parses_and_preserves_order() {
		let plan = parse(FIXTURE).unwrap();
		assert_eq!(plan.plugins.len(), 2);
		assert_eq!(plan.plugins[0].phase, Phase::Entry);
		assert_eq!(plan.plugins[1].kind, Kind::Native);
		let status = plan.plugins[0].status.as_ref().unwrap();
		assert_eq!(status.interpret(1).0, "active");
		assert_eq!(status.interpret(0).0, "refused");
		assert_eq!(status.interpret(7).0, "failed");
	}

	#[test]
	fn native_plugins_never_load_under_the_loader_lock() {
		let text = FIXTURE.replace("\"phase\": \"deferred\"", "\"phase\": \"process_attach\"");
		assert!(parse(&text).unwrap_err().contains("loader lock"));
	}

	#[test]
	fn other_formats_are_refused() {
		let text = FIXTURE.replace("\"format\": 1", "\"format\": 2");
		assert!(parse(&text).is_err());
	}
}
