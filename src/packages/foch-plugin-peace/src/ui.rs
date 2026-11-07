use std::ffi::CString;

const MAX_SUMMARY: usize = 64 * 1024;
const RECEIVE_HEADING: &[u8] = b"PREVIEW: mixed submission is unavailable.\n\nYOU RECEIVE (+)\n";
const GIVE_HEADING: &[u8] = b"\n\nYOU GIVE (-)\n";

/// The text remains in EU4's current encoding, including native colour escapes.
pub fn agreement_summary(receive: &[u8], give: &[u8]) -> Result<CString, &'static str> {
	let length = receive
		.len()
		.checked_add(give.len())
		.and_then(|n| n.checked_add(RECEIVE_HEADING.len()))
		.and_then(|n| n.checked_add(GIVE_HEADING.len()))
		.filter(|&n| n <= MAX_SUMMARY)
		.ok_or("peace summary exceeds the native display limit")?;
	let mut text = Vec::with_capacity(length);
	text.extend_from_slice(RECEIVE_HEADING);
	text.extend_from_slice(receive);
	text.extend_from_slice(GIVE_HEADING);
	text.extend_from_slice(give);
	CString::new(text).map_err(|_| "native peace summary contains an embedded NUL")
}

#[cfg(windows)]
pub(crate) mod windows {
	use super::*;
	use core::ffi::{c_char, c_void};
	use std::ptr;

	const VIEW_ROOT: usize = 0x38;
	const VIEW_DIRECTION: usize = 0x660;
	const TEXT_STRING: usize = 0x170;

	/// All pointers must belong to the verified native game build and current UI thread.
	pub unsafe fn find_text(view: *mut u8, name: &std::ffi::CStr) -> Option<*mut u8> {
		let root = unsafe { view.add(VIEW_ROOT).cast::<*mut u8>().read_unaligned() };
		if root.is_null() {
			return None;
		}
		let vtable = unsafe { root.cast::<*const usize>().read_unaligned() };
		let address = unsafe { vtable.add(0x98 / 8).read() };
		let find: unsafe extern "system" fn(*mut u8, *const c_char) -> *mut u8 =
			unsafe { std::mem::transmute(address) };
		let text = unsafe { find(root, name.as_ptr()) };
		(!text.is_null()).then_some(text)
	}

	/// Copies an MSVC x64 std::string without transferring its allocation.
	pub unsafe fn read_text(text: *mut u8) -> Result<Vec<u8>, &'static str> {
		let string = unsafe { text.add(TEXT_STRING) };
		let len = unsafe { string.add(16).cast::<usize>().read_unaligned() };
		let capacity = unsafe { string.add(24).cast::<usize>().read_unaligned() };
		if len > capacity || len > MAX_SUMMARY || (capacity < 16 && capacity != 15) {
			return Err("invalid native peace text");
		}
		let data = if capacity < 16 {
			string
		} else {
			unsafe { string.cast::<*mut u8>().read_unaligned() }
		};
		if data.is_null() && len != 0 {
			return Err("missing native peace text allocation");
		}
		if len == 0 {
			return Ok(Vec::new());
		}
		Ok(unsafe { std::slice::from_raw_parts(data, len) }.to_vec())
	}

	/// Uses the engine's copying C-string setter. Its std::string setter moves
	/// ownership and must never receive a Rust-owned allocation.
	pub unsafe fn set_text(base: usize, text: *mut u8, value: &std::ffi::CStr) {
		let setter: unsafe extern "system" fn(*mut u8, *const c_char) =
			unsafe { std::mem::transmute(base + 0x152aea0) };
		unsafe { setter(text, value.as_ptr()) };
	}

	pub unsafe fn update_bilateral_summary(base: usize, view: *mut u8) -> Result<(), &'static str> {
		let summary: unsafe extern "system" fn(*mut c_void) =
			unsafe { std::mem::transmute(base + 0x130a5c0) };
		let desc = unsafe { find_text(view, c"desc") }.ok_or("peace description is absent")?;
		let active = unsafe { view.add(VIEW_DIRECTION).read() };
		let active_text = unsafe { read_text(desc) }?;
		unsafe {
			ptr::write(view.add(VIEW_DIRECTION), u8::from(active == 0));
			summary(view.cast());
		}
		let opposite = unsafe { read_text(desc) };
		// Restore the active picker, its costs and its native widget state.
		unsafe {
			ptr::write(view.add(VIEW_DIRECTION), active);
			summary(view.cast());
		}
		let opposite = opposite?;
		let (receive, give) = if active != 0 {
			(active_text, opposite)
		} else {
			(opposite, active_text)
		};
		let combined = agreement_summary(&receive, &give)?;
		unsafe { set_text(base, desc, &combined) };
		for (name, title) in [
			(c"demand_tab_title", c"Receive (+)"),
			(c"offer_tab_title", c"Give (-)"),
		] {
			if let Some(text) = unsafe { find_text(view, name) } {
				unsafe { set_text(base, text, title) };
			}
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keeps_native_encoding_and_each_direction_in_one_summary() {
		let text = agreement_summary(b"\xa7YGenoa\xa7!", b"\xa7Y100 ducats\xa7!").unwrap();
		assert_eq!(
			text.to_bytes(),
			b"PREVIEW: mixed submission is unavailable.\n\nYOU RECEIVE (+)\n\xa7YGenoa\xa7!\n\nYOU GIVE (-)\n\xa7Y100 ducats\xa7!"
		);
	}

	#[test]
	fn does_not_silently_truncate_a_large_agreement() {
		assert!(agreement_summary(&vec![b'x'; MAX_SUMMARY], b"x").is_err());
	}

	#[test]
	fn display_limit_counts_headings_and_the_preview_warning() {
		let payload_length = MAX_SUMMARY - RECEIVE_HEADING.len() - GIVE_HEADING.len();
		let payload = vec![b'x'; payload_length];
		assert_eq!(
			agreement_summary(&payload, b"").unwrap().to_bytes().len(),
			MAX_SUMMARY
		);
		assert!(agreement_summary(&payload, b"x").is_err());
	}

	#[test]
	fn rejects_nul_that_would_hide_the_rest_of_the_agreement() {
		assert!(agreement_summary(b"one\0two", b"three").is_err());
	}
}
