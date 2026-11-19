//! foch-eu4-tribute — Foch native plugin (ABI 1.0).
//!
//! Scales each subject nation's tribute to its overlord by a configurable
//! percentage: tag replaces kind replaces global, master switch passthrough.
//! The coefficients live in a runtime config the slider panel (a later
//! milestone) mutates; until that lands, the manifest config and
//! `tribute-config.json` in the plugin data directory are the interface.

#![allow(clippy::missing_safety_doc)]

mod coefficient;
mod config;
mod signature;

use foch_plugin_sdk::{
	ABI_MAJOR, ABI_MINOR, FochHostApi, FochPluginApi, FochStatus, FochStr, dir, log, result, state,
};
use foch_runtime::dataplane::{self, Memory};
use std::sync::RwLock;

/// The runtime configuration the hot path reads and the panel writes.
static CONFIG: RwLock<coefficient::TributeConfig> = RwLock::new(coefficient::TributeConfig::ONCE);

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
	let config = coefficient::TributeConfig::from_manifest_config(config_json)
		.overlay_data_file(std::path::Path::new(data_dir));
	*CONFIG.write().unwrap() = config;

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
			(
				config.coefficient("", coefficient::SubjectKind::Tributary),
				config.coefficient("", coefficient::SubjectKind::Vassal),
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

fn log_line(host: &FochHostApi, level: i32, message: &str) {
	unsafe { (host.log)(host, level, FochStr::from_str(message)) };
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
/// faulting. Country tags are decoded from the inline packed identity at
/// `CCountry + layout::country::TAG` (`+0x20`).
mod detour {
	use foch_plugin_sdk::{FochHook, FochHostApi, result};
	use foch_runtime::dataplane::{Memory, game, layout::country};
	use std::ffi::c_void;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

	/// Tribute payment command amount offset (`i32`, milli-ducats) in `FUN_140b1c800`.
	const COMMAND_AMOUNT_OFFSET: usize = 0x1c;
	/// Monthly accumulated income bank (`i32`) on `CCountry`, banked by `ApplyMonthlyIncome`.
	const COUNTRY_MONTHLY_INCOME_BANK: usize = 0x68;
	/// Upper bound on engine country indices (matches `foch_runtime::dataplane::game::Countries`).
	const MAX_COUNTRY_INDEX: usize = 1024;

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

	/// Guarded read of a country's inline packed tag and index at `country + country::TAG`.
	pub(crate) fn read_country_tag(memory: &Memory, country_addr: usize) -> Option<(String, u16)> {
		let tag_addr = country_addr.checked_add(country::TAG)?;
		let packed = memory.read_qword(tag_addr)?;
		decode_packed_country_tag(packed, false)
	}

	/// Resolve the subject tag from `tribute_executor`'s `partner: u64` argument.
	///
	/// Accepts either a packed `CCountryTag` passed by value (high validity byte
	/// non-zero, never collides with a canonical x64 user-mode pointer) or a
	/// `CCountry*` pointer whose `+0x20` holds the inline packed tag.
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
		let desired = {
			let config = crate::CONFIG.read().unwrap();
			config.coefficient(
				tag.as_deref().unwrap_or(""),
				crate::coefficient::SubjectKind::Tributary,
			)
		};
		let scale = residual_scale(desired, applied_tribute_scale());
		if (scale - 1.0).abs() > f64::EPSILON
			&& let Some(amount_addr) = (command as usize).checked_add(COMMAND_AMOUNT_OFFSET)
			&& let Some(before) = memory.read_i32(amount_addr)
		{
			let after = (before as f64 * scale).round() as i32;
			if after != before {
				// SAFETY: `command + 0x1c` is the engine-owned command amount
				// field on the game thread, with no Rust alias.
				if unsafe { memory.write_i32(amount_addr, after) }.is_some() {
					log_transfer(tag.as_deref().unwrap_or("?"), before, after, desired);
				}
			}
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
		let subject_tag = read_country_tag(&memory, subject as usize).map(|(tag, _)| tag);
		let desired = {
			let config = crate::CONFIG.read().unwrap();
			config.coefficient(
				subject_tag.as_deref().unwrap_or(""),
				crate::coefficient::SubjectKind::Vassal,
			)
		};
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

		let key = subject as i64;
		if let Ok(mut seen) = SEEN_SUBJECT_INCOME.try_lock()
			&& seen.len() < 500
			&& !seen.iter().any(|&(_, s)| s == key)
		{
			seen.push((after as i64, key));
			let overlord_tag = read_country_tag(&memory, country as usize).map(|(tag, _)| tag);
			log_line_raw(&format!(
				"subject income {} -> {} (x{desired:.2}) overlord={} subject={} args c={:#x} s={:#x} t={:#x}",
				before,
				after,
				overlord_tag.as_deref().unwrap_or("?"),
				subject_tag.as_deref().unwrap_or("?"),
				country as i64,
				key,
				tooltip as i64
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
		let Some((tag, country_index)) = read_country_tag(&memory, country as usize) else {
			return;
		};
		let Some(amount) = (country as usize)
			.checked_add(COUNTRY_MONTHLY_INCOME_BANK)
			.and_then(|addr| memory.read_i32(addr))
		else {
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

	fn pack_country_tag(tag: &[u8; 3], index: i16, valid_flag: u8) -> u64 {
		let idx = index.to_le_bytes();
		u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, valid_flag])
	}

	/// Build an 8-byte-aligned fake `CCountry` buffer with its inline packed
	/// identity at `layout::country::TAG` (`+0x20`).
	fn fake_country(tag: &[u8; 3], index: i16, valid_flag: u8) -> Vec<u64> {
		let mut buf = vec![0_u64; 16];
		let slot = foch_runtime::dataplane::layout::country::TAG / 8;
		buf[slot] = pack_country_tag(tag, index, valid_flag);
		buf
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

		// 2. CCountry* pointer passed in `partner` (tag read at +0x20 via dataplane::Memory).
		let mug_country = fake_country(b"MUG", 77, 1);
		command[0x1c..0x20].copy_from_slice(&1000i32.to_le_bytes());
		unsafe { detour::tribute_executor(command.as_mut_ptr(), mug_country.as_ptr() as u64) };
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
		*CONFIG.write().unwrap() = cfg;

		let mut overlord = fake_country(b"ENG", 10, 1);
		let mut wls = fake_country(b"WLS", 11, 1);
		let mut sco = fake_country(b"SCO", 12, 1);
		let mut half_init = vec![0_u64; 16];

		// 1. No define patch: WLS gets 150% (1500), SCO gets vassal 50% (500).
		let mut out = 0_i32;
		unsafe {
			detour::subject_income(
				overlord.as_mut_ptr().cast(),
				&mut out,
				wls.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1500);

		unsafe {
			detour::subject_income(
				overlord.as_mut_ptr().cast(),
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
				overlord.as_mut_ptr().cast(),
				&mut out,
				sco.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1000);

		unsafe {
			detour::subject_income(
				overlord.as_mut_ptr().cast(),
				&mut out,
				wls.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 3000);

		// 3. Half-initialized (zeroed) and null/unmapped subject pointers never
		//    fault and fall back cleanly to the vassal kind coefficient.
		detour::set_applied_scales(1.0, 1.0);
		unsafe {
			detour::subject_income(
				overlord.as_mut_ptr().cast(),
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
				overlord.as_mut_ptr().cast(),
				&mut out,
				wls.as_mut_ptr().cast(),
				std::ptr::null_mut(),
			);
		}
		assert_eq!(out, 1000);
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
		// Non-zero byte 3 (not NUL-terminated), lowercase chars, negative or >=1024 index.
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
			None
		);
		// Null page / overflow refused without faulting.
		assert_eq!(detour::read_country_tag(&memory, 0), None);
		assert_eq!(detour::read_country_tag(&memory, 0x1000), None);
		assert_eq!(detour::read_country_tag(&memory, usize::MAX), None);
	}
}
