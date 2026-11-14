//! The economy-update detour riding the game thread.
//!
//! The spike pinned the hook target to the economy update that ends by
//! recalculating every trade node (see [`crate::offsets::Sigs::tick`]):
//! `fn(context: &GameContext, flag: u8)`. The detour always calls the
//! original first, then runs one step; the trade database is the context
//! object at a fixed offset, cross-checked against the context global
//! before use. The host chains further detours through the same cell.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::offsets::layout;

/// The host-owned next cell, installed once by `init` when the hook is
/// created. Re-read on every call: the host relinks the chain under us.
static NEXT_CELL: AtomicUsize = AtomicUsize::new(0);

/// Whether the monthly logic still runs. A panic inside a step disables
/// further steps for this process; the original call continues.
static RUN: AtomicBool = AtomicBool::new(false);

pub fn install_next_cell(cell: *mut *const *const c_void) {
	NEXT_CELL.store(cell as usize, Ordering::Release);
	RUN.store(true, Ordering::Release);
}

fn original(context: *mut c_void, flag: u8) {
	let cell = NEXT_CELL.load(Ordering::Acquire) as *mut *const c_void;
	if cell.is_null() {
		// Unreachable while installed: the host owns the cell for the
		// process lifetime once hook_create succeeded.
		return;
	}
	// SAFETY: the cell holds the host-owned pointer to the original (or
	// the next detour); transmuting the fn pointer matches the ABI.
	let next: unsafe extern "C" fn(*mut c_void, u8) = unsafe { std::mem::transmute(*cell) };
	unsafe { next(context, flag) }
}

/// The detour the host chains onto the economy update.
///
/// # Safety
///
/// Must only be installed on the pinned economy update, whose first
/// argument is the global game context; the host enforces the chain.
pub unsafe extern "C" fn economy_update_detour(context: *mut c_void, flag: u8) {
	original(context, flag);
	if RUN.load(Ordering::Acquire) {
		let db = context as usize + layout::DB_IN_CONTEXT;
		let step = move || crate::runtime::monthly(context as usize, db);
		if std::panic::catch_unwind(AssertUnwindSafe(step)).is_err() {
			RUN.store(false, Ordering::Release);
			crate::state::set_failed_after_panic();
			crate::logging::write(
				foch_plugin_abi::log::ERROR,
				"economy-update step panicked; disabled for this process",
			);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::Ordering;

	#[test]
	fn a_detour_without_an_installed_cell_is_inert() {
		// The module statics start empty; calling through `original`
		// directly is not exercised because that path is unreachable while
		// the hook is installed. This test pins the disabled default.
		assert!(!super::RUN.load(Ordering::Acquire));
		assert_eq!(super::NEXT_CELL.load(Ordering::Acquire), 0);
	}
}
