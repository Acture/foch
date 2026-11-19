//! foch-eu4-tribute — Foch native plugin (ABI 1.0).
//!
//! Scales each subject nation's tribute to its overlord by a configurable
//! percentage: tag replaces the subject-type key, then each `copy_from`
//! parent, then `default`, then global. The master switch is passthrough.
//! The coefficients live in a runtime config the slider panel (a later
//! milestone) mutates; until that lands, the manifest config and
//! `tribute-config.json` in the plugin data directory are the interface.

#![allow(clippy::missing_safety_doc)]

mod coefficient;
mod config;
mod panel;
mod signature;
mod subject_types;

use foch_plugin_sdk::{
	ABI_MAJOR, ABI_MINOR, FochHostApi, FochPluginApi, FochStatus, FochStr, dir, log, result, state,
};
use foch_runtime::dataplane::{self, Memory};
use std::sync::RwLock;

/// The runtime configuration the hot path reads and the panel writes.
static CONFIG: RwLock<coefficient::TributeConfig> = RwLock::new(coefficient::TributeConfig::ONCE);
static WATCH: std::sync::Mutex<Option<config::ConfigWatch>> = std::sync::Mutex::new(None);

/// Why the plugin is not active; reported through status/detail. The string
/// is leaked once per refusal — refusals happen at most once per launch.
static REFUSAL: RwLock<Option<&'static str>> = RwLock::new(None);

unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	let host = match unsafe { host.as_ref() } {
		Some(host) if host.abi_major == ABI_MAJOR => host,
		_ => return result::E_INVALID,
	};
	let refuse = |detail: String| {
		log_line(host, log::WARN, &format!("refusing: {detail}"));
		*REFUSAL.write().unwrap() = Some(Box::leak(detail.into_boxed_str()));
		unsafe { (host.report)(host, &FochStatus::new(state::REFUSED)) };
		result::OK
	};

	// Runtime configuration: manifest config overlaid with the data file.
	let config_json = unsafe { (host.config_json)(host) };
	let Some(config_json) = (unsafe { config_json.as_str() }) else {
		return refuse("host configuration is not valid UTF-8".into());
	};
	let data_dir = unsafe { (host.dir)(host, dir::DATA) };
	let data_dir = unsafe { data_dir.as_str() }.unwrap_or_default();
	let baseline = coefficient::TributeConfig::from_manifest_config(config_json);
	let config = baseline
		.clone()
		.overlay_data_file(std::path::Path::new(data_dir));
	*CONFIG.write().unwrap() = config;
	if !data_dir.is_empty() {
		install_runtime_watch(std::path::PathBuf::from(data_dir), baseline);
	}

	// The signature table gates everything: no verified table, no hook.
	let table = match signature::load_embedded() {
		Ok(table) => table,
		Err(error) => return refuse(error),
	};
	if let Err(gap) = table.usable() {
		return refuse(gap);
	}

	// Resolve the settlement address; one unique match or nothing.
	let pattern = FochStr::from_str(&table.tribute_settlement.pattern);
	let mut address = std::ptr::null_mut();
	let found = unsafe { (host.find_pattern)(host, pattern, &mut address) };
	if found != result::OK {
		return refuse(format!(
			"tribute_settlement pattern: {}",
			result_name(found)
		));
	}
	let offset = table.tribute_settlement.offset;
	let address = ((address as usize).wrapping_add_signed(offset as isize)) as *mut u8;

	// Reset define-scale tracking before attaching hooks or applying patches.
	detour::set_applied_scales(1.0, 1.0);

	let exe_sha256 = unsafe { host.exe_sha256.as_str() }.unwrap_or_default();
	let dataplane_supported = dataplane::supports_build(exe_sha256);
	detour::set_exe_base(if dataplane_supported {
		host.exe_base as usize
	} else {
		0
	});
	log_line(
		host,
		log::INFO,
		&format!("dataplane build match: {dataplane_supported}"),
	);

	// Per-subject income hook (vassal/colony/... category 2 monthly + UI).
	let subject_pattern = FochStr::from_str(&table.subject_income.pattern);
	let mut subject_address = std::ptr::null_mut();
	let subject_found = unsafe { (host.find_pattern)(host, subject_pattern, &mut subject_address) };
	let (subject_hooked, subject_probe) = if subject_found == result::OK {
		match detour::attach_subject_probe(host, subject_address as *mut u8) {
			Ok(()) => (true, " + subject-income hook"),
			Err(error) => {
				log_line(
					host,
					log::WARN,
					&format!("subject-income hook skipped: {error}"),
				);
				(false, "")
			}
		}
	} else {
		(false, "")
	};

	// Baseline scaling via the tribute define constants. Per-tag overrides
	// are layered on top in `tribute_executor` and `subject_income` using the
	// residual multiplier `desired / applied_define_scale`.
	{
		let (coefficient, vassal_coefficient, has_tag_overrides) = {
			let config = CONFIG.read().unwrap();
			let types = subject_types::ACTIVE.read().unwrap();
			(
				config.coefficient("", "tributary", &types),
				config.coefficient("", "vassal", &types),
				!config.tag_percent.is_empty(),
			)
		};
		log_line(
			host,
			log::INFO,
			&format!(
				"coefficients: tribute {coefficient}, vassal {vassal_coefficient}, tag_overrides {has_tag_overrides}"
			),
		);
		let mut applied_tribute = 1.0_f64;
		let mut applied_vassal = 1.0_f64;
		if coefficient != 1.0 || vassal_coefficient != 1.0 {
			let base = host.exe_base as usize;
			// Hardcoded define RVAs are pinned to the verified 1.37.5 build;
			// skip them when the build digest does not match or the image is
			// a small host test fixture.
			if !dataplane_supported || table.defines.vassal_share as usize + 4 >= host.exe_size {
				log_line(
					host,
					log::INFO,
					"define patch skipped: unverified build or image too small",
				);
			} else {
				// If subject_income is hooked and the user configured 0% vassal
				// baseline alongside per-tag overrides, leave vassal_share
				// untouched so subject_income can still scale non-zero tag
				// overrides instead of receiving 0 from the engine.
				let patch_vassal_define = vassal_coefficient != 1.0
					&& !(subject_hooked && vassal_coefficient == 0.0 && has_tag_overrides);
				let memory = Memory;
				let patch_define = |name: &str,
				                    rva: u32,
				                    is_float: bool,
				                    scale: f64|
				 -> Result<(), String> {
					let address = base + rva as usize;
					// The cash rate is a milli-percent int; the power/manpower rates
					// are IEEE f32 constants, so scale through the bit pattern.
					let raw_i32 = memory
						.read_i32(address)
						.ok_or_else(|| format!("read define {name}: unreadable"))?;
					let (before, after) = if is_float {
						let raw = f32::from_bits(raw_i32 as u32);
						(raw.to_bits() as i64, (raw * scale as f32).to_bits() as i64)
					} else {
						(raw_i32 as i64, (raw_i32 as f64 * scale).round() as i64)
					};
					let bytes = (after as u32).to_le_bytes();
					let mut patch: *mut foch_plugin_sdk::FochPatch = std::ptr::null_mut();
					let created = unsafe {
						(host.patch_create)(host, address as *mut _, bytes.as_ptr(), 4, &mut patch)
					};
					if created != result::OK {
						return Err(format!("patch {name}: {}", result_name(created)));
					}
					let enabled = unsafe { (host.patch_set_enabled)(host, patch, 1) };
					if enabled != result::OK {
						return Err(format!("patch {name}: {}", result_name(enabled)));
					}
					log_line(
						host,
						log::INFO,
						&format!("define {name}: {before} -> {after}"),
					);
					Ok(())
				};

				if coefficient != 1.0 {
					for (name, rva, is_float) in [
						("cash_rate", table.defines.cash_rate, false),
						("adm_rate", table.defines.adm_rate, true),
						("dip_rate", table.defines.dip_rate, true),
						("mil_rate", table.defines.mil_rate, true),
						("manpower_rate", table.defines.manpower_rate, true),
					] {
						if let Err(detail) = patch_define(name, rva, is_float, coefficient) {
							return refuse(detail);
						}
					}
					applied_tribute = coefficient;
				}
				if patch_vassal_define {
					if let Err(detail) = patch_define(
						"vassal_share",
						table.defines.vassal_share,
						true,
						vassal_coefficient,
					) {
						return refuse(detail);
					}
					applied_vassal = vassal_coefficient;
				}
			}
		}
		detour::set_applied_scales(applied_tribute, applied_vassal);
	}

	// Income probe: the add_income pattern from the same table.
	let pattern = FochStr::from_str(&table.add_income.pattern);
	let mut income_address = std::ptr::null_mut();
	let found = unsafe { (host.find_pattern)(host, pattern, &mut income_address) };
	let monthly_pattern = FochStr::from_str(&table.monthly_apply.pattern);
	let mut monthly_address = std::ptr::null_mut();
	let monthly_found = unsafe { (host.find_pattern)(host, monthly_pattern, &mut monthly_address) };
	let monthly_probe = if monthly_found == result::OK {
		match detour::attach_monthly_probe(host, monthly_address as *mut u8) {
			Ok(()) => " + monthly probe",
			Err(error) => {
				log_line(host, log::WARN, &format!("monthly probe skipped: {error}"));
				""
			}
		}
	} else {
		""
	};
	let income_probe = if found == result::OK {
		match detour::attach_income_probe(host, income_address as *mut u8) {
			Ok(()) => " + add_income probe",
			Err(error) => {
				log_line(
					host,
					log::WARN,
					&format!("add_income probe skipped: {error}"),
				);
				""
			}
		}
	} else {
		""
	};

	// The detour's calling convention is part of the signature table; the
	// reversing milestone fills in `detour` and this becomes a hook_create.
	let hooked = match detour::attach(host, address, &table) {
		Ok(()) => {
			log_line(
				host,
				log::INFO,
				&format!("tribute settlement hooked{income_probe}{monthly_probe}{subject_probe}"),
			);
			true
		}
		Err(error) => {
			// The executor probe is advisory; scaling rides on the define
			// patches, and stack-relative prologues are (correctly) refused.
			log_line(
				host,
				log::WARN,
				&format!("tribute settlement probe skipped: {error}"),
			);
			false
		}
	};
	if hooked {
		*REFUSAL.write().unwrap() = None;
		unsafe { (host.report)(host, &FochStatus::new(state::ACTIVE)) };
	} else {
		// Define patches are already in place; the plugin is active even
		// though the advisory probe could not be installed.
		*REFUSAL.write().unwrap() = None;
		unsafe { (host.report)(host, &FochStatus::new(state::ACTIVE)) };
	}
	result::OK
}

unsafe extern "C" fn status(out: *mut FochStatus) {
	let refusal = *REFUSAL.read().unwrap();
	let state_value = if refusal.is_some() {
		state::REFUSED
	} else {
		state::ACTIVE
	};
	let detail = refusal.unwrap_or("scaling subject tribute");
	if let Some(out) = unsafe { out.as_mut() } {
		*out = FochStatus {
			size: std::mem::size_of::<FochStatus>() as u32,
			state: state_value,
			reason_code: 0,
			detail: FochStr::from_str(detail),
		};
	}
}

static PLUGIN_API: FochPluginApi = FochPluginApi {
	size: std::mem::size_of::<FochPluginApi>() as u32,
	abi_major: ABI_MAJOR,
	abi_minor: ABI_MINOR,
	// The host does not deliver live callbacks yet; the panel mutates the
	// runtime config directly, so neither capability is claimed.
	caps: 0,
	init,
	status,
	config_changed: None,
	set_enabled: None,
	shutdown: None,
};

/// The one export the host looks for. No side effects, per the header rules.
#[unsafe(no_mangle)]
pub extern "C" fn foch_plugin_get_api(host_abi_major: u32) -> *const FochPluginApi {
	if host_abi_major != ABI_MAJOR {
		return std::ptr::null();
	}
	&PLUGIN_API
}

pub use coefficient::TributeConfig;
pub use panel::SliderModel;
pub use subject_types::SubjectTypeGraph;

fn log_line(host: &FochHostApi, level: i32, message: &str) {
	unsafe { (host.log)(host, level, FochStr::from_str(message)) };
}

fn install_runtime_watch(dir: std::path::PathBuf, baseline: coefficient::TributeConfig) {
	*WATCH.lock().unwrap() = Some(config::ConfigWatch::new(dir, baseline));
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn clear_runtime_watch() {
	if let Ok(mut watch) = WATCH.lock() {
		*watch = None;
	}
}

pub(crate) fn poll_runtime_at(now_ms: u64) {
	let replacement = {
		let Ok(mut guard) = WATCH.lock() else {
			return;
		};
		let Some(watch) = guard.as_mut() else {
			return;
		};
		watch.poll(now_ms)
	};
	if let Some(config) = replacement {
		*CONFIG.write().unwrap() = config;
	}
}

fn poll_runtime() {
	let now = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|elapsed| elapsed.as_millis() as u64)
		.unwrap_or(0);
	poll_runtime_at(now);
}

fn active_coefficient(tag: &str, kind: &str) -> f64 {
	poll_runtime();
	let config = CONFIG.read().unwrap();
	let types = subject_types::ACTIVE.read().unwrap();
	config.coefficient(tag, kind, &types)
}

fn result_name(code: i32) -> &'static str {
	match code {
		result::E_NOT_FOUND => "not found",
		result::E_AMBIGUOUS => "ambiguous",
		result::E_UNSUPPORTED => "unsupported",
		result::E_CONFLICT => "conflict",
		result::E_MEMORY => "memory",
		_ => "error",
	}
}

/// Settlement and subject-income detours.
///
/// Every game-memory read and write goes through `foch_runtime::dataplane::Memory`
/// so null, unmapped, or half-initialized objects degrade to `None` instead of
/// faulting. Country tags come from `CountryView` (`from_ptr`, `try_tag`,
/// `index`). The context global and the province array stay inside
/// `Eu4Game::from_exe_base` and `Eu4Game::province`.
mod detour {
	use foch_plugin_sdk::{FochHook, FochHostApi, result};
	use foch_runtime::dataplane::{Memory, game};
	use std::ffi::c_void;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

	/// Tribute payment command amount offset (`i32`, milli-ducats) in `FUN_140b1c800`.
	const COMMAND_AMOUNT_OFFSET: usize = 0x1c;
	/// Upper bound on engine country indices. Matches
	/// `foch_runtime::dataplane::game::valid_country_count`, which accepts a
	/// registry of up to `i16::MAX` countries for overhaul mods. Indices are
	/// zero-based, so `i16::MAX` itself is not a valid index into that registry.
	const MAX_COUNTRY_INDEX: usize = i16::MAX as usize;
	/// `std::vector<int32>` of province ids on `FUN_1403f1510`'s third argument.
	const SUBJECT_PROVINCE_BEGIN: usize = 0x88;
	const SUBJECT_PROVINCE_END: usize = 0x90;
	/// Candidate packed country identity on `CProvince`. Static 1.37.5: the
	/// income function compares its index bits with the income recipient.
	/// Not live-verified. Anchored at `Province::raw_address`; the data plane
	/// does not publish this offset.
	pub(crate) const PROVINCE_OWNER: usize = 0x468;
	/// Refuse a province-id span longer than this before touching it.
	const MAX_PROVINCE_SPAN: usize = 4096;
	/// How many ids the hot path inspects looking for a subject owner.
	const MAX_PROVINCE_SCAN: usize = 32;
	const MAX_PROVINCE_ID: i32 = 65_535;

	static EXE_BASE: AtomicUsize = AtomicUsize::new(0);

	pub(crate) fn set_exe_base(base: usize) {
		EXE_BASE.store(base, Ordering::Relaxed);
	}

	/// The host-owned trampoline cell; call through `*next` on every call.
	static NEXT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

	/// Baseline scale already baked into engine defines by `init()`, stored as
	/// `f64::to_bits()`. Detours apply the residual `desired / applied` so
	/// normal subjects are never double-scaled while per-tag overrides still hit
	/// their exact target percentage.
	static APPLIED_TRIBUTE_SCALE: AtomicU64 = AtomicU64::new(1.0_f64.to_bits());
	static APPLIED_VASSAL_SCALE: AtomicU64 = AtomicU64::new(1.0_f64.to_bits());

	pub(crate) fn set_applied_scales(tribute: f64, vassal: f64) {
		APPLIED_TRIBUTE_SCALE.store(tribute.to_bits(), Ordering::Relaxed);
		APPLIED_VASSAL_SCALE.store(vassal.to_bits(), Ordering::Relaxed);
	}

	fn applied_tribute_scale() -> f64 {
		f64::from_bits(APPLIED_TRIBUTE_SCALE.load(Ordering::Relaxed))
	}

	fn applied_vassal_scale() -> f64 {
		f64::from_bits(APPLIED_VASSAL_SCALE.load(Ordering::Relaxed))
	}

	pub(crate) fn residual_scale(desired: f64, applied_define_scale: f64) -> f64 {
		if applied_define_scale > 0.0 && (applied_define_scale - 1.0).abs() > f64::EPSILON {
			desired / applied_define_scale
		} else {
			desired
		}
	}

	/// Decode a packed EU4 country identity qword via the shared dataplane
	/// decoder (`foch_runtime::dataplane::game::decode_packed_tag`); the
	/// local copy was removed in favor of the master-pinned implementation.
	pub(crate) fn decode_packed_country_tag(
		packed: u64,
		require_valid_flag: bool,
	) -> Option<(String, u16)> {
		let decoded = game::decode_packed_tag(packed, require_valid_flag)?;
		if decoded.index as usize >= MAX_COUNTRY_INDEX {
			return None;
		}
		Some((decoded.tag, decoded.index))
	}

	/// Guarded read of a country's packed tag and index.
	pub(crate) fn read_country_tag(memory: &Memory, country_addr: usize) -> Option<(String, u16)> {
		let view = game::CountryView::from_ptr(*memory, country_addr)?;
		let tag = view.try_tag()?;
		let index = view.index()?;
		if usize::from(index) >= MAX_COUNTRY_INDEX {
			return None;
		}
		Some((tag, index))
	}

	fn live_game() -> Option<game::Eu4Game> {
		let base = EXE_BASE.load(Ordering::Relaxed);
		game::Eu4Game::from_exe_base(base)
	}

	/// Subject tag for `FUN_1403f1510`.
	///
	/// The third argument is not a `CCountry`. It holds province ids at
	/// `+0x88`/`+0x90`. Each id is `game.province(id)`, and the candidate
	/// owner at `raw_address + PROVINCE_OWNER` is a packed country identity.
	/// The income function keeps provinces whose owner index matches the
	/// recipient, so those owners are the overlord. The first owner with a
	/// different index is the subject. Anything unreadable, oversized, or
	/// identical to the overlord yields `None` and the kind coefficient.
	///
	/// The vector element itself (`FUN_140abbf00`) only uses `element+0x10`
	/// to reach this same province-id object. That function loads no type
	/// name and does not reference `copy_from`; a `CSubjectType` key is not
	/// established on `element` or `*(element+0x10)`.
	pub(crate) fn subject_tag_from_provinces(
		game: &game::Eu4Game,
		subject: usize,
		overlord_index: Option<u16>,
	) -> Option<String> {
		let memory = &game.memory;
		let begin = memory.read_qword(subject.checked_add(SUBJECT_PROVINCE_BEGIN)?)? as usize;
		let end = memory.read_qword(subject.checked_add(SUBJECT_PROVINCE_END)?)? as usize;
		if begin == 0 && end == 0 {
			return None;
		}
		if begin <= 0x1_0000 || end < begin || !(end - begin).is_multiple_of(4) {
			return None;
		}
		let count = (end - begin) / 4;
		if count == 0 || count > MAX_PROVINCE_SPAN {
			return None;
		}
		let scan = count.min(MAX_PROVINCE_SCAN);
		for index in 0..scan {
			let Some(id) = memory.read_i32(begin + index * 4) else {
				continue;
			};
			if !(0..=MAX_PROVINCE_ID).contains(&id) {
				continue;
			}
			let Some(province) = game.province(id as u32) else {
				continue;
			};
			let Some(owner_addr) = province.raw_address().checked_add(PROVINCE_OWNER) else {
				continue;
			};
			let Some(packed) = memory.read_qword(owner_addr) else {
				continue;
			};
			let Some((tag, owner_index)) = decode_packed_country_tag(packed, false) else {
				continue;
			};
			if overlord_index == Some(owner_index) {
				continue;
			}
			return Some(tag);
		}
		None
	}

	/// Resolve the subject tag from `tribute_executor`'s `partner: u64` argument.
	///
	/// Accepts either a packed `CCountryTag` passed by value (high validity byte
	/// non-zero, never collides with a canonical x64 user-mode pointer) or a
	/// `CCountry*` pointer whose packed identity `CountryView` reads.
	pub(crate) fn resolve_partner_tag(memory: &Memory, partner: u64) -> Option<String> {
		if let Some((tag, _)) = decode_packed_country_tag(partner, true) {
			return Some(tag);
		}
		if partner > 0x1_0000 && partner & 7 == 0 {
			return read_country_tag(memory, partner as usize).map(|(tag, _)| tag);
		}
		None
	}

	/// Test-only: point the tribute chain cell at a stub original.
	#[cfg(test)]
	pub(crate) fn set_next_for_test(cell: *mut c_void) {
		NEXT.store(cell, Ordering::Relaxed);
	}

	/// Test-only: point the subject_income chain cell at a stub original.
	#[cfg(test)]
	pub(crate) fn set_next_subject_for_test(cell: *mut c_void) {
		NEXT_SUBJECT.store(cell, Ordering::Relaxed);
	}

	/// Kept alive for the process lifetime so a future disable path can reach
	/// the host handle; never dereferenced here.
	static mut HOOK: *mut FochHook = std::ptr::null_mut();
	/// The host table, kept from init so the detour can log each transfer.
	static HOST: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
	static NEXT_INCOME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
	static NEXT_MONTHLY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
	static NEXT_SUBJECT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

	pub(crate) type Original = unsafe extern "C" fn(*mut u8, u64);
	pub(crate) type SubjectOriginal = unsafe extern "C" fn(*mut u8, *mut i32, *mut u8, *mut u8);

	pub(crate) unsafe extern "C" fn tribute_executor(command: *mut u8, partner: u64) {
		let cell = NEXT.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		let memory = Memory;
		let tag = resolve_partner_tag(&memory, partner);
		let desired = crate::active_coefficient(tag.as_deref().unwrap_or(""), "tributary");
		let scale = residual_scale(desired, applied_tribute_scale());
		if let Some(amount_addr) = (command as usize).checked_add(COMMAND_AMOUNT_OFFSET)
			&& let Some(before) = memory.read_i32(amount_addr)
		{
			let mut after = before;
			if (scale - 1.0).abs() > f64::EPSILON {
				let scaled = (before as f64 * scale).round() as i32;
				if scaled != before {
					// SAFETY: `command + 0x1c` is the engine-owned command amount
					// field on the game thread, with no Rust alias.
					if unsafe { memory.write_i32(amount_addr, scaled) }.is_some() {
						log_transfer(tag.as_deref().unwrap_or("?"), before, scaled, desired);
						after = scaled;
					}
				}
			}
			let reported = tag.as_deref().unwrap_or("");
			let case = if reported.is_empty() {
				"untagged"
			} else {
				reported
			};
			emit_receipt("tribute", case, "tributary", reported, scale, before, after);
		}
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: Original = unsafe { *(cell as *const Original) };
		unsafe { original(command, partner) };
	}

	/// Every income category the game actually pays, observed on the real
	/// monthly tick: (category, amount) pairs, first-sight only.
	static SEEN_CATEGORIES: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());

	static SEEN_SUBJECT_INCOME: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());

	/// Per-subject income contribution (`FUN_1403f1510`): resolves the subject's
	/// 3-letter tag via `dataplane::Memory`, applies any per-tag / residual
	/// scaling to `*out`, and records first-sight probe data safely.
	pub(crate) unsafe extern "C" fn subject_income(
		country: *mut u8,
		out: *mut i32,
		subject: *mut u8,
		tooltip: *mut u8,
	) {
		let cell = NEXT_SUBJECT.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: SubjectOriginal = unsafe { *(cell as *const SubjectOriginal) };
		unsafe { original(country, out, subject, tooltip) };

		let memory = Memory;
		let Some(before) = memory.read_i32(out as usize) else {
			return;
		};
		if before == 0 {
			return;
		}
		let overlord = read_country_tag(&memory, country as usize);
		let subject_tag = live_game().and_then(|game| {
			subject_tag_from_provinces(
				&game,
				subject as usize,
				overlord.as_ref().map(|(_, index)| *index),
			)
		});
		let desired = crate::active_coefficient(subject_tag.as_deref().unwrap_or(""), "vassal");
		let scale = residual_scale(desired, applied_vassal_scale());
		let mut after = before;
		if (scale - 1.0).abs() > f64::EPSILON {
			let scaled = (before as f64 * scale).round() as i32;
			if scaled != before {
				// SAFETY: `out` is the engine-provided i32 out-parameter on the
				// game thread, already populated by `original`, with no Rust alias.
				if unsafe { memory.write_i32(out as usize, scaled) }.is_some() {
					after = scaled;
				}
			}
		}
		let reported = subject_tag.as_deref().unwrap_or("");
		let case = overlord
			.as_ref()
			.map(|(tag, _)| tag.as_str())
			.filter(|tag| !tag.is_empty())
			.unwrap_or(if reported.is_empty() {
				"untagged"
			} else {
				reported
			});
		emit_receipt(
			"subject_income",
			case,
			"vassal",
			reported,
			scale,
			before,
			after,
		);

		let key = subject as i64;
		if let Ok(mut seen) = SEEN_SUBJECT_INCOME.try_lock()
			&& seen.len() < 500
			&& !seen.iter().any(|&(_, s)| s == key)
		{
			seen.push((after as i64, key));
			let overlord_tag = overlord.as_ref().map(|(tag, _)| tag.as_str());
			// The third argument is the province-list object, not a CCountry.
			// `subject_tag` came from CProvince+0x468 via that list.
			let mut slot = [0u8; 0x40];
			let slot_ok = memory.read_bytes(subject as usize, &mut slot).is_some();
			let slot_hex = slot
				.iter()
				.map(|b| format!("{b:02x}"))
				.collect::<Vec<_>>()
				.join("");
			log_line_raw(&format!(
				"subject income {} -> {} (x{desired:.2}) overlord={} subject={} slot={} c={:#x} s={:#x}",
				before,
				after,
				overlord_tag.unwrap_or("?"),
				subject_tag.as_deref().unwrap_or("?"),
				if slot_ok { slot_hex.as_str() } else { "-" },
				country as i64,
				key,
			));
		}
	}

	static SEEN_MONTHLY: Mutex<Vec<(u16, i32)>> = Mutex::new(Vec::new());

	unsafe extern "C" fn monthly_apply(country: *mut u8, flush: u8) {
		let cell = NEXT_MONTHLY.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		type MonthlyOriginal = unsafe extern "C" fn(*mut u8, u8);
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: MonthlyOriginal = unsafe { *(cell as *const MonthlyOriginal) };
		unsafe { original(country, flush) };

		let memory = Memory;
		let Some(view) = game::CountryView::from_ptr(memory, country as usize) else {
			return;
		};
		let Some(tag) = view.try_tag() else {
			return;
		};
		let Some(country_index) = view.index() else {
			return;
		};
		if usize::from(country_index) >= MAX_COUNTRY_INDEX {
			return;
		}
		let Some(amount) = view.monthly_income_bank() else {
			return;
		};
		if let Ok(mut seen) = SEEN_MONTHLY.try_lock()
			&& seen.len() < 3000
			&& !seen.iter().any(|(c, _)| *c == country_index)
		{
			seen.push((country_index, amount));
			log_line_raw(&format!(
				"monthly income {tag} ({country_index}) banks {amount}"
			));
		}
	}

	unsafe extern "C" fn add_income(country: *mut u8, category: i32, amount: *mut i32, flag: u8) {
		let cell = NEXT_INCOME.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		type IncomeOriginal = unsafe extern "C" fn(*mut u8, i32, *mut i32, u8);
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: IncomeOriginal = unsafe { *(cell as *const IncomeOriginal) };
		unsafe { original(country, category, amount, flag) };

		let memory = Memory;
		let Some(value) = memory.read_i32(amount as usize) else {
			return;
		};
		if let Ok(mut seen) = SEEN_CATEGORIES.try_lock()
			&& seen.len() < 400
			&& !seen.iter().any(|(c, _)| *c == category)
		{
			seen.push((category, value));
			log_line_raw(&format!(
				"income category {category:#x} first seen amount {value}"
			));
		}
	}

	fn log_line_raw(message: &str) {
		let host = HOST.load(Ordering::Relaxed) as *const FochHostApi;
		if host.is_null() {
			return;
		}
		unsafe { ((*host).log)(host, crate::log::INFO, crate::FochStr::from_str(message)) };
	}

	/// `FOCH_RECEIPT/1` payload. Every value is a string; envelope keys stay
	/// with the host. `case` correlates to the country the test named, and
	/// `request` is the stable token `1` for this hook's monthly observation.
	pub(crate) fn receipt_message(
		hook: &str,
		case: &str,
		kind: &str,
		tag: &str,
		scale: f64,
		before: i32,
		after: i32,
	) -> String {
		let status = if after != before {
			"scaled"
		} else {
			"passthrough"
		};
		let payload = serde_json::json!({
			"case": case,
			"request": "1",
			"type": hook,
			"status": status,
			"tag": tag,
			"kind": kind,
			"scale": receipt_number(scale),
			"before": before.to_string(),
			"after": after.to_string(),
		});
		format!("FOCH_RECEIPT/1 {payload}")
	}

	fn receipt_number(value: f64) -> String {
		if !value.is_finite() {
			return "0".to_string();
		}
		let text = format!("{value:.6}");
		let trimmed = text.trim_end_matches('0').trim_end_matches('.');
		if trimmed.is_empty() || trimmed == "-" {
			"0".to_string()
		} else {
			trimmed.to_string()
		}
	}

	fn emit_receipt(
		hook: &str,
		case: &str,
		kind: &str,
		tag: &str,
		scale: f64,
		before: i32,
		after: i32,
	) {
		log_line_raw(&receipt_message(
			hook, case, kind, tag, scale, before, after,
		));
	}

	fn log_transfer(tag: &str, before: i32, after: i32, coefficient: f64) {
		let host = HOST.load(Ordering::Relaxed) as *const FochHostApi;
		if host.is_null() {
			return;
		}
		let message = format!("tribute transfer [{tag}] {before} -> {after} (x{coefficient:.2})");
		unsafe { ((*host).log)(host, crate::log::INFO, crate::FochStr::from_str(&message)) };
	}

	pub(super) fn attach(
		host: &FochHostApi,
		address: *mut u8,
		_table: &crate::signature::SignatureTable,
	) -> Result<(), String> {
		let mut next: *const *const c_void = std::ptr::null();
		let mut hook: *mut FochHook = std::ptr::null_mut();
		let created = unsafe {
			(host.hook_create)(
				host,
				address.cast(),
				tribute_executor as *mut c_void,
				50,
				&mut next,
				&mut hook,
			)
		};
		if created != result::OK {
			return Err(format!("hook_create: {}", crate::result_name(created)));
		}
		NEXT.store(next as *mut c_void, Ordering::Relaxed);
		HOST.store(host as *const FochHostApi as *mut c_void, Ordering::Relaxed);
		let enabled = unsafe { (host.hook_set_enabled)(host, hook, 1) };
		if enabled != result::OK {
			return Err(format!("hook_set_enabled: {}", crate::result_name(enabled)));
		}
		unsafe { HOOK = hook }
		Ok(())
	}

	pub(super) fn attach_subject_probe(host: &FochHostApi, address: *mut u8) -> Result<(), String> {
		let mut next: *const *const c_void = std::ptr::null();
		let mut hook: *mut FochHook = std::ptr::null_mut();
		let created = unsafe {
			(host.hook_create)(
				host,
				address.cast(),
				subject_income as *mut c_void,
				20,
				&mut next,
				&mut hook,
			)
		};
		if created != result::OK {
			return Err(format!(
				"hook_create(subject_income): {}",
				crate::result_name(created)
			));
		}
		NEXT_SUBJECT.store(next as *mut c_void, Ordering::Relaxed);
		HOST.store(host as *const FochHostApi as *mut c_void, Ordering::Relaxed);
		let enabled = unsafe { (host.hook_set_enabled)(host, hook, 1) };
		if enabled != result::OK {
			return Err(format!(
				"hook_set_enabled(subject_income): {}",
				crate::result_name(enabled)
			));
		}
		Ok(())
	}

	pub(super) fn attach_monthly_probe(host: &FochHostApi, address: *mut u8) -> Result<(), String> {
		let mut next: *const *const c_void = std::ptr::null();
		let mut hook: *mut FochHook = std::ptr::null_mut();
		let created = unsafe {
			(host.hook_create)(
				host,
				address.cast(),
				monthly_apply as *mut c_void,
				30,
				&mut next,
				&mut hook,
			)
		};
		if created != result::OK {
			return Err(format!(
				"hook_create(monthly_apply): {}",
				crate::result_name(created)
			));
		}
		NEXT_MONTHLY.store(next as *mut c_void, Ordering::Relaxed);
		HOST.store(host as *const FochHostApi as *mut c_void, Ordering::Relaxed);
		let enabled = unsafe { (host.hook_set_enabled)(host, hook, 1) };
		if enabled != result::OK {
			return Err(format!(
				"hook_set_enabled(monthly_apply): {}",
				crate::result_name(enabled)
			));
		}
		Ok(())
	}

	/// Probe hook on AddIncome (see the signature table): pure logging while
	/// the category map is being learned on the real monthly tick.
	pub(super) fn attach_income_probe(host: &FochHostApi, address: *mut u8) -> Result<(), String> {
		let mut next: *const *const c_void = std::ptr::null();
		let mut hook: *mut FochHook = std::ptr::null_mut();
		let created = unsafe {
			(host.hook_create)(
				host,
				address.cast(),
				add_income as *mut c_void,
				40,
				&mut next,
				&mut hook,
			)
		};
		if created != result::OK {
			return Err(format!(
				"hook_create(add_income): {}",
				crate::result_name(created)
			));
		}
		NEXT_INCOME.store(next as *mut c_void, Ordering::Relaxed);
		HOST.store(host as *const FochHostApi as *mut c_void, Ordering::Relaxed);
		let enabled = unsafe { (host.hook_set_enabled)(host, hook, 1) };
		if enabled != result::OK {
			return Err(format!(
				"hook_set_enabled(add_income): {}",
				crate::result_name(enabled)
			));
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicI32, Ordering};

	/// Serialize tests that mutate process-wide detour statics (`CONFIG`, `NEXT`, etc.).
	static DETOUR_TEST_LOCK: Mutex<()> = Mutex::new(());

	struct ResetSubjectTypes;

	impl ResetSubjectTypes {
		fn clear() -> Self {
			*subject_types::ACTIVE.write().unwrap() = subject_types::SubjectTypeGraph::EMPTY;
			detour::set_exe_base(0);
			clear_runtime_watch();
			Self
		}

		fn install(graph: subject_types::SubjectTypeGraph) -> Self {
			*subject_types::ACTIVE.write().unwrap() = graph;
			Self
		}
	}

	impl Drop for ResetSubjectTypes {
		fn drop(&mut self) {
			detour::set_exe_base(0);
			clear_runtime_watch();
			if let Ok(mut active) = subject_types::ACTIVE.write() {
				*active = subject_types::SubjectTypeGraph::EMPTY;
			}
		}
	}

	/// Decoy packed identity on the province-list argument. The income path
	/// must ignore this slot and read the province owner instead. It is the
	/// argument object's own field, not a published country offset.
	const SUBJECT_DECOY_SLOT: usize = 0x20;

	/// Third-argument stand-in: province ids at +0x88/+0x90, owners written
	/// at the `0x468` candidate field, and a decoy packed tag that must be
	/// ignored. The scene keeps the synthetic image alive.
	#[allow(dead_code)]
	struct ProvinceSubject {
		ids: Vec<i32>,
		subject: Vec<u64>,
		scene: ProvinceScene,
	}

	#[cfg(windows)]
	struct ProvinceScene(foch_runtime::dataplane::testing::FakeGame);
	#[cfg(not(windows))]
	struct ProvinceScene;

	impl ProvinceSubject {
		fn bind(&mut self) {
			#[cfg(windows)]
			{
				let base = self.scene.0.exe_base().expect("synthetic image base");
				detour::set_exe_base(base);
			}
		}

		fn game(&mut self) -> Option<foch_runtime::dataplane::game::Eu4Game> {
			#[cfg(windows)]
			{
				Some(self.scene.0.game().expect("synthetic game"))
			}
			#[cfg(not(windows))]
			{
				None
			}
		}
	}

	fn province_subject(owners: &[([u8; 3], i16)], decoy: u64) -> ProvinceSubject {
		let scene = plant_province_scene(owners);
		let mut ids = Vec::with_capacity(owners.len());
		for (index, _) in owners.iter().enumerate() {
			ids.push(index as i32);
		}
		let mut subject = vec![0_u64; 0xA0 / 8];
		subject[SUBJECT_DECOY_SLOT / 8] = decoy;
		subject[0x88 / 8] = ids.as_ptr() as u64;
		// SAFETY: `ids` is not mutated after this pointer is stored, and the
		// buffer outlives every read in the test.
		subject[0x90 / 8] = unsafe { ids.as_ptr().add(ids.len()) as u64 };
		let mut built = ProvinceSubject {
			ids,
			subject,
			scene,
		};
		built.bind();
		built
	}

	#[cfg(windows)]
	fn plant_province_scene(owners: &[([u8; 3], i16)]) -> ProvinceScene {
		let mut scene = foch_runtime::dataplane::testing::FakeGame::new();
		for (index, (tag, country_index)) in owners.iter().enumerate() {
			let id = u32::try_from(index).expect("province id");
			scene.add_province(id, 0, 0).expect("synthetic province");
			let packed = pack_country_tag(tag, *country_index, 0);
			scene
				.write_province(id, detour::PROVINCE_OWNER, &packed.to_le_bytes())
				.expect("province owner");
		}
		ProvinceScene(scene)
	}

	#[cfg(not(windows))]
	fn plant_province_scene(_owners: &[([u8; 3], i16)]) -> ProvinceScene {
		ProvinceScene
	}

	fn pack_country_tag(tag: &[u8; 3], index: i16, valid_flag: u8) -> u64 {
		let idx = index.to_le_bytes();
		u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, valid_flag])
	}

	/// Synthetic `CCountry` whose packed identity `CountryView` can read.
	/// `valid_flag` stays in the signature so callers still record the flag
	/// the live path accepts; `FakeGame` seals a valid identity.
	struct SyntheticCountry {
		address: usize,
		_keep: CountryScene,
	}

	#[cfg(windows)]
	#[allow(dead_code)] // keeps the synthetic country allocation alive
	struct CountryScene(foch_runtime::dataplane::testing::FakeGame);
	#[cfg(not(windows))]
	struct CountryScene;

	fn fake_country(tag: &[u8; 3], index: i16, valid_flag: u8) -> SyntheticCountry {
		let (address, _keep) = plant_country(tag, index, valid_flag);
		SyntheticCountry { address, _keep }
	}

	#[cfg(windows)]
	fn plant_country(tag: &[u8; 3], index: i16, valid_flag: u8) -> (usize, CountryScene) {
		assert_eq!(valid_flag, 1, "synthetic countries carry a valid identity");
		let index = u16::try_from(index).expect("country index");
		let tag = std::str::from_utf8(tag).expect("country tag");
		let mut scene = foch_runtime::dataplane::testing::FakeGame::new();
		scene
			.add_country(tag, index, 0, None, &[])
			.expect("synthetic country");
		let address = scene
			.country(usize::from(index))
			.expect("country view")
			.raw_address();
		(address, CountryScene(scene))
	}

	#[cfg(not(windows))]
	fn plant_country(_tag: &[u8; 3], _index: i16, _valid_flag: u8) -> (usize, CountryScene) {
		(0, CountryScene)
	}

	#[test]
	fn receipt_payload_is_only_strings() {
		let scaled =
			detour::receipt_message("subject_income", "ENG", "vassal", "WAL", 1.5, 1000, 1500);
		assert!(scaled.starts_with("FOCH_RECEIPT/1 "));
		let payload: serde_json::Value =
			serde_json::from_str(scaled.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
		let object = payload.as_object().unwrap();
		assert!(object.values().all(serde_json::Value::is_string));
		assert_eq!(object["case"], "ENG");
		assert_eq!(object["request"], "1");
		assert_eq!(object["type"], "subject_income");
		assert_eq!(object["kind"], "vassal");
		assert_eq!(object["tag"], "WAL");
		assert_eq!(object["scale"], "1.5");
		assert_eq!(object["before"], "1000");
		assert_eq!(object["after"], "1500");
		assert_eq!(object["status"], "scaled");
		for forbidden in ["plugin_id", "run_id", "format", "phase"] {
			assert!(!object.contains_key(forbidden), "{forbidden}");
		}
		let same = detour::receipt_message("tribute", "KOR", "tributary", "KOR", 1.0, 40, 40);
		let passthrough: serde_json::Value =
			serde_json::from_str(same.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
		assert_eq!(passthrough["status"], "passthrough");
		assert_eq!(passthrough["scale"], "1");
	}

	#[test]
	fn the_entry_point_rejects_other_abi_majors() {
		assert!(foch_plugin_get_api(ABI_MAJOR + 1).is_null());
		assert!(!foch_plugin_get_api(ABI_MAJOR).is_null());
	}

	#[test]
	fn the_api_table_is_sized_and_claims_no_caps() {
		assert_eq!(PLUGIN_API.size, std::mem::size_of::<FochPluginApi>() as u32);
		assert_eq!(PLUGIN_API.caps, 0);
	}

	/// The full detour behavior: the command amount (milli, at +0x1c) is
	/// scaled by the runtime coefficient before the original runs, and the
	/// chain cell is honored exactly as the host ABI prescribes.
	#[test]
	fn the_detour_scales_the_command_amount_before_the_original() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::clear();
		static SEEN: AtomicI32 = AtomicI32::new(i32::MIN);
		unsafe extern "C" fn original(command: *mut u8, _partner: u64) {
			unsafe {
				SEEN.store(*(command.add(0x1c) as *const i32), Ordering::SeqCst);
			}
		}
		// The cell the host would own: points at the original function.
		static mut CELL: detour::Original = original as detour::Original;
		let cell = (&raw const CELL) as *mut std::ffi::c_void;
		detour::set_next_for_test(cell);
		detour::set_applied_scales(1.0, 1.0);

		*CONFIG.write().unwrap() = coefficient::TributeConfig {
			enabled: true,
			global_percent: 50,
			..coefficient::TributeConfig::ONCE
		};
		let mut command = [0u8; 0x20];
		command[0x1c..0x20].copy_from_slice(&1000i32.to_le_bytes());

		unsafe { detour::tribute_executor(command.as_mut_ptr(), 0) };

		assert_eq!(
			SEEN.load(Ordering::SeqCst),
			500,
			"amount must be halved in the original call"
		);
		assert_eq!(
			i32::from_le_bytes(command[0x1c..0x20].try_into().unwrap()),
			500
		);

		// Disabled passthrough: the command must reach the original untouched.
		CONFIG.write().unwrap().enabled = false;
		command[0x1c..0x20].copy_from_slice(&1000i32.to_le_bytes());
		unsafe { detour::tribute_executor(command.as_mut_ptr(), 0) };
		assert_eq!(SEEN.load(Ordering::SeqCst), 1000);
	}

	#[test]
	fn tribute_executor_resolves_tag_overrides_and_avoids_double_scaling() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::clear();
		static SEEN: AtomicI32 = AtomicI32::new(i32::MIN);
		unsafe extern "C" fn original(command: *mut u8, _partner: u64) {
			unsafe {
				SEEN.store(*(command.add(0x1c) as *const i32), Ordering::SeqCst);
			}
		}
		static mut CELL: detour::Original = original as detour::Original;
		detour::set_next_for_test((&raw const CELL) as *mut std::ffi::c_void);
		detour::set_applied_scales(1.0, 1.0);

		let mut cfg = coefficient::TributeConfig {
			enabled: true,
			global_percent: 50,
			..coefficient::TributeConfig::ONCE
		};
		cfg.tag_percent.insert("KOR".into(), 150);
		cfg.tag_percent.insert("MUG".into(), 25);
		*CONFIG.write().unwrap() = cfg;

		// 1. Packed CTag passed by value in `partner`.
		let mut command = [0u8; 0x20];
		command[0x1c..0x20].copy_from_slice(&1000i32.to_le_bytes());
		let kor_packed = pack_country_tag(b"KOR", 42, 1);
		unsafe { detour::tribute_executor(command.as_mut_ptr(), kor_packed) };
		assert_eq!(SEEN.load(Ordering::SeqCst), 1500);

		// 2. CCountry* pointer passed in `partner` (tag read via CountryView).
		let mug_country = fake_country(b"MUG", 77, 1);
		command[0x1c..0x20].copy_from_slice(&1000i32.to_le_bytes());
		unsafe { detour::tribute_executor(command.as_mut_ptr(), mug_country.address as u64) };
		assert_eq!(SEEN.load(Ordering::SeqCst), 250);

		// 3. When defines already applied 0.5x, residual scaling avoids double-scaling
		//    un-overridden tags (JPN stays 500) while lifting KOR from 500 -> 1500.
		detour::set_applied_scales(0.5, 1.0);
		let jpn_packed = pack_country_tag(b"JPN", 55, 1);
		command[0x1c..0x20].copy_from_slice(&500i32.to_le_bytes());
		unsafe { detour::tribute_executor(command.as_mut_ptr(), jpn_packed) };
		assert_eq!(SEEN.load(Ordering::SeqCst), 500);

		command[0x1c..0x20].copy_from_slice(&500i32.to_le_bytes());
		unsafe { detour::tribute_executor(command.as_mut_ptr(), kor_packed) };
		assert_eq!(SEEN.load(Ordering::SeqCst), 1500);

		detour::set_applied_scales(1.0, 1.0);
	}

	#[test]
	fn subject_income_scales_per_tag_and_guards_invalid_subject_pointers() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::clear();
		unsafe extern "C" fn original(
			_country: *mut u8,
			out: *mut i32,
			_subject: *mut u8,
			_tooltip: *mut u8,
		) {
			if !out.is_null() {
				unsafe { *out = 1000 };
			}
		}
		static mut CELL: detour::SubjectOriginal = original as detour::SubjectOriginal;
		detour::set_next_subject_for_test((&raw const CELL) as *mut std::ffi::c_void);
		detour::set_applied_scales(1.0, 1.0);

		let mut cfg = coefficient::TributeConfig {
			enabled: true,
			global_percent: 100,
			..coefficient::TributeConfig::ONCE
		};
		cfg.kind_percent.insert("vassal".into(), 50);
		cfg.tag_percent.insert("WLS".into(), 150);
		// Decoy at subject+0x20. Reading arg3 as a CCountry would apply this
		// instead of the province owner.
		cfg.tag_percent.insert("MUG".into(), 10);
		*CONFIG.write().unwrap() = cfg;

		let overlord = fake_country(b"ENG", 10, 1);
		let wls = province_subject(&[(*b"WLS", 11)], pack_country_tag(b"MUG", 99, 1));
		let mut sco = vec![0_u64; 0xA0 / 8];
		let mut half_init = vec![0_u64; 0xA0 / 8];

		// 1. No define patch: WLS comes from CProvince+0x468 (1500). The decoy
		//    tag at +0x20 is ignored. An empty province list falls back to vassal.
		let mut out = 0_i32;
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				wls.subject.as_ptr() as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1500);

		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				sco.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 500);

		// 2. Define patch already scaled vassal_share by 0.5: residual scaling
		//    leaves SCO at 1000 (no double scaling) and lifts WLS to 3000 (3x residual).
		detour::set_applied_scales(1.0, 0.5);
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				sco.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1000);

		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				wls.subject.as_ptr() as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 3000);

		// 3. Half-initialized (zeroed) and null/unmapped subject pointers never
		//    fault and fall back cleanly to the vassal kind coefficient.
		detour::set_applied_scales(1.0, 1.0);
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				half_init.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 500);

		unsafe {
			detour::subject_income(
				std::ptr::null_mut(),
				&mut out,
				0x1000 as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 500);

		// 4. Disabled passthrough leaves `*out` untouched.
		CONFIG.write().unwrap().enabled = false;
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				wls.subject.as_ptr() as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1000);
	}

	#[test]
	fn subject_income_walks_copy_from_before_the_global_percent() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::install(
			subject_types::SubjectTypeGraph::from_script(
				"vassal = { copy_from = default }\ndefault = {}\n",
			)
			.unwrap(),
		);
		unsafe extern "C" fn original(
			_country: *mut u8,
			out: *mut i32,
			_subject: *mut u8,
			_tooltip: *mut u8,
		) {
			if !out.is_null() {
				unsafe { *out = 1000 };
			}
		}
		static mut CELL: detour::SubjectOriginal = original as detour::SubjectOriginal;
		detour::set_next_subject_for_test((&raw const CELL) as *mut std::ffi::c_void);
		detour::set_applied_scales(1.0, 1.0);

		let mut cfg = coefficient::TributeConfig {
			enabled: true,
			global_percent: 100,
			..coefficient::TributeConfig::ONCE
		};
		cfg.kind_percent.insert("default".into(), 25);
		*CONFIG.write().unwrap() = cfg;

		let overlord = fake_country(b"ENG", 10, 1);
		let mut sco = vec![0_u64; 0xA0 / 8];
		let mut out = 0_i32;
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				sco.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 250);
	}

	#[test]
	fn province_owner_matching_the_overlord_is_not_the_subject_tag() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::clear();
		unsafe extern "C" fn original(
			_country: *mut u8,
			out: *mut i32,
			_subject: *mut u8,
			_tooltip: *mut u8,
		) {
			if !out.is_null() {
				unsafe { *out = 1000 };
			}
		}
		static mut CELL: detour::SubjectOriginal = original as detour::SubjectOriginal;
		detour::set_next_subject_for_test((&raw const CELL) as *mut std::ffi::c_void);
		detour::set_applied_scales(1.0, 1.0);
		let mut cfg = coefficient::TributeConfig {
			enabled: true,
			global_percent: 100,
			..coefficient::TributeConfig::ONCE
		};
		cfg.kind_percent.insert("vassal".into(), 50);
		cfg.tag_percent.insert("ENG".into(), 200);
		cfg.tag_percent.insert("WLS".into(), 150);
		*CONFIG.write().unwrap() = cfg;

		let overlord = fake_country(b"ENG", 10, 1);
		let both = province_subject(&[(*b"ENG", 10), (*b"WLS", 11)], 0);
		let mut out = 0_i32;
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				both.subject.as_ptr() as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1500, "the later non-overlord owner is the subject");

		let mut only_overlord = province_subject(&[(*b"ENG", 10)], 0);
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				only_overlord.subject.as_ptr() as *mut u8,
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 500, "an overlord-only owner falls back to the kind");

		let mut corrupt = [0_u64; 0xA0 / 8];
		corrupt[0x88 / 8] = 0x2_0000;
		corrupt[0x90 / 8] = 0x2_0000 + 4 * 5_000;
		let game = only_overlord.game().expect("synthetic game");
		assert!(
			detour::subject_tag_from_provinces(&game, corrupt.as_ptr() as usize, Some(10),)
				.is_none()
		);
	}

	#[test]
	fn a_saved_config_reloads_on_the_hot_path_after_the_debounce() {
		let _guard = DETOUR_TEST_LOCK.lock().unwrap();
		let _types = ResetSubjectTypes::clear();
		let dir = tempfile::tempdir().unwrap();
		let baseline = coefficient::TributeConfig {
			enabled: true,
			global_percent: 100,
			..coefficient::TributeConfig::ONCE
		};
		install_runtime_watch(dir.path().to_path_buf(), baseline.clone());
		*CONFIG.write().unwrap() = baseline;
		let path = dir.path().join(config::DATA_FILE);
		std::fs::write(&path, r#"{"kind_percent":{"vassal":25}}"#).unwrap();
		std::fs::File::options()
			.write(true)
			.open(&path)
			.unwrap()
			.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_001))
			.unwrap();
		poll_runtime_at(5_000);
		assert_eq!(CONFIG.read().unwrap().kind_percent.get("vassal"), Some(&25));

		std::fs::write(&path, r#"{"kind_percent":{"vassal":10}}"#).unwrap();
		std::fs::File::options()
			.write(true)
			.open(&path)
			.unwrap()
			.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_002))
			.unwrap();
		poll_runtime_at(5_100);
		assert_eq!(
			CONFIG.read().unwrap().kind_percent.get("vassal"),
			Some(&25),
			"a save inside the debounce window waits"
		);
		poll_runtime_at(5_000 + config::RELOAD_DEBOUNCE_MS);
		assert_eq!(CONFIG.read().unwrap().kind_percent.get("vassal"), Some(&10));

		unsafe extern "C" fn original(
			_country: *mut u8,
			out: *mut i32,
			_subject: *mut u8,
			_tooltip: *mut u8,
		) {
			if !out.is_null() {
				unsafe { *out = 1000 };
			}
		}
		static mut CELL: detour::SubjectOriginal = original as detour::SubjectOriginal;
		detour::set_next_subject_for_test((&raw const CELL) as *mut std::ffi::c_void);
		detour::set_applied_scales(1.0, 1.0);
		let overlord = fake_country(b"ENG", 10, 1);
		let mut subject = vec![0_u64; 0xA0 / 8];
		let mut out = 0_i32;
		unsafe {
			detour::subject_income(
				overlord.address as *mut u8,
				&mut out,
				subject.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 100);
	}

	#[test]
	fn packed_country_tag_validation_rejects_malformed_and_null_page() {
		let memory = Memory;
		// Valid packed tag.
		let valid = pack_country_tag(b"C01", 300, 1);
		assert_eq!(
			detour::decode_packed_country_tag(valid, true),
			Some(("C01".into(), 300))
		);
		// Missing valid flag rejected when required (distinguishes canonical x64 pointers).
		let no_flag = pack_country_tag(b"C01", 300, 0);
		assert_eq!(detour::decode_packed_country_tag(no_flag, true), None);
		assert_eq!(
			detour::decode_packed_country_tag(no_flag, false),
			Some(("C01".into(), 300))
		);
		// Non-zero byte 3 (not NUL-terminated), lowercase chars, negative index,
		// or an index at the `i16::MAX` country-count ceiling.
		assert_eq!(
			detour::decode_packed_country_tag(
				u64::from_le_bytes([b'F', b'R', b'A', b'X', 1, 0, 0, 1]),
				false
			),
			None
		);
		assert_eq!(
			detour::decode_packed_country_tag(pack_country_tag(b"fra", 1, 1), false),
			None
		);
		assert_eq!(
			detour::decode_packed_country_tag(pack_country_tag(b"FRA", -1, 1), false),
			None
		);
		assert_eq!(
			detour::decode_packed_country_tag(pack_country_tag(b"FRA", 1024, 1), false),
			Some(("FRA".into(), 1024))
		);
		assert_eq!(
			detour::decode_packed_country_tag(pack_country_tag(b"FRA", i16::MAX - 1, 1), false),
			Some(("FRA".into(), (i16::MAX as u16) - 1))
		);
		assert_eq!(
			detour::decode_packed_country_tag(pack_country_tag(b"FRA", i16::MAX, 1), false),
			None
		);
		// Null page / overflow refused without faulting.
		assert_eq!(detour::read_country_tag(&memory, 0), None);
		assert_eq!(detour::read_country_tag(&memory, 0x1000), None);
		assert_eq!(detour::read_country_tag(&memory, usize::MAX), None);
	}
}
