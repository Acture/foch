//! Opt-in read-only monthly probe: one suggestion receipt per install.
//!
//! Configuration (`{"probe":true,"graph":"…","weights":"…","our_tag":"SWE",
//! "our_index":N}`) gates everything; without it the plugin stays
//! REFUSED and installs nothing. The hook rides the shared verified
//! monthly tick ([`foch_runtime::dataplane::resolve_monthly_tick`]), reads one
//! frame through the gated candidate readers, scores it with the
//! loaded GeneralGnn, and logs a single `FOCH_RECEIPT/1` line. The
//! detour only ever reads — no game memory writes, no orders.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use foch_plugin_sdk::{FochHostApi, FochStr, log, result};
use foch_runtime::dataplane::Memory;

use serde::Deserialize;

use crate::decide::suggest;
use crate::frame::{ArmySnapshot, DynamicProvince, FrameInput, WarFacts, assemble};
use crate::graph::{self, StaticGraph};
use crate::terrain::TerrainCodec;

struct Probe {
	host: usize,
	graph: StaticGraph,
	gnn: foch_eu4_gnn::GeneralGnn,
	terrain_sources: Vec<String>,
	our_tag: String,
	our_index: i32,
	enemy_index: Option<i32>,
	emitted: AtomicBool,
}

/// Why [`install`] refused. The variant decides the reported reason
/// code, so the log detail and the status transition stay consistent.
#[derive(Debug)]
pub(super) enum InstallError {
	/// A model asset (GNN weights, residual heads) was unreadable or
	/// malformed.
	BadAsset(String),
	/// The host-computed digest is not a supported build.
	BuildMismatch(String),
	/// The monthly hook, its target, or a non-model asset failed.
	HookFailed(String),
}

impl std::fmt::Display for InstallError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::BadAsset(text) | Self::BuildMismatch(text) | Self::HookFailed(text) => {
				f.write_str(text)
			}
		}
	}
}

static CONTEXT: OnceLock<Probe> = OnceLock::new();
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(serde::Deserialize)]
pub struct ProbeConfig {
	#[serde(default)]
	pub probe: bool,
	#[serde(default)]
	pub graph: String,
	#[serde(default)]
	pub weights: String,
	#[serde(default)]
	pub our_tag: String,
	#[serde(default)]
	pub our_index: i32,
	#[serde(default)]
	pub enemy_index: Option<i32>,
	/// Paths of staged terrain texts, used when `playset` is empty.
	#[serde(default)]
	pub terrain_sources: Vec<String>,
	/// Game root, then mods, in playset order. A string is comma-separated.
	/// Each root's `descriptor.mod` `replace_path` is applied.
	#[serde(default, deserialize_with = "deserialize_playset")]
	pub playset: Vec<String>,
}

/// Parse and validate the opt-in configuration; `None` when probing is
/// not requested. Validation is total before any hook exists.
pub fn parse_config(config_json: &str) -> Result<Option<ProbeConfig>, String> {
	let config: ProbeConfig =
		serde_json::from_str(config_json).map_err(|e| format!("probe config: {e}"))?;
	if !config.probe {
		return Ok(None);
	}
	if config.weights.is_empty() || config.our_tag.is_empty() {
		return Err("probe requires weights and our_tag".into());
	}
	if config.playset.is_empty() && config.graph.is_empty() {
		return Err("probe requires a playset or a graph asset".into());
	}
	if config.our_tag.len() != 3 || !config.our_tag.bytes().all(|b| b.is_ascii_uppercase()) {
		return Err("our_tag must be three uppercase letters".into());
	}
	// Country indexes are i16 in the packed identity. The shared dataplane
	// accepts 1..=i16::MAX countries, so an overhaul index past 1024 is valid.
	if config.our_index < 0 || config.our_index > i16::MAX as i32 {
		return Err("our_index is outside the country-index range".into());
	}
	for path in [&config.graph, &config.weights] {
		if !path.is_empty() && !acceptable_asset_path(path) {
			return Err("asset paths must not contain traversal".into());
		}
	}
	for path in &config.playset {
		if !acceptable_asset_path(path) {
			return Err("playset paths must be non-empty and without traversal".into());
		}
	}
	Ok(Some(config))
}

fn acceptable_asset_path(path: &str) -> bool {
	!path.is_empty() && path.len() <= 1024 && !path.contains("..")
}

fn deserialize_playset<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	let value = serde_json::Value::deserialize(deserializer)?;
	match value {
		serde_json::Value::String(text) => Ok(split_playset(&text)),
		serde_json::Value::Array(items) => {
			let mut paths = Vec::new();
			for item in items {
				let Some(text) = item.as_str() else {
					return Err(serde::de::Error::custom("playset entries must be strings"));
				};
				if !text.is_empty() {
					paths.push(text.to_string());
				}
			}
			Ok(paths)
		}
		serde_json::Value::Null => Ok(Vec::new()),
		_ => Err(serde::de::Error::custom(
			"playset must be a string or an array",
		)),
	}
}

fn split_playset(text: &str) -> Vec<String> {
	text.split(',')
		.map(str::trim)
		.filter(|path| !path.is_empty())
		.map(str::to_string)
		.collect()
}

fn terrain_from_playset(files: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<String> {
	let mut texts = Vec::new();
	if let Some(bytes) = files.get("map/terrain.txt") {
		texts.push(String::from_utf8_lossy(bytes).into_owned());
	}
	for (path, bytes) in files {
		if path.starts_with("common/terrain/") && path.ends_with(".txt") {
			texts.push(String::from_utf8_lossy(bytes).into_owned());
		}
	}
	texts
}

/// Install the monthly hook. Windows x64 only; the build digest is
/// checked eagerly and every address check happens before the hook is
/// created, so an unsupported build fails closed with nothing installed.
pub(super) fn install(host: &FochHostApi, config: ProbeConfig) -> Result<(), InstallError> {
	// The digest gate, first: the host-computed digest of the image we
	// are loaded into must match a compiled-in layout profile before any
	// asset is read or any hook exists. An unsupported build is refused,
	// not installed-doomed.
	let digest = unsafe { host.exe_sha256.as_str() }
		.ok_or_else(|| InstallError::BuildMismatch("host digest is not valid UTF-8".into()))?;
	if !foch_runtime::dataplane::supports_build(digest) {
		return Err(InstallError::BuildMismatch(
			"the host-computed digest is not a supported build".into(),
		));
	}
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err(InstallError::HookFailed(
			"read decide requires Windows x64".into(),
		));
	}
	let (graph, terrain_sources) = if config.playset.is_empty() {
		let graph_text = std::fs::read_to_string(&config.graph).map_err(|e| {
			InstallError::HookFailed(format!("static graph asset {}: {e}", config.graph))
		})?;
		let graph = StaticGraph::from_json(&graph_text)
			.ok_or_else(|| InstallError::HookFailed("static graph asset failed to parse".into()))?;
		let terrain_sources = config
			.terrain_sources
			.iter()
			.filter_map(|path| std::fs::read_to_string(path).ok())
			.collect::<Vec<_>>();
		(graph, terrain_sources)
	} else {
		let mut layers = Vec::new();
		for root in &config.playset {
			layers.push(
				graph::load_layer(std::path::Path::new(root))
					.map_err(|e| InstallError::HookFailed(format!("playset root {root}: {e}")))?,
			);
		}
		let files = graph::resolve_playset(&layers);
		let terrain_sources = terrain_from_playset(&files);
		let graph = StaticGraph::from_map_files(&files).ok_or_else(|| {
			InstallError::HookFailed("playset map did not yield a province graph".into())
		})?;
		(graph, terrain_sources)
	};
	let weights = std::fs::read_to_string(&config.weights)
		.map_err(|e| InstallError::BadAsset(format!("weight asset {}: {e}", config.weights)))?;
	let gnn = foch_eu4_gnn::GeneralGnn::from_json(&weights).ok_or_else(|| {
		InstallError::BadAsset("weight asset rejected: malformed or mismatched shapes".into())
	})?;

	// SAFETY: `host` is the live table for this plugin init. The resolved
	// pointer is the pinned monthly tick inside that image.
	let resolved = unsafe { foch_runtime::dataplane::resolve_monthly_tick(host) }
		.map_err(|reason| InstallError::HookFailed(reason.into()))?;
	let mut next = std::ptr::null();
	let mut hook = std::ptr::null_mut();
	let code = unsafe {
		(host.hook_create)(
			host,
			resolved,
			detour as *mut c_void,
			0,
			&mut next,
			&mut hook,
		)
	};
	if code != result::OK || next.is_null() || hook.is_null() {
		return Err(InstallError::HookFailed(format!(
			"monthly hook was refused: {code}"
		)));
	}
	CONTEXT
		.set(Probe {
			host: host as *const _ as usize,
			graph,
			gnn,
			terrain_sources,
			our_tag: config.our_tag,
			our_index: config.our_index,
			enemy_index: config.enemy_index.filter(|index| *index >= 0),
			emitted: AtomicBool::new(false),
		})
		.map_err(|_| InstallError::HookFailed("read decide was already initialized".into()))?;
	NEXT.store(next as usize, Ordering::Release);
	let code = unsafe { (host.hook_set_enabled)(host, hook, 1) };
	if code != result::OK {
		return Err(InstallError::HookFailed(format!(
			"monthly hook enable was refused: {code}"
		)));
	}
	Ok(())
}

unsafe extern "C" fn detour(context: *mut c_void, flag: u8) {
	let cell = NEXT.load(Ordering::Acquire);
	if cell == 0 {
		return;
	}
	let next = unsafe { AtomicPtr::from_ptr(cell as *mut *mut c_void) }.load(Ordering::Acquire);
	if next.is_null() {
		return;
	}
	let original: unsafe extern "C" fn(*mut c_void, u8) = unsafe { std::mem::transmute(next) };
	unsafe { original(context, flag) };
	if let Some(probe) = CONTEXT.get() {
		let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			observe(probe, context as usize)
		}));
		let reason = match outcome {
			Ok(Ok(())) => return,
			Ok(Err(reason)) => reason,
			Err(_) => "read decide probe panicked".to_string(),
		};
		let host = unsafe { &*(probe.host as *const FochHostApi) };
		unsafe { (host.log)(host, log::ERROR, FochStr::from_str(&reason)) };
	}
}

/// One gated frame read + suggestion receipt. Failure strings go to
/// the host log; nothing is retried.
fn observe(probe: &Probe, context: usize) -> Result<(), String> {
	if probe.emitted.swap(true, Ordering::AcqRel) {
		return Ok(()); // one receipt per install, like the trade probe
	}
	let host = unsafe { &*(probe.host as *const FochHostApi) };
	// The digest gate: VerifiedGame refuses anything but the pinned
	// build before a single field read. The host reports the digest of
	// the executable it loaded us into.
	let digest = unsafe { host.exe_sha256.as_str() }
		.ok_or_else(|| "host digest is not valid UTF-8".to_string())?
		.to_owned();
	let game = foch_eu4_state::view::VerifiedGame::new(Memory, context, &digest)
		.ok_or("build gate refused the host digest")?;

	let count = game.province_count().ok_or("province count unavailable")?;
	// Terrain decode follows the ACTIVE mod set: the codec's sources
	// are the staged game + mod common/terrain texts from the config.
	let mut codec = TerrainCodec::from_sources(&probe.terrain_sources);
	let our = standing_stack(&probe.graph).ok_or("our standing province not in graph")?;
	let mut dynamics = |id: u32| -> Option<DynamicProvince> {
		let address = game.province_address(id)?;
		Some(DynamicProvince {
			// development() returns the normalized column value; the
			// frame wants the engine's thousandths back.
			development_mille: (game.development(address)? * 35_000.0).round() as i32,
			fort_level: game.fort_level(address)?,
			controller_index: Some(game.controller_index(address)?),
			terrain_bucket: game
				.terrain_of(id, count)
				.as_deref()
				.map(|name| codec.bucket(name)),
		})
	};
	let input = FrameInput {
		our_index: probe.our_index,
		our_tag: probe.our_tag.clone(),
		enemy_index: probe.enemy_index,
		war: WarFacts {
			war_score: None,
			..WarFacts::default()
		},
		our: Some(our),
		..FrameInput::default()
	};
	let state = assemble(&probe.graph, &mut dynamics, &input).ok_or("frame assembly refused")?;
	let suggestion = suggest(&probe.gnn, &state, 8.0, 0.5).ok_or("scoring refused")?;

	let top = suggestion.candidates[suggestion.sampled];
	let target = probe.graph.id_at(top as usize).unwrap_or(u32::MAX);
	let payload: std::collections::BTreeMap<&str, String> = [
		("type", "read_decide".into()),
		("status", "suggested".into()),
		("pick_province", target.to_string()),
		(
			"argmax_province",
			probe
				.graph
				.id_at(suggestion.candidates[suggestion.argmax] as usize)
				.unwrap_or(u32::MAX)
				.to_string(),
		),
		("candidates", suggestion.candidates.len().to_string()),
		("temperature", format!("{}", suggestion.temperature)),
	]
	.into();
	let json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
	unsafe {
		(host.log)(
			host,
			log::INFO,
			FochStr::from_str(&format!("FOCH_RECEIPT/1 {json}")),
		)
	};
	Ok(())
}

/// Our stack until the monthly hook has an army-object address.
/// [`foch_eu4_state::view::VerifiedGame::regiment_counts`] can replace
/// the headcounts once that root exists. Morale stays neutral: its
/// field was not pinned.
fn standing_stack(graph: &StaticGraph) -> Option<ArmySnapshot> {
	let province = graph
		.provinces
		.iter()
		.find(|(_, record)| !record.owner.is_empty())
		.map(|(&id, _)| id)?;
	Some(ArmySnapshot {
		province,
		infantry: 14.0,
		cavalry: 4.0,
		artillery: 2.0,
		morale: 0.75,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn config_requires_opt_in_and_valid_shapes() {
		assert!(parse_config("{}").unwrap().is_none());
		assert!(parse_config("{\"probe\":true}").is_err());
		assert!(parse_config(
			"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"our_tag\":\"swe\",\"our_index\":3}"
		)
		.is_err());
		assert!(parse_config(
			"{\"probe\":true,\"graph\":\"../escape\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":3}"
		)
		.is_err());
		assert!(parse_config(
			"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":3}"
		)
		.is_ok());
		let overhaul = parse_config(
			"{\"probe\":true,\"playset\":\"game,mod\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":1500}",
		)
		.unwrap()
		.unwrap();
		assert_eq!(
			overhaul.playset,
			vec!["game".to_string(), "mod".to_string()]
		);
		assert!(parse_config(
			"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":32767}"
		)
		.is_ok());
		assert!(parse_config(
			"{\"probe\":true,\"graph\":\"g.json\",\"weights\":\"w.json\",\"our_tag\":\"SWE\",\"our_index\":32768}"
		)
		.is_err());
	}
}
