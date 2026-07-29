use foch_language::analyzer::parser::AstFile;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MergeKernelMode {
	/// Patch-address engine retained only as a merge-quality baseline.
	Legacy,
	/// Production tree-state engine. The external `structured` label remains
	/// stable in shadow-report schemas while the internal architecture migrates.
	#[default]
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MergeEvaluationKernel {
	/// Address-patch implementation retained as a historical quality reference.
	AddressPatchReference,
	/// Production semantic-tree implementation.
	#[default]
	SemanticTree,
}

impl MergeEvaluationKernel {
	/// Stable labels used by existing merge-quality artifacts.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::AddressPatchReference => "legacy",
			Self::SemanticTree => "structured",
		}
	}
}

impl From<MergeEvaluationKernel> for MergeKernelMode {
	fn from(value: MergeEvaluationKernel) -> Self {
		match value {
			MergeEvaluationKernel::AddressPatchReference => Self::Legacy,
			MergeEvaluationKernel::SemanticTree => Self::Structured,
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
}
