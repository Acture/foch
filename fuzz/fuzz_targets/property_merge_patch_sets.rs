#![no_main]

mod common;

use arbitrary::{Arbitrary, Unstructured};
use foch_engine::merge::DeferHandler;
use foch_engine::merge::patch::diff_ast;
use foch_engine::merge::patch_merge::merge_patch_sets;
use foch_language::analyzer::content_family::MergeKeySource;
use libfuzzer_sys::fuzz_target;

#[derive(Debug)]
struct MergeInput<'a> {
	mod_a: &'a [u8],
	mod_b: &'a [u8],
	mod_c: &'a [u8],
}

impl<'a> Arbitrary<'a> for MergeInput<'a> {
	fn arbitrary(unstructured: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
		let rest = unstructured.bytes(unstructured.len())?;
		let (mod_a, mod_b, mod_c) = common::split_three(rest);
		Ok(Self {
			mod_a,
			mod_b,
			mod_c,
		})
	}
}

fuzz_target!(|data: &[u8]| {
	let mut unstructured = Unstructured::new(data);
	let Ok(input) = MergeInput::arbitrary(&mut unstructured) else {
		return;
	};
	let base = common::fixed_base();
	let mut expected_total = 0;
	let mut mod_patches = Vec::new();
	for (index, (mod_id, bytes)) in [
		("mod_a", input.mod_a),
		("mod_b", input.mod_b),
		("mod_c", input.mod_c),
	]
	.into_iter()
	.enumerate()
	{
		let patches = common::parsed_script_from_bytes(
			mod_id,
			&format!("common/property_merge_{mod_id}.txt"),
			bytes,
			true,
		)
		.map(|overlay| diff_ast(&base, &overlay, MergeKeySource::AssignmentKey))
		.unwrap_or_default();
		expected_total += patches.len();
		mod_patches.push((mod_id.to_string(), index, patches));
	}
	let mut handler = DeferHandler;
	let result = merge_patch_sets(mod_patches, &common::default_policies(), &mut handler)
		.expect("defer handler should not abort");
	assert_eq!(result.stats.total_patches, expected_total);
});
