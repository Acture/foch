use crate::native;
use std::ffi::CString;

const MAX_SUMMARY: usize = 64 * 1024;
/// The view's active-direction byte: non-zero selects the demand draft.
pub const VIEW_DIRECTION: usize = 0x660;
const RECEIVE_HEADING: &[u8] = b"YOU RECEIVE (+)\n";
const GIVE_HEADING: &[u8] = b"\n\nYOU GIVE (-)\n";
/// The desc box scrolls; the net line leads so the merged deal value is
/// visible without scrolling.
const NET_HEADING: &[u8] = b"NET DEAL: ";
const NET_TAIL: &[u8] = b"\n\n";
/// Appended to the demand tab's existing localized title.
pub const DEMAND_TAB_MARKER: &[u8] = b" (+)";
/// Appended to the offer tab's existing localized title.
pub const OFFER_TAB_MARKER: &[u8] = b" (-)";

/// Renders a cached per-mille warscore value as a signed percent string.
pub fn permille_percent(value: i32) -> String {
	let sign = if value < 0 { "-" } else { "+" };
	format!("{sign}{}", permille_magnitude(value))
}

/// Renders only the magnitude, so a direction slot can carry its own sign.
fn permille_magnitude(value: i32) -> String {
	let absolute = value.unsigned_abs();
	format!("{}.{:01}%", absolute / 1000, absolute % 1000 / 100)
}

/// The text remains in EU4's current encoding, including native colour escapes.
/// `receive_score`/`give_score` are the two drafts' cached per-mille totals;
/// the appended net line is the deal value the player's warscore must cover.
pub fn agreement_summary(
	receive: &[u8],
	give: &[u8],
	receive_score: i32,
	give_score: i32,
) -> Result<CString, &'static str> {
	let net_line = format!(
		"{} -{} = {}",
		permille_percent(receive_score),
		permille_magnitude(give_score),
		permille_percent(receive_score - give_score)
	);
	let length = receive
		.len()
		.checked_add(give.len())
		.and_then(|n| n.checked_add(RECEIVE_HEADING.len()))
		.and_then(|n| n.checked_add(GIVE_HEADING.len()))
		.and_then(|n| n.checked_add(NET_HEADING.len()))
		.and_then(|n| n.checked_add(NET_TAIL.len()))
		.and_then(|n| n.checked_add(net_line.len()))
		.filter(|&n| n <= MAX_SUMMARY)
		.ok_or("peace summary exceeds the native display limit")?;
	let mut text = Vec::with_capacity(length);
	text.extend_from_slice(NET_HEADING);
	text.extend_from_slice(net_line.as_bytes());
	text.extend_from_slice(NET_TAIL);
	text.extend_from_slice(RECEIVE_HEADING);
	text.extend_from_slice(receive);
	text.extend_from_slice(GIVE_HEADING);
	text.extend_from_slice(give);
	CString::new(text).map_err(|_| "native peace summary contains an embedded NUL")
}

/// Keeps the widget's current title — the game's localization, or a UI mod's
/// replacement — and appends one direction marker. A title that already ends
/// with that marker is unchanged, so a summary refresh does not stack suffixes.
pub fn annotated_tab_title(original: &[u8], marker: &[u8]) -> Result<CString, &'static str> {
	if marker != DEMAND_TAB_MARKER && marker != OFFER_TAB_MARKER {
		return Err("unknown peace tab marker");
	}
	let body = original.strip_suffix(marker).unwrap_or(original);
	if body.len().saturating_add(marker.len()) > 256 {
		return Err("localized tab title is too long");
	}
	let mut text = Vec::with_capacity(body.len() + marker.len());
	text.extend_from_slice(body);
	text.extend_from_slice(marker);
	CString::new(text).map_err(|_| "localized tab title contains an embedded NUL")
}

#[cfg(windows)]
pub(crate) mod windows {
	use super::*;
	use crate::profile;
	use core::ffi::{c_char, c_void};
	use foch_runtime::dataplane::Memory;

	const VIEW_ROOT: usize = 0x38;
	const TEXT_STRING: usize = 0x170;

	/// All pointers must belong to the verified native game build and current UI thread.
	pub unsafe fn find_text(view: *mut u8, name: &std::ffi::CStr) -> Option<*mut u8> {
		let memory = Memory;
		let root = memory.read_ptr((view as usize).checked_add(VIEW_ROOT)?)?;
		let vtable = memory.read_ptr(root)?;
		let address = memory.read_ptr(vtable.checked_add(0x98)?)?;
		let find: unsafe extern "system" fn(*mut u8, *const c_char) -> *mut u8 =
			unsafe { std::mem::transmute(address) };
		let text = unsafe { find(root as *mut u8, name.as_ptr()) };
		(!text.is_null()).then_some(text)
	}

	/// Copies an MSVC x64 std::string without transferring its allocation,
	/// through the shared SSO reader. The game's peace text can contain any
	/// byte, including colour escapes, so every byte is accepted.
	pub unsafe fn read_text(text: *mut u8) -> Result<Vec<u8>, &'static str> {
		let memory = Memory;
		let string = (text as usize)
			.checked_add(TEXT_STRING)
			.ok_or("invalid native peace text")?;
		// The shared read_sso caps at 64 bytes; the peace summary can be
		// longer, so read the header and bulk-read the payload instead.
		let len = memory
			.read_qword(string.checked_add(16).ok_or("invalid native peace text")?)
			.ok_or("unreadable native peace text")? as usize;
		let capacity = memory
			.read_qword(string.checked_add(24).ok_or("invalid native peace text")?)
			.ok_or("unreadable native peace text")? as usize;
		if len > capacity || len > MAX_SUMMARY || (capacity < 16 && capacity != 15) {
			return Err("invalid native peace text");
		}
		if len == 0 {
			return Ok(Vec::new());
		}
		let data = if capacity < 16 {
			string
		} else {
			memory
				.read_ptr(string)
				.ok_or("missing native peace text allocation")?
		};
		let mut bytes = vec![0u8; len];
		memory
			.read_bytes(data, &mut bytes)
			.ok_or("unreadable native peace text")?;
		Ok(bytes)
	}

	/// Uses the engine's copying C-string setter. Its std::string setter moves
	/// ownership and must never receive a Rust-owned allocation.
	pub unsafe fn set_text(base: usize, text: *mut u8, value: &std::ffi::CStr) {
		let setter: unsafe extern "system" fn(*mut u8, *const c_char) =
			unsafe { std::mem::transmute(base + profile::SET_TEXT.rva) };
		unsafe { setter(text, value.as_ptr()) };
	}

	/// `receive_score`/`give_score` must be freshly rebuilt one-direction
	/// totals: this hook overwrites the active draft's cached total with the
	/// net value, so a stale or already-netted value would compound.
	pub unsafe fn update_bilateral_summary(
		base: usize,
		view: *mut u8,
		receive_score: i32,
		give_score: i32,
	) -> Result<(), &'static str> {
		let memory = Memory;
		let view_addr = view as usize;
		let direction_addr = view_addr
			.checked_add(VIEW_DIRECTION)
			.ok_or("invalid peace view")?;
		let summary: unsafe extern "system" fn(*mut c_void) =
			unsafe { std::mem::transmute(base + profile::SUMMARY.rva) };
		let desc = unsafe { find_text(view, c"desc") }.ok_or("peace description is absent")?;
		let active = memory
			.read_u8(direction_addr)
			.ok_or("cannot read active peace direction")?;
		let active_text = unsafe { read_text(desc) }?;
		unsafe {
			memory
				.write_bytes(direction_addr, &[u8::from(active == 0)])
				.ok_or("cannot toggle peace direction")?;
			summary(view.cast());
		}
		let opposite = unsafe { read_text(desc) };
		// Restore the active picker, its costs and its native widget state.
		unsafe {
			let _ = memory.write_bytes(direction_addr, &[active]);
			summary(view.cast());
		}
		let opposite = opposite?;
		let (receive, give) = if active != 0 {
			(active_text, opposite)
		} else {
			(opposite, active_text)
		};
		// Keep the native cost display (bottom-right) on the merged deal's net
		// value while both drafts have terms. The caller supplies freshly
		// rebuilt one-direction totals; the active draft is what the display
		// reads, and a single-direction selection keeps its own value.
		if receive_score != 0 && give_score != 0 {
			let net = receive_score.saturating_sub(give_score);
			let active_offset = if active != 0 {
				crate::settlement::RECEIVE_OFFER
			} else {
				crate::settlement::GIVE_OFFER
			};
			let score_addr = view_addr
				.checked_add(active_offset + native::SCORE_TOTAL_OFFSET)
				.ok_or("invalid peace offer score offset")?;
			unsafe {
				memory
					.write_i32(score_addr, net)
					.ok_or("cannot update peace offer score")?;
			}
			// "offer_score" is the deal-cost readout beside the send button —
			// the number the player watches. Show the merged deal's net value
			// there directly. The top "warscore" widget stays native: it
			// shows the war's progress, not the offer's cost.
			if let Some(score) = unsafe { find_text(view, c"offer_score") } {
				let text = std::ffi::CString::new(format!("{}%", net / 1000))
					.map_err(|_| "native peace summary contains an embedded NUL")?;
				unsafe { set_text(base, score, &text) };
			}
		}
		let combined = agreement_summary(&receive, &give, receive_score, give_score)?;
		unsafe { set_text(base, desc, &combined) };
		for (name, marker) in [
			(c"demand_tab_title", DEMAND_TAB_MARKER),
			(c"offer_tab_title", OFFER_TAB_MARKER),
		] {
			if let Some(text) = unsafe { find_text(view, name) } {
				let original = unsafe { read_text(text) }?;
				let title = annotated_tab_title(&original, marker)?;
				unsafe { set_text(base, text, &title) };
			}
		}
		Ok(())
	}

	/// One-shot inventory of the window's text widgets: name, text length and
	/// leading bytes of every candidate that exists. The goal is a merged,
	/// single-list agreement body, and picking the widget that actually
	/// renders it needs this evidence, not guesses.
	pub static PROBED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

	pub unsafe fn probe_text_widgets<F: Fn(&str)>(base: usize, view: *mut u8, report: F) {
		const CANDIDATES: &[&std::ffi::CStr] = &[
			c"desc",
			c"body",
			c"summary",
			c"agreement",
			c"terms",
			c"peace_desc",
			c"description",
			c"treaty_desc",
			c"demand_list",
			c"offer_list",
			c"demand_tab_title",
			c"offer_tab_title",
			c"cost",
			c"warscore",
			c"total_cost",
			c"score",
		];
		for name in CANDIDATES {
			let Some(widget) = (unsafe { find_text(view, name) }) else {
				continue;
			};
			let text = unsafe { read_text(widget) }.unwrap_or_default();
			let preview: String = text
				.iter()
				.take(60)
				.map(|byte| {
					if byte.is_ascii_graphic() || *byte == b' ' {
						*byte as char
					} else {
						'.'
					}
				})
				.collect();
			report(&format!(
				"widget {name:?}: {} bytes, starts {preview:?}",
				text.len()
			));
		}
		let _ = base;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keeps_native_encoding_and_each_direction_in_one_summary() {
		let text =
			agreement_summary(b"\xa7YGenoa\xa7!", b"\xa7Y100 ducats\xa7!", 12_300, 4_500).unwrap();
		assert_eq!(
			text.to_bytes(),
			b"NET DEAL: +12.3% -4.5% = +7.8%\n\nYOU RECEIVE (+)\n\xa7YGenoa\xa7!\n\nYOU GIVE (-)\n\xa7Y100 ducats\xa7!"
		);
	}

	#[test]
	fn net_line_subtracts_the_given_side_and_keeps_the_player_sign() {
		let text = agreement_summary(b"a", b"b", 3_000, 9_000).unwrap();
		assert!(
			text.to_bytes()
				.starts_with(b"NET DEAL: +3.0% -9.0% = -6.0%")
		);
		let even = agreement_summary(b"a", b"b", 0, 0).unwrap();
		assert!(
			even.to_bytes()
				.starts_with(b"NET DEAL: +0.0% -0.0% = +0.0%")
		);
	}

	#[test]
	fn permille_percent_renders_one_decimal_and_signs() {
		assert_eq!(permille_percent(0), "+0.0%");
		assert_eq!(permille_percent(999), "+0.9%");
		assert_eq!(permille_percent(12_345), "+12.3%");
		assert_eq!(permille_percent(-750), "-0.7%");
		assert_eq!(permille_percent(i32::MIN), "-2147483.6%");
	}

	#[test]
	fn does_not_silently_truncate_a_large_agreement() {
		assert!(agreement_summary(&vec![b'x'; MAX_SUMMARY], b"x", 1, 1).is_err());
	}

	#[test]
	fn display_limit_counts_headings_and_the_net_line() {
		let net_line = format!(
			"{} -{} = {}",
			permille_percent(12_300),
			permille_magnitude(4_500),
			permille_percent(7_800)
		);
		let payload_length = MAX_SUMMARY
			- RECEIVE_HEADING.len()
			- GIVE_HEADING.len()
			- NET_HEADING.len()
			- NET_TAIL.len()
			- net_line.len();
		let payload = vec![b'x'; payload_length];
		assert_eq!(
			agreement_summary(&payload, b"", 12_300, 4_500)
				.unwrap()
				.to_bytes()
				.len(),
			MAX_SUMMARY
		);
		assert!(agreement_summary(&payload, b"x", 12_300, 4_500).is_err());
	}

	#[test]
	fn rejects_nul_that_would_hide_the_rest_of_the_agreement() {
		assert!(agreement_summary(b"one\0two", b"three", 0, 0).is_err());
	}

	#[test]
	fn tab_titles_keep_the_localized_text_and_append_a_direction_marker() {
		let demand = annotated_tab_title("需求".as_bytes(), DEMAND_TAB_MARKER).unwrap();
		let offer = annotated_tab_title(b"\xa7Y\xc8\xce\xa7!", OFFER_TAB_MARKER).unwrap();
		assert_eq!(demand.to_bytes(), "需求 (+)".as_bytes());
		assert_eq!(offer.to_bytes(), b"\xa7Y\xc8\xce\xa7! (-)");
		assert_eq!(
			annotated_tab_title(demand.to_bytes(), DEMAND_TAB_MARKER)
				.unwrap()
				.to_bytes(),
			demand.to_bytes()
		);
		let custom = annotated_tab_title(b"Spoils", DEMAND_TAB_MARKER).unwrap();
		assert_eq!(custom.to_bytes(), b"Spoils (+)");
		assert!(
			!custom
				.to_bytes()
				.windows(7)
				.any(|bytes| bytes == b"Receive")
		);
		assert!(!offer.to_bytes().windows(4).any(|bytes| bytes == b"Give"));
	}

	#[test]
	fn tab_titles_reject_a_nul_and_an_oversized_localization() {
		assert!(annotated_tab_title(b"one\0two", DEMAND_TAB_MARKER).is_err());
		assert!(annotated_tab_title(&vec![b'x'; 253], OFFER_TAB_MARKER).is_err());
		assert!(annotated_tab_title(b"Demand", b" (?)").is_err());
		assert_eq!(
			annotated_tab_title(b"", DEMAND_TAB_MARKER)
				.unwrap()
				.to_bytes(),
			b" (+)"
		);
	}
}
