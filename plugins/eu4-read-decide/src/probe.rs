//! Opt-in read-only monthly probe: one suggestion receipt per install.
//!
//! Configuration (`{"probe":true,"graph":"…","weights":"…","our_tag":"SWE",
//! "our_index":N}`) gates everything; without it the plugin stays
//! REFUSED and installs nothing. The hook rides the shared verified
//! monthly tick (pattern-checked at `layout::TICK_RVA`), reads one
//! frame through the gated candidate readers, scores it with the
//! loaded GeneralGnn, and logs a single `FOCH_RECEIPT/1` line. The
//! detour only ever reads — no game memory writes, no orders.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use foch_plugin_sdk::{FochHostApi, FochStr, log, result};
use foch_runtime::dataplane::Memory;
use foch_runtime::dataplane::layout;

use crate::decide::suggest;
use crate::frame::{ArmySnapshot, DynamicProvince, FrameInput, WarFacts, assemble};
use crate::graph::StaticGraph;

/// The pattern the host must find at the tick RVA before any hook is
/// installed — identical to the trade line's verified probe.
const TICK_PATTERN: &str = "48 89 5C 24 ?? 48 89 6C 24 ?? 57 48 81 EC";

struct Probe {
	host: usize,
	graph: StaticGraph,
	gnn: foch_eu4_gnn::GeneralGnn,
	our_tag: String,
	our_index: i32,
	enemy_index: Option<i32>,
	emitted: AtomicBool,
}

static CONTEXT: OnceLock<Probe> = OnceLock::new();
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(serde::Deserialize)]
pub(super) struct ProbeConfig {
	#[serde(default)]
	probe: bool,
	#[serde(default)]
	graph: String,
	#[serde(default)]
	weights: String,
	#[serde(default)]
	our_tag: String,
	#[serde(default)]
	our_index: i32,
	#[serde(default)]
	enemy_index: Option<i32>,
}

/// Parse and validate the opt-in configuration; `None` when probing is
/// not requested. Validation is total before any hook exists.
pub(super) fn parse_config(config_json: &str) -> Result<Option<ProbeConfig>, String> {
	let config: ProbeConfig =
		serde_json::from_str(config_json).map_err(|e| format!("probe config: {e}"))?;
	if !config.probe {
		return Ok(None);
	}
	if config.graph.is_empty() || config.weights.is_empty() || config.our_tag.is_empty() {
		return Err("probe requires graph, weights, and our_tag".into());
	}
	if config.our_tag.len() != 3 || !config.our_tag.bytes().all(|b| b.is_ascii_uppercase()) {
		return Err("our_tag must be three uppercase letters".into());
	}
	if config.our_index < 0 || config.our_index > 1024 {
		return Err("our_index out of the registry range".into());
	}
	for path in [&config.graph, &config.weights] {
		if path.is_empty() || path.len() > 260 || path.contains("..") {
			return Err("asset paths must be non-empty, relative, and without traversal".into());
		}
	}
	Ok(Some(config))
}

/// Install the monthly hook. Windows x64 only; every address check
/// happens before the hook is created.
pub(super) fn install(host: &FochHostApi, config: ProbeConfig) -> Result<(), String> {
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err("read decide requires Windows x64".into());
	}
	let graph_text = std::fs::read_to_string(&config.graph)
		.map_err(|e| format!("static graph asset {}: {e}", config.graph))?;
	let graph = StaticGraph::from_json(&graph_text).ok_or("static graph asset failed to parse")?;
	let weights = std::fs::read_to_string(&config.weights)
		.map_err(|e| format!("weight asset {}: {e}", config.weights))?;
	let gnn = foch_eu4_gnn::GeneralGnn::from_json(&weights)
		.ok_or("weight asset rejected: malformed or mismatched shapes")?;

	let tick = (host.exe_base as usize)
		.checked_add(layout::TICK_RVA)
		.ok_or("tick address overflow")?;
	let required = layout::CONTEXT_GLOBAL_RVA.max(layout::COUNTRY_MANAGER_GLOBAL_RVA) + 8;
	if host.exe_base.is_null() || host.exe_size < required || layout::TICK_RVA >= host.exe_size {
		return Err("host image does not contain the pinned game fields".into());
	}
	let mut resolved = std::ptr::null_mut();
	let code = unsafe { (host.find_pattern)(host, FochStr::from_str(TICK_PATTERN), &mut resolved) };
	if code != result::OK || resolved as usize != tick {
		return Err("verified monthly tick signature or RVA did not match".into());
	}
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
		return Err(format!("monthly hook was refused: {code}"));
	}
	CONTEXT
		.set(Probe {
			host: host as *const _ as usize,
			graph,
			gnn,
			our_tag: config.our_tag,
			our_index: config.our_index,
			enemy_index: config.enemy_index,
			emitted: AtomicBool::new(false),
		})
		.map_err(|_| "read decide was already initialized")?;
	NEXT.store(next as usize, Ordering::Release);
	let code = unsafe { (host.hook_set_enabled)(host, hook, 1) };
	if code != result::OK {
		return Err(format!("monthly hook enable was refused: {code}"));
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
	let our = standing_stack(&probe.graph).ok_or("our standing province not in graph")?;
	let dynamics = |id: u32| -> Option<DynamicProvince> {
		let address = game.province_address(id)?;
		Some(DynamicProvince {
			// development() returns the normalized column value; the
			// frame wants the engine's thousandths back.
			development_mille: (game.development(address)? * 35_000.0).round() as i32,
			fort_level: game.fort_level(address)?,
			controller_index: Some(game.controller_index(address)?),
			terrain_name: game
				.terrain_of(id, count)
				.as_deref()
				.and_then(buckets::simulator_name),
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
	let state = assemble(&probe.graph, &dynamics, &input).ok_or("frame assembly refused")?;
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

/// Our stack: v1 has no live regiment read, so the snapshot carries a
/// neutral full-strength stack standing on our first owned province
/// of the static graph. Regiment/morale RE replaces this.
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

mod buckets {
	/// Map a real terrain name onto the simulator's five; `None` keeps
	/// the frame refused.
	pub fn simulator_name(name: &str) -> Option<&'static str> {
		match name.to_ascii_lowercase().as_str() {
			"plains" | "grasslands" | "farmlands" | "drylands" | "steppe" | "savannah"
			| "coastline" | "coast desert" => Some("plains"),
			"forest" | "woods" | "broadleaf" | "coniferous" | "taiga" => Some("forest"),
			"hills" => Some("hills"),
			"mountains" | "highlands" => Some("mountains"),
			"marsh" | "wetlands" | "flooded" => Some("marsh"),
			_ => None,
		}
	}
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
	}

	#[test]
	fn real_terrain_names_bucket_into_the_simulator_five() {
		assert_eq!(buckets::simulator_name("Grasslands"), Some("plains"));
		assert_eq!(buckets::simulator_name("Woods"), Some("forest"));
		assert_eq!(buckets::simulator_name("Highlands"), Some("mountains"));
		assert_eq!(buckets::simulator_name("lava"), None);
	}
}
