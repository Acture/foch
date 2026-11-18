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

	// Per-subject income probe (vassal/colony/... category 2 monthly).
	let subject_pattern = FochStr::from_str(&table.subject_income.pattern);
	let mut subject_address = std::ptr::null_mut();
	let subject_found = unsafe { (host.find_pattern)(host, subject_pattern, &mut subject_address) };
	let subject_probe = if subject_found == result::OK {
		match detour::attach_subject_probe(host, subject_address as *mut u8) {
			Ok(()) => " + subject-income probe",
			Err(error) => {
				log_line(
					host,
					log::WARN,
					&format!("subject-income probe skipped: {error}"),
				);
				""
			}
		}
	} else {
		""
	};

	// The scaling itself: patch the tribute define constants. Every monthly
	// tribute payment (cash, power, manpower) is computed from these, so one
	// patch scales the whole system; the hooks above stay as probes.
	{
		let coefficient = {
			let config = CONFIG.read().unwrap();
			config.coefficient("", coefficient::SubjectKind::Tributary)
		};
		let vassal_coefficient = {
			let config = CONFIG.read().unwrap();
			config.coefficient("", coefficient::SubjectKind::Vassal)
		};
		log_line(
			host,
			log::INFO,
			&format!("coefficients: tribute {coefficient}, vassal {vassal_coefficient}"),
		);
		if coefficient != 1.0 || vassal_coefficient != 1.0 {
			let base = host.exe_base as usize;
			// The defines live deep in .data; a runtime image smaller than the
			// real one (the host test fixture) simply has nothing to patch.
			if table.defines.vassal_share as usize + 4 >= host.exe_size {
				log_line(host, log::INFO, "define patch skipped: image too small");
			} else {
				for (name, rva, is_float, coefficient) in [
					("cash_rate", table.defines.cash_rate, false, coefficient),
					("adm_rate", table.defines.adm_rate, true, coefficient),
					("dip_rate", table.defines.dip_rate, true, coefficient),
					("mil_rate", table.defines.mil_rate, true, coefficient),
					(
						"manpower_rate",
						table.defines.manpower_rate,
						true,
						coefficient,
					),
					(
						"vassal_share",
						table.defines.vassal_share,
						true,
						vassal_coefficient,
					),
				] {
					let address = base + rva as usize;
					// The cash rate is a milli-percent int; the power/manpower rates
					// are IEEE f32 constants, so scale through the bit pattern.
					let (before, after) = unsafe {
						if is_float {
							let raw = *(address as *const f32);
							(
								raw.to_bits() as i64,
								(raw * coefficient as f32).to_bits() as i64,
							)
						} else {
							let raw = *(address as *const i32);
							(raw as i64, (raw as f64 * coefficient).round() as i64)
						}
					};
					let bytes = (after as u32).to_le_bytes();
					let mut patch: *mut foch_plugin_sdk::FochPatch = std::ptr::null_mut();
					let created = unsafe {
						(host.patch_create)(host, address as *mut _, bytes.as_ptr(), 4, &mut patch)
					};
					if created != result::OK {
						return refuse(format!("patch {name}: {}", result_name(created)));
					}
					let enabled = unsafe { (host.patch_set_enabled)(host, patch, 1) };
					if enabled != result::OK {
						return refuse(format!("patch {name}: {}", result_name(enabled)));
					}
					log_line(
						host,
						log::INFO,
						&format!("define {name}: {before} -> {after}"),
					);
				}
			}
		}
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

/// The settlement detour. Facts from the signature table, not choices:
/// `int* GetTributeAmount(int* out, CCountry* subject, u8 type)` — x64
/// Microsoft ABI, so RCX/RDX/R8 arrive as the three parameters below.
mod detour {
	use foch_plugin_sdk::{FochHook, FochHostApi, result};
	use std::ffi::c_void;
	use std::sync::atomic::{AtomicPtr, Ordering};

	/// The host-owned trampoline cell; call through `*next` on every call.
	static NEXT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

	/// Test-only: point the chain cell at a stub original.
	#[cfg(test)]
	pub(crate) fn set_next_for_test(cell: *mut c_void) {
		NEXT.store(cell, Ordering::Relaxed);
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

	pub(crate) unsafe extern "C" fn tribute_executor(command: *mut u8, partner: u64) {
		let cell = NEXT.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		let config = crate::CONFIG.read().unwrap();
		// Tag lookup is not mapped yet (see signatures table), so tag
		// overrides fall back to the tributary kind coefficient.
		let coefficient = config.coefficient("", crate::coefficient::SubjectKind::Tributary);
		let coefficient_value = coefficient;
		if coefficient != 1.0 {
			unsafe {
				let amount = command.add(0x1c) as *mut i32;
				let before = *amount;
				*amount = (before as f64 * coefficient).round() as i32;
				log_transfer(before, *amount, coefficient_value);
			}
		}
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: Original = unsafe { *(cell as *const Original) };
		unsafe { original(command, partner) };
	}

	/// Every income category the game actually pays, observed on the real
	/// monthly tick: (category, amount) pairs, first-sight only. This is the
	/// probe that maps `vassal_income` and friends to their enum values.
	static mut SEEN_CATEGORIES: Vec<(i32, i32)> = Vec::new();

	static mut SEEN_SUBJECT_INCOME: Vec<(i64, i64)> = Vec::new();

	/// Per-subject income contribution probe: dump the amount and the head of
	/// the subject object so a real-game run reveals the kind discriminant.
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
		type SubjectOriginal = unsafe extern "C" fn(*mut u8, *mut i32, *mut u8, *mut u8);
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: SubjectOriginal = unsafe { *(cell as *const SubjectOriginal) };
		unsafe { original(country, out, subject, tooltip) };
		unsafe {
			let amount = *out;
			if amount != 0 {
				let seen = &raw mut SEEN_SUBJECT_INCOME;
				// Zero dereference beyond the arguments themselves: during loading
				// the objects may be half-initialized, so only pointer identities
				// are recorded, never their memory.
				let key = subject as i64;
				if (*seen).len() < 500 && !(*seen).iter().any(|&(_, s)| s == key) {
					(*seen).push((amount as i64, key));
					log_line_raw(&format!(
						"subject income {amount} args c={:#x} s={:#x} t={:#x}",
						country as i64, key, tooltip as i64
					));
				}
			}
		}
	}

	static mut SEEN_MONTHLY: Vec<(i32, i32)> = Vec::new();

	unsafe extern "C" fn monthly_apply(country: *mut u8, flush: u8) {
		let cell = NEXT_MONTHLY.load(Ordering::Relaxed);
		if cell.is_null() {
			return;
		}
		type MonthlyOriginal = unsafe extern "C" fn(*mut u8, u8);
		// SAFETY: the chain cell is re-read per call per the ABI contract.
		let original: MonthlyOriginal = unsafe { *(cell as *const MonthlyOriginal) };
		unsafe { original(country, flush) };
		unsafe {
			let country_index = ((*(country.add(0x20) as *const u64)) >> 0x30) as u16 as i32;
			let amount = *(country.add(0x68) as *const i32);
			let seen = &raw mut SEEN_MONTHLY;
			if (*seen).len() < 3000 && !(*seen).iter().any(|(c, _)| *c == country_index) {
				(*seen).push((country_index, amount));
				log_line_raw(&format!(
					"monthly income country {country_index} banks {amount}"
				));
			}
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
		unsafe {
			let seen = &raw mut SEEN_CATEGORIES;
			if (*seen).len() < 400 && !(*seen).iter().any(|(c, _)| *c == category) {
				(*seen).push((category, *amount));
				log_line_raw(&format!(
					"income category {category:#x} first seen amount {}",
					*amount
				));
			}
		}
	}

	fn log_line_raw(message: &str) {
		let host = HOST.load(Ordering::Relaxed) as *const FochHostApi;
		if host.is_null() {
			return;
		}
		unsafe { ((*host).log)(host, crate::log::INFO, crate::FochStr::from_str(message)) };
	}

	fn log_transfer(before: i32, after: i32, coefficient: f64) {
		let host = HOST.load(Ordering::Relaxed) as *const FochHostApi;
		if host.is_null() {
			return;
		}
		let message = format!("tribute transfer {before} -> {after} (x{coefficient:.2})");
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
	use std::sync::atomic::{AtomicI32, Ordering};

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
}
