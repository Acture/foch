use foch_runtime::dataplane::{pinned_exe_sha256, supports_build};

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
