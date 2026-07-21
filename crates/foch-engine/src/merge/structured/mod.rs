mod ast_adapter;
mod merge;
mod policy;

pub use ast_adapter::AstAdapterError;
pub(crate) use merge::merge_event_files;
pub use merge::{
	ClausewitzConflictSummary, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	merge_clausewitz_files,
};

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

#[cfg(test)]
mod tests;
