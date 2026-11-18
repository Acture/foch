use foch_runtime::dataplane::{layout::EXPECTED_EXE_SHA256, supports_build};

#[test]
fn shared_layout_requires_the_complete_host_computed_digest() {
	assert!(supports_build(EXPECTED_EXE_SHA256));
	for digest in [
		"",
		"1.37.5",
		"c9ec55a4",
		&"0".repeat(64),
		&EXPECTED_EXE_SHA256.to_ascii_uppercase(),
	] {
		assert!(!supports_build(digest), "{digest}");
	}
	let mut wrong = EXPECTED_EXE_SHA256.to_string();
	wrong.pop();
	wrong.push('0');
	assert!(!supports_build(&wrong));
}
