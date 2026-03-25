pub(crate) mod binding;
pub(crate) mod overlap;

pub(crate) use binding::{
	build_runtime_state_for_request, build_runtime_state_from_workspace, dependency_hint_for_edge,
	nearest_enclosing_definition, runtime_reference_target, DependencyMatchKind, RuntimeState,
};
pub(crate) use overlap::{build_overlap_findings, OverlapStatus};
