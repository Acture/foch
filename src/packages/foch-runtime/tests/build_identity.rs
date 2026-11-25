use foch_runtime::dataplane::{pinned_exe_sha256, supports_build};

/// The unmodified Steam 1.37.5 build embedded next to the pinned AMDAC one.
const STOCK_STEAM: &str = "9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a";

#[test]
fn shared_layout_requires_the_complete_host_computed_digest() {
	let expected = pinned_exe_sha256();
	assert!(supports_build(expected));
	for digest in [
		"",
		"1.37.5",
		"c9ec55a4",
		&"0".repeat(64),
		&expected.to_ascii_uppercase(),
	] {
		assert!(!supports_build(digest), "{digest}");
	}
	let mut wrong = expected.to_string();
	wrong.pop();
	wrong.push('0');
	assert!(!supports_build(&wrong));
}

#[test]
fn both_embedded_1375_builds_pass_the_shared_digest_gate() {
	let pinned = pinned_exe_sha256();
	assert_ne!(pinned, STOCK_STEAM);
	for digest in [pinned, STOCK_STEAM] {
		assert!(supports_build(digest), "{digest}");
	}
	// Arbitrary or malformed digests — including mutations of the stock
	// digest — stay rejected: exact bytes, never a prefix or case variant.
	for digest in [
		"",
		"9ad3efe1",
		&"9".repeat(64),
		&STOCK_STEAM.to_ascii_uppercase(),
		&format!("{STOCK_STEAM}0"),
	] {
		assert!(!supports_build(digest), "{digest}");
	}
	let mut wrong = STOCK_STEAM.to_string();
	wrong.pop();
	wrong.push('0');
	assert!(!supports_build(&wrong));
}
