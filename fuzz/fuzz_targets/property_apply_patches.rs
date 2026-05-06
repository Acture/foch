#![no_main]

mod common;

use arbitrary::{Arbitrary, Unstructured};
use foch_engine::merge::patch_apply::apply_patches;
use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::AstStatement;
use libfuzzer_sys::fuzz_target;

#[derive(Debug)]
struct ApplyInput<'a> {
	base: &'a [u8],
}

impl<'a> Arbitrary<'a> for ApplyInput<'a> {
	fn arbitrary(unstructured: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
		Ok(Self {
			base: unstructured.bytes(unstructured.len())?,
		})
	}
}

fuzz_target!(|data: &[u8]| {
	let mut unstructured = Unstructured::new(data);
	let Ok(input) = ApplyInput::arbitrary(&mut unstructured) else {
		return;
	};
	let Some(base) = common::parsed_script_from_bytes(
		"base",
		"common/property_apply_base.txt",
		input.base,
		false,
	) else {
		return;
	};
	let patches = common::fixed_patches();
	let _merged: Vec<AstStatement> = apply_patches(
		&base.ast.statements,
		&patches,
		MergeKeySource::AssignmentKey,
	);
});
