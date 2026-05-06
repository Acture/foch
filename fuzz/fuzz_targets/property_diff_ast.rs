#![no_main]

mod common;

use arbitrary::{Arbitrary, Unstructured};
use foch_engine::merge::patch::{ClausewitzPatch, diff_ast};
use foch_language::analyzer::content_family::MergeKeySource;
use libfuzzer_sys::fuzz_target;

#[derive(Debug)]
struct ScriptPair<'a> {
	base: &'a [u8],
	overlay: &'a [u8],
}

impl<'a> Arbitrary<'a> for ScriptPair<'a> {
	fn arbitrary(unstructured: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
		let rest = unstructured.bytes(unstructured.len())?;
		let (base, overlay) = common::split_pair(rest);
		Ok(Self { base, overlay })
	}
}

fuzz_target!(|data: &[u8]| {
	let mut unstructured = Unstructured::new(data);
	let Ok(input) = ScriptPair::arbitrary(&mut unstructured) else {
		return;
	};
	let Some(base) =
		common::parsed_script_from_bytes("base", "common/property_diff_base.txt", input.base, true)
	else {
		return;
	};
	let Some(overlay) = common::parsed_script_from_bytes(
		"overlay",
		"common/property_diff_overlay.txt",
		input.overlay,
		true,
	) else {
		return;
	};
	let _patches: Vec<ClausewitzPatch> = diff_ast(&base, &overlay, MergeKeySource::AssignmentKey);
});
