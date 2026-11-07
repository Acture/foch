//! Host lifecycle and UI callbacks. Mixed settlement remains fail-closed until
//! directional validation, AI evaluation and execution are verified together.
use crate::{bridge, composition, native, profile, ui};
use core::ffi::c_void;
use foch_plugin_abi::{self as abi, FochHostApi, FochPluginApi, FochStatus, FochStr};
use std::{
	panic::{AssertUnwindSafe, catch_unwind},
	sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering},
};

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static HOST: AtomicPtr<FochHostApi> = AtomicPtr::new(std::ptr::null_mut());
static BASE: AtomicUsize = AtomicUsize::new(0);
static STATE: AtomicI32 = AtomicI32::new(abi::state::INACTIVE);
static WARNED: AtomicBool = AtomicBool::new(false);

static API: FochPluginApi = FochPluginApi {
	size: std::mem::size_of::<FochPluginApi>() as u32,
	abi_major: abi::ABI_MAJOR,
	abi_minor: abi::ABI_MINOR,
	caps: 0,
	init,
	status,
	config_changed: None,
	set_enabled: None,
	shutdown: None,
};

pub fn api(major: u32) -> *const FochPluginApi {
	if major == abi::ABI_MAJOR {
		&API
	} else {
		std::ptr::null()
	}
}

unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	if host.is_null() || INITIALIZED.swap(true, Ordering::AcqRel) {
		return abi::result::E_INVALID;
	}
	HOST.store(host.cast_mut(), Ordering::Release);
	let host_ref = unsafe { &*host };
	if let Err(reason) = unsafe { profile::verify(host_ref) } {
		STATE.store(abi::state::REFUSED, Ordering::Release);
		log(abi::log::WARN, reason);
		return abi::result::OK;
	}
	BASE.store(host_ref.exe_base as usize, Ordering::Release);
	let redirects = [
		redirect(
			profile::SUMMARY_UPDATE,
			profile::SUMMARY.rva,
			summary as *const c_void,
		),
		redirect(
			profile::SUMMARY_SUGGEST,
			profile::SUMMARY.rva,
			summary as *const c_void,
		),
		redirect(
			profile::SUMMARY_SURRENDER,
			profile::SUMMARY.rva,
			summary as *const c_void,
		),
		redirect(
			profile::SEND_CALLBACK,
			profile::SEND.rva,
			send as *const c_void,
		),
	];
	match unsafe { bridge::install(host, &redirects) } {
		Ok(()) => {
			STATE.store(abi::state::ACTIVE, Ordering::Release);
			log(
				abi::log::INFO,
				"Bilateral peace summaries installed; mixed submission is unavailable",
			);
		}
		Err(error) => {
			STATE.store(abi::state::FAILED, Ordering::Release);
			log(abi::log::ERROR, &error.to_string());
		}
	}
	// The host retains the plugin even after a failed partial patch rollback.
	abi::result::OK
}

fn redirect(
	site: profile::CodeSite,
	target: usize,
	callback: *const c_void,
) -> bridge::Redirect<'static> {
	bridge::Redirect {
		site_rva: site.rva,
		expected_target_rva: target,
		original: site.original,
		callback,
	}
}

unsafe extern "C" fn status(out: *mut FochStatus) {
	if out.is_null() {
		return;
	}
	let state = STATE.load(Ordering::Acquire);
	let mut value = FochStatus::new(state);
	value.detail = FochStr::from_str(match state {
		abi::state::ACTIVE => "Bilateral summary preview; mixed submission is not implemented",
		abi::state::REFUSED => "Unsupported game executable; original peace editor retained",
		abi::state::FAILED => "Peace editor patch installation failed; see plugin log",
		_ => "Peace editor has not initialized",
	});
	unsafe { out.write(value) };
}

fn engine() -> Result<native::Engine, native::NativeError> {
	unsafe {
		native::Engine::new(
			BASE.load(Ordering::Acquire),
			native::NativeRvas {
				offer_ctor: profile::OFFER_CONSTRUCTOR.rva,
				offer_copy_ctor: profile::OFFER_COPY.rva,
				offer_dtor: profile::OFFER_DESTROY.rva,
				treaty_clone: profile::CLONE_CLAUSE.rva,
				vector_emplace: profile::VECTOR_APPEND.rva,
				rebuild_cost: profile::REBUILD_COST.rva,
			},
		)
	}
}

unsafe extern "system" fn summary(view: *mut u8) {
	let base = BASE.load(Ordering::Acquire);
	let original: unsafe extern "system" fn(*mut u8) =
		unsafe { std::mem::transmute(base + profile::SUMMARY.rva) };
	unsafe { original(view) };
	if view.is_null() {
		return;
	}
	let result = catch_unwind(AssertUnwindSafe(|| unsafe {
		let engine = engine().map_err(|_| "cannot bind the peace editor")?;
		let receive = engine
			.read_draft(view.add(0x48))
			.map_err(|_| "cannot read demands")?;
		let give = engine
			.read_draft(view.add(0x208))
			.map_err(|_| "cannot read concessions")?;
		composition::compose(&receive, &give).map_err(|_| "unrelated peace drafts")?;
		ui::windows::update_bilateral_summary(base, view)
	}));
	let reason = match result {
		Ok(Ok(())) => return,
		Ok(Err(reason)) => reason,
		Err(_) => "panic while updating the bilateral peace preview",
	};
	if !WARNED.swap(true, Ordering::AcqRel) {
		log(abi::log::WARN, reason);
	}
}

unsafe extern "system" fn send(view: *mut u8) {
	let base = BASE.load(Ordering::Acquire);
	let original: unsafe extern "system" fn(*mut u8) =
		unsafe { std::mem::transmute(base + profile::SEND.rva) };
	if view.is_null() {
		return;
	}
	let state = catch_unwind(AssertUnwindSafe(|| unsafe {
		let engine = engine().map_err(|_| ())?;
		let receive = engine.read_draft(view.add(0x48)).map_err(|_| ())?;
		let give = engine.read_draft(view.add(0x208)).map_err(|_| ())?;
		composition::compose(&receive, &give).map_err(|_| ())?;
		Ok::<_, ()>(!receive.selected.is_empty() && !give.selected.is_empty())
	}));
	match state {
		Ok(Ok(false)) => unsafe { original(view) },
		Ok(Ok(true)) => {
			if let Some(text) = unsafe { ui::windows::find_text(view, c"desc") } {
				unsafe {
					ui::windows::set_text(
						base, text,
						c"Mixed submission is not available in this preview.\nBoth drafts have been kept. Clear one side to send a standard peace offer.",
					);
				}
			}
			log(
				abi::log::WARN,
				"Mixed peace submission refused; both drafts retained",
			);
		}
		_ => log(
			abi::log::ERROR,
			"Peace draft validation failed; submission refused",
		),
	}
}

fn log(level: i32, message: &str) {
	let host = HOST.load(Ordering::Acquire);
	if !host.is_null() {
		unsafe { ((*host).log)(host, level, FochStr::from_str(message)) };
	}
}
