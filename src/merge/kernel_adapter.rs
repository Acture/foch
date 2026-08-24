use crate::game::eu4::script::parser::AstFile;

pub use crate::model::{MergeBackendDescriptor, MergeBackendId};

#[derive(Clone, Debug)]
pub(crate) struct KernelRevision {
	pub source_id: String,
	pub precedence: usize,
	pub ast: AstFile,
}

#[derive(Clone, Debug)]
pub(crate) struct KernelMergeInput {
	pub base: AstFile,
	pub revisions: Vec<KernelRevision>,
}

impl KernelMergeInput {
	pub fn new(base: AstFile, mut revisions: Vec<KernelRevision>) -> Self {
		revisions.sort_by(|left, right| {
			left.precedence
				.cmp(&right.precedence)
				.then_with(|| left.source_id.cmp(&right.source_id))
		});
		Self { base, revisions }
	}
}
