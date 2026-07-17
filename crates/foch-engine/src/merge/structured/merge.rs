use foch_language::analyzer::parser::AstFile;
use foch_merge_kernel::{MergeOutcome, StructuralConflict, three_way_merge};

use super::ast_adapter::{AstAdapterError, denormalize_ast, normalize_ast};
use super::policy::EventTreePolicy;

#[derive(Clone, Debug)]
pub(crate) struct ClausewitzMergeOutcome {
	tentative_ast: AstFile,
	kernel: MergeOutcome,
}

impl ClausewitzMergeOutcome {
	pub(crate) fn conflicts(&self) -> &[StructuralConflict] {
		&self.kernel.conflicts
	}

	pub(crate) fn resolved_ast(&self) -> Option<&AstFile> {
		self.kernel
			.conflicts
			.is_empty()
			.then_some(&self.tentative_ast)
	}

	#[cfg(test)]
	pub(crate) fn tentative_ast(&self) -> &AstFile {
		&self.tentative_ast
	}

	#[cfg(test)]
	pub(crate) fn kernel(&self) -> &MergeOutcome {
		&self.kernel
	}
}

pub(crate) fn merge_event_files(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
) -> Result<ClausewitzMergeOutcome, AstAdapterError> {
	let policy = EventTreePolicy;
	let base_tree = normalize_ast(base, &policy)?;
	let left_tree = normalize_ast(left, &policy)?;
	let right_tree = normalize_ast(right, &policy)?;
	let kernel = three_way_merge(&base_tree, &left_tree, &right_tree);
	let tentative_ast = denormalize_ast(base.path.clone(), kernel.tentative_tree())?;
	Ok(ClausewitzMergeOutcome {
		tentative_ast,
		kernel,
	})
}
