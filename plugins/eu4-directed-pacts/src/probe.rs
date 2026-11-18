//! Explicit read diagnostic on the existing, pinned monthly game callback.

use crate::receipt::Correlation;
use foch_plugin_sdk::{FochHostApi, FochStr, log, result};
use foch_runtime::{dataplane::Eu4Game, war::eu4::WarRoster};
use serde::Deserialize;
use std::{
	collections::BTreeMap,
	ffi::c_void,
	sync::{
		OnceLock,
		atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
	},
};

// The exact signature/ABI already used by the trade plugin. Build identity,
// memory access and detour chaining remain shared Foch services. The void
// return follows static evidence; main-thread/tick thread identity has not
// been measured. This read diagnostic grants no mutation permission.
static NEXT: AtomicUsize = AtomicUsize::new(0);
static CONTEXT: OnceLock<Probe> = OnceLock::new();

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
	read_only_roster_probe: bool,
	probe_case: String,
	probe_request: String,
	probe_wars: String,
}
impl Default for Config {
	fn default() -> Self {
		Self {
			read_only_roster_probe: false,
			probe_case: "roster".into(),
			probe_request: "1".into(),
			probe_wars: "HAB:FRA,ENG:FRA".into(),
		}
	}
}

pub(super) struct Request {
	correlation: Correlation,
	wars: Vec<(String, String)>,
}

pub(super) fn configuration(bytes: &[u8]) -> Result<Option<Request>, String> {
	let config: Config = if bytes.is_empty() {
		Config::default()
	} else {
		serde_json::from_slice(bytes).map_err(|error| error.to_string())?
	};
	if !config.read_only_roster_probe {
		return Ok(None);
	}
	let correlation = Correlation::new(&config.probe_case, &config.probe_request)?;
	let mut wars = vec![];
	for item in config.probe_wars.split(',') {
		let (attacker, defender) = item
			.split_once(':')
			.ok_or("probe wars require ATTACKER:DEFENDER pairs")?;
		let tag = |value: &str| {
			value.len() == 3
				&& value
					.bytes()
					.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
		};
		let pair = (attacker.to_string(), defender.to_string());
		if !tag(attacker)
			|| !tag(defender)
			|| attacker == defender
			|| wars.contains(&pair)
			|| wars.len() >= 4
		{
			return Err("probe requires one to four distinct opposing country pairs".into());
		}
		wars.push(pair);
	}
	Ok(Some(Request { correlation, wars }))
}

struct Probe {
	// The SDK retains the API and callbacks for the plugin's process lifetime.
	host: usize,
	exe_base: usize,
	request: Request,
	emitted: AtomicBool,
}

pub(super) unsafe fn install(host: &FochHostApi, request: Request) -> Result<(), String> {
	// SAFETY: the caller passes the live host table for this plugin init.
	let resolved = unsafe { foch_runtime::dataplane::resolve_monthly_tick(host) }?;
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
		return Err(format!("monthly callback hook was refused: {code}"));
	}
	CONTEXT
		.set(Probe {
			host: host as *const _ as usize,
			exe_base: host.exe_base as usize,
			request,
			emitted: AtomicBool::new(false),
		})
		.map_err(|_| "read diagnostic was already initialized")?;
	NEXT.store(next as usize, Ordering::Release);
	let code = unsafe { (host.hook_set_enabled)(host, hook, 1) };
	if code != result::OK {
		return Err(format!("monthly callback enable was refused: {code}"));
	}
	Ok(())
}

fn messages(request: &Request, rosters: &[WarRoster]) -> Result<Option<Vec<String>>, String> {
	let mut selected = vec![];
	for (attacker, defender) in &request.wars {
		let matching: Vec<_> = rosters
			.iter()
			.filter(|war| {
				war.original_attacker.tag == *attacker && war.original_defender.tag == *defender
			})
			.collect();
		match matching.as_slice() {
			[] => return Ok(None), // The fixture has not established every war yet.
			[war] => selected.push(*war),
			_ => {
				return Err(
					"multiple wars have these original principals; no first-match selection".into(),
				);
			}
		}
	}
	selected
		.into_iter()
		.map(|war| {
			let tags = |countries: &[foch_runtime::war::eu4::CountryRef]| {
				let mut names: Vec<_> = countries
					.iter()
					.map(|country| country.tag.as_str())
					.collect();
				names.sort_unstable();
				names.join(",")
			};
			let payload: BTreeMap<&str, String> = [
				("type", "war_roster".into()),
				("status", "observed".into()),
				("case", request.correlation.case().into()),
				("request", request.correlation.request().into()),
				("original_attacker", war.original_attacker.tag.clone()),
				("original_defender", war.original_defender.tag.clone()),
				("attackers", tags(&war.attackers)),
				("defenders", tags(&war.defenders)),
				("wars", rosters.len().to_string()),
				("native_calls", "0".into()),
			]
			.into();
			serde_json::to_string(&payload)
				.map(|json| format!("FOCH_RECEIPT/1 {json}"))
				.map_err(|error| error.to_string())
		})
		.collect::<Result<Vec<_>, _>>()
		.map(Some)
}

fn observe(probe: &Probe, context: usize) -> Result<(), String> {
	if probe.emitted.load(Ordering::Acquire) {
		return Ok(());
	}
	let Some(game) = Eu4Game::from_tick(probe.exe_base, context) else {
		return Ok(());
	};
	let Some(countries) = game.countries() else {
		return Ok(());
	};
	// Observe synchronously on the callback thread after forwarding next.
	// No mutation or engine object pointer is retained beyond this read.
	// SAFETY: same synchronous callback context the hook just forwarded.
	let rosters = unsafe { game.active_wars() }?;
	for war in &rosters {
		for country in [&war.original_attacker, &war.original_defender]
			.into_iter()
			.chain(&war.attackers)
			.chain(&war.defenders)
		{
			if countries
				.country(country.id.0 as usize)
				.is_none_or(|actual| actual.tag() != country.tag)
			{
				return Err("war participant differs from the real country registry".into());
			}
		}
	}
	let Some(messages) = messages(&probe.request, &rosters)? else {
		return Ok(());
	};
	let host = unsafe { &*(probe.host as *const FochHostApi) };
	for message in messages {
		unsafe { (host.log)(host, log::INFO, FochStr::from_str(&message)) };
	}
	probe.emitted.store(true, Ordering::Release);
	Ok(())
}

unsafe extern "C" fn detour(context: *mut c_void, flag: u8) {
	let cell = NEXT.load(Ordering::Acquire);
	if cell == 0 {
		return;
	}
	// The host publishes this retained, aligned cell with AtomicPtr::store.
	// Acquire mirrors that publication while re-reading the next chain link.
	let next = unsafe { AtomicPtr::from_ptr(cell as *mut *mut c_void) }.load(Ordering::Acquire);
	if next.is_null() {
		return;
	}
	let original: unsafe extern "C" fn(*mut c_void, u8) = unsafe { std::mem::transmute(next) };
	unsafe { original(context, flag) };
	if !super::probe_active() {
		return;
	}
	if let Some(probe) = CONTEXT.get() {
		let outcome = std::panic::catch_unwind(|| observe(probe, context as usize));
		if !matches!(outcome, Ok(Ok(()))) {
			let reason = match outcome {
				Ok(Err(reason)) => reason,
				_ => "read diagnostic panicked".into(),
			};
			let host = unsafe { &*(probe.host as *const FochHostApi) };
			unsafe { (host.log)(host, log::ERROR, FochStr::from_str(&reason)) };
			super::probe_failed(host);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_runtime::war::{CountryId, eu4::CountryRef};
	#[test]
	fn disabled_observation_still_forwards_every_dl_bit_and_rereads_next() {
		static RECEIVED: AtomicUsize = AtomicUsize::new(0);
		static CONTEXT_ARG: AtomicUsize = AtomicUsize::new(0);
		unsafe extern "C" fn first(context: *mut c_void, flag: u8) {
			RECEIVED.store(flag as usize, Ordering::Relaxed);
			CONTEXT_ARG.store(context as usize, Ordering::Relaxed);
		}
		unsafe extern "C" fn second(context: *mut c_void, flag: u8) {
			RECEIVED.store(256 + flag as usize, Ordering::Relaxed);
			CONTEXT_ARG.store(context as usize, Ordering::Relaxed);
		}
		let cell = AtomicPtr::new(first as *mut c_void);
		NEXT.store(cell.as_ptr() as usize, Ordering::Release);
		for flag in 0..=255u8 {
			unsafe { detour(0x12340usize as *mut c_void, flag) };
			assert_eq!(RECEIVED.load(Ordering::Relaxed), flag as usize);
			assert_eq!(CONTEXT_ARG.load(Ordering::Relaxed), 0x12340);
		}
		cell.store(second as *mut c_void, Ordering::Release);
		unsafe { detour(0x56780usize as *mut c_void, 0xff) };
		assert_eq!(RECEIVED.load(Ordering::Relaxed), 511);
		assert_eq!(CONTEXT_ARG.load(Ordering::Relaxed), 0x56780);
		NEXT.store(0, Ordering::Release);
	}
	fn country(tag: &str, id: u32) -> CountryRef {
		CountryRef {
			tag: tag.into(),
			id: CountryId(id),
		}
	}
	fn war(attacker: &str) -> WarRoster {
		WarRoster {
			original_attacker: country(attacker, 1),
			original_defender: country("FRA", 2),
			attackers: vec![country(attacker, 1)],
			defenders: vec![country("FRA", 2)],
		}
	}
	#[test]
	fn diagnostics_are_explicit_and_reject_unattributed_or_ambiguous_requests() {
		assert!(configuration(b"{}").unwrap().is_none());
		assert!(configuration(b"").unwrap().is_none());
		for text in [
			r#"{"read_only_roster_probe":true,"probe_request":"0"}"#,
			r#"{"read_only_roster_probe":true,"probe_wars":"FRA:FRA"}"#,
			r#"{"read_only_roster_probe":true,"probe_wars":"HAB:FRA,HAB:FRA"}"#,
			r#"{"force_join":true}"#,
		] {
			assert!(configuration(text.as_bytes()).is_err());
		}
	}
	#[test]
	fn no_receipt_precedes_both_real_rosters_and_observation_never_claims_a_join() {
		let request = configuration(br#"{"read_only_roster_probe":true}"#)
			.unwrap()
			.unwrap();
		assert!(messages(&request, &[]).unwrap().is_none());
		assert!(messages(&request, &[war("HAB")]).unwrap().is_none());
		let observed = messages(&request, &[war("HAB"), war("ENG")])
			.unwrap()
			.unwrap();
		assert_eq!(observed.len(), 2);
		for message in observed {
			let json: serde_json::Value =
				serde_json::from_str(message.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
			assert_eq!(json["native_calls"], "0");
			assert_eq!(json["type"], "war_roster");
			assert_eq!(json["status"], "observed");
			assert!(
				json.as_object()
					.unwrap()
					.values()
					.all(|value| value.is_string())
			);
			assert!(json.get("war").is_none()); // Stable cross-save identity remains unverified.
		}
		assert!(messages(&request, &[war("HAB"), war("HAB"), war("ENG")]).is_err());
	}
}
