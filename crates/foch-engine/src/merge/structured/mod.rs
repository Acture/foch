mod ast_adapter;
mod control_flow;
mod definition_module;
mod merge;
mod policy;
mod trivia;

pub use ast_adapter::AstAdapterError;
pub use definition_module::{
	ClausewitzDefinitionModuleOutcome, merge_clausewitz_definition_module,
};
pub(crate) use merge::merge_event_files;
pub use merge::{
	ClausewitzConflictSummary, ClausewitzMergeOutcome, ClausewitzMergeTimings,
	ClausewitzScalarReduction, canonicalize_clausewitz_file, merge_clausewitz_files,
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
