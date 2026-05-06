#![no_main]

use foch_core::decode_paradox_bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
	let decoded = decode_paradox_bytes(bytes);
	assert!(std::str::from_utf8(decoded.as_bytes()).is_ok());
});
