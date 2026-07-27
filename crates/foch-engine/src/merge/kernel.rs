use foch_language::analyzer::parser::AstFile;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MergeKernelMode {
	#[default]
	Legacy,
	Structured,
}

impl MergeKernelMode {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Legacy => "legacy",
			Self::Structured => "structured",
		}
	}
}

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

	pub fn exactly_two_revisions(&self) -> Result<[&KernelRevision; 2], String> {
		match self.revisions.as_slice() {
			[left, right] => Ok([left, right]),
			_ => Err(format!(
				"expected exactly two revisions for {}, found {}",
				self.base.path.display(),
				self.revisions.len()
			)),
		}
	}
}
