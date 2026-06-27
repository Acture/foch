pub(crate) mod binding;
pub(crate) mod overlap;

pub(crate) use binding::{
	DependencyMatchKind, build_runtime_state_from_workspace, dependency_hint_for_edge,
	nearest_enclosing_definition, runtime_reference_target,
};
pub use binding::{RuntimeState, build_runtime_state_for_request};
pub(crate) use overlap::{OverlapStatus, build_overlap_findings};
