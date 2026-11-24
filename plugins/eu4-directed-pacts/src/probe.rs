//! Explicit read diagnostic on the existing, pinned monthly game callback.

use crate::receipt::Correlation;
use foch_plugin_sdk::{FochHostApi, FochStr, log, result};
use foch_runtime::{
	dataplane::{CountryFlags, Eu4Game},
	war::eu4::{CampaignStamp, WarRoster},
};
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
	probe_country_flag: String,
}
impl Default for Config {
	fn default() -> Self {
		Self {
			read_only_roster_probe: false,
			probe_case: "roster".into(),
			probe_request: "1".into(),
			probe_wars: "HAB:FRA,ENG:FRA".into(),
			probe_country_flag: String::new(),
		}
	}
}

pub(super) struct Request {
	correlation: Correlation,
	wars: Vec<(String, String)>,
	flag: Option<(String, String)>,
}

impl Request {
	fn correlation_at(&self, observation: usize) -> Result<Correlation, String> {
		let base = self
			.correlation
			.request()
			.parse::<u64>()
			.map_err(|error| error.to_string())?;
		let number = base
			.checked_add(u64::try_from(observation).map_err(|error| error.to_string())?)
			.ok_or("diagnostic request range overflows")?;
		Correlation::new(self.correlation.case(), &number.to_string())
	}
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
	let flag = if config.probe_country_flag.is_empty() {
		None
	} else {
		let (country, name) = config
			.probe_country_flag
			.split_once(':')
			.ok_or("flag probe requires COUNTRY:FLAG")?;
		if !wars
			.iter()
			.any(|(attacker, defender)| country == attacker || country == defender)
			|| name.is_empty()
			|| name.len() > 64
			|| !name
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
		{
			return Err(
				"flag probe requires one requested war principal and a 1..64 byte flag key".into(),
			);
		}
		Some((country.to_string(), name.to_string()))
	};
	let request = Request {
		correlation,
		wars,
		flag,
	};
	let count = request.wars.len() + usize::from(request.flag.is_some());
	request.correlation_at(count - 1)?; // Refuse the whole range before any hook.
	Ok(Some(request))
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
		.enumerate()
		.map(|(index, war)| {
			let correlation = request.correlation_at(index)?;
			let tags = |countries: &[foch_runtime::dataplane::war::CountryRef]| {
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
				("case", correlation.case().into()),
				("request", correlation.request().into()),
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

fn flag_message(
	request: &Request,
	stamp: &CampaignStamp,
	flags: &CountryFlags,
) -> Result<Option<String>, String> {
	let Some((country, name)) = &request.flag else {
		return Ok(None);
	};
	if flags.country_tag != *country {
		return Err("flag snapshot is from another actual country".into());
	}
	let matching: Vec<_> = flags
		.flags
		.iter()
		.filter(|flag| flag.name == *name)
		.collect();
	let flag = match matching.as_slice() {
		[] => return Ok(None),
		[flag] => *flag,
		_ => return Err("duplicate observed flag names".into()),
	};
	let correlation = request.correlation_at(request.wars.len())?;
	let payload: BTreeMap<&str, String> = [
		("type", "country_flag".into()),
		("status", "observed".into()),
		("case", correlation.case().into()),
		("request", correlation.request().into()),
		("country", flags.country_tag.clone()),
		("flag", flag.name.clone()),
		("set_at_hours", flag.set_at_hours.to_string()),
		("campaign", stamp.campaign.clone()),
		("day", stamp.day.to_string()),
		("native_calls", "0".into()),
	]
	.into();
	serde_json::to_string(&payload)
		.map(|json| Some(format!("FOCH_RECEIPT/1 {json}")))
		.map_err(|error| error.to_string())
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
	let Some(mut messages) = messages(&probe.request, &rosters)? else {
		return Ok(());
	};
	let host = unsafe { &*(probe.host as *const FochHostApi) };
	if let Some((tag, _)) = &probe.request.flag {
		// Reuse identities already reconciled against Countries above. The flag
		// diagnostic is restricted to one of the requested war's principals.
		let indices: std::collections::BTreeSet<_> = rosters
			.iter()
			.flat_map(|war| [&war.original_attacker, &war.original_defender])
			.filter(|country| country.tag == *tag)
			.map(|country| country.id)
			.collect();
		if indices.len() != 1 {
			return Err("flag country identity is absent or ambiguous".into());
		}
		let country = countries
			.country(indices.first().unwrap().0 as usize)
			.ok_or("flag country is unavailable")?;
		let digest = unsafe { host.exe_sha256.as_str() }.unwrap_or("");
		// SAFETY: the same synchronous callback/context and registry objects as
		// the roster read; no native pointers survive this callback.
		let flags =
			unsafe { game.country_flags(&country, digest) }.map_err(|error| error.to_string())?;
		let stamp = unsafe { game.campaign_stamp() }?;
		let Some(message) = flag_message(&probe.request, &stamp, &flags)? else {
			return Ok(());
		};
		messages.push(message);
	}
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
	use foch_runtime::{dataplane::war::CountryRef, war::CountryId};
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
			r#"{"read_only_roster_probe":true,"probe_request":"18446744073709551615"}"#,
			r#"{"read_only_roster_probe":true,"probe_request":"18446744073709551614","probe_country_flag":"FRA:flag"}"#,
			r#"{"read_only_roster_probe":true,"probe_wars":"FRA:FRA"}"#,
			r#"{"read_only_roster_probe":true,"probe_wars":"HAB:FRA,HAB:FRA"}"#,
			r#"{"force_join":true}"#,
		] {
			assert!(configuration(text.as_bytes()).is_err());
		}
		let last_two = configuration(
			br#"{"read_only_roster_probe":true,"probe_request":"18446744073709551614"}"#,
		)
		.unwrap()
		.unwrap();
		let observed = messages(&last_two, &[war("HAB"), war("ENG")])
			.unwrap()
			.unwrap();
		let last: serde_json::Value =
			serde_json::from_str(observed[1].strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
		assert_eq!(last["request"], u64::MAX.to_string());
	}
	#[test]
	fn flag_receipt_requires_the_requested_real_country_and_exact_observed_flag() {
		use foch_runtime::{
			dataplane::{CountryFlag, CountryFlags},
			war::eu4::CampaignStamp,
		};
		let request = configuration(
			br#"{"read_only_roster_probe":true,"probe_country_flag":"FRA:foch_dp_roster_flag"}"#,
		)
		.unwrap()
		.unwrap();
		let stamp = CampaignStamp {
			campaign: "stored-guid".into(),
			day: 123,
		};
		let mut flags = CountryFlags {
			country_tag: "FRA".into(),
			flags: vec![],
		};
		assert!(flag_message(&request, &stamp, &flags).unwrap().is_none());
		flags.flags.push(CountryFlag {
			name: "unrelated".into(),
			set_at_hours: 10,
		});
		assert!(flag_message(&request, &stamp, &flags).unwrap().is_none());
		flags.flags.push(CountryFlag {
			name: "foch_dp_roster_flag".into(),
			set_at_hours: 456,
		});
		let message = flag_message(&request, &stamp, &flags).unwrap().unwrap();
		let json: serde_json::Value =
			serde_json::from_str(message.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
		for (key, expected) in [
			("type", "country_flag"),
			("country", "FRA"),
			("flag", "foch_dp_roster_flag"),
			("campaign", "stored-guid"),
			("day", "123"),
			("set_at_hours", "456"),
			("native_calls", "0"),
			("request", "3"),
		] {
			assert_eq!(json[key], expected);
		}
		assert!(
			json.as_object()
				.unwrap()
				.values()
				.all(|value| value.is_string())
		);
		flags.country_tag = "SWE".into();
		assert!(flag_message(&request, &stamp, &flags).is_err());
		for value in ["FRA:", "SWE:flag", "FRA:two:flags", "FRA:flag with spaces"] {
			let text =
				serde_json::json!({"read_only_roster_probe":true,"probe_country_flag":value})
					.to_string();
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
		for (index, message) in observed.into_iter().enumerate() {
			let json: serde_json::Value =
				serde_json::from_str(message.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
			assert_eq!(json["native_calls"], "0");
			assert_eq!(json["type"], "war_roster");
			assert_eq!(json["status"], "observed");
			assert_eq!(json["request"], (index + 1).to_string());
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
