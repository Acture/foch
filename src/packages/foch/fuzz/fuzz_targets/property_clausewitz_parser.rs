#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
	if bytes.len() > common::MAX_SCRIPT_BYTES {
		return;
	}
	let parsed = common::parse_clausewitz_file_from_bytes("common/property_parser.txt", bytes);
	let diagnostic_bound = bytes.len().saturating_mul(8).saturating_add(128);
	assert!(parsed.diagnostics.len() <= diagnostic_bound);
});
