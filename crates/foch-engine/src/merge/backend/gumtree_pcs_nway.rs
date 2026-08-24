use foch::game::eu4::content::DuplicateDefinitionPolicy;

use super::{
	BackendOutcome, BackendProfile, BackendRequest, BackendUnit, MergeBackend,
	MergeBackendDescriptor, MergeBackendId,
};
use crate::merge::output::materialize::structural::{
	merge_semantic_definition_module, merge_semantic_structural_file,
};

pub(crate) struct GumtreePcsNwayBackend;

impl MergeBackend for GumtreePcsNwayBackend {
	fn descriptor(&self) -> MergeBackendDescriptor {
		MergeBackendId::GumtreePcsNway.descriptor()
	}

	fn profile(&self) -> BackendProfile {
		BackendProfile {
			validate_semantic_units: true,
			duplicate_definition_override: Some(DuplicateDefinitionPolicy::LaterDefinitionWins),
		}
	}

	fn analyze(&self, request: BackendRequest<'_, '_>) -> BackendOutcome {
		match request.unit {
			BackendUnit::File(contributors) => merge_semantic_structural_file(
				request.target_path,
				contributors,
				request.context,
				request.interactive_handler,
				request.interactive_config_path,
			),
			BackendUnit::DefinitionModule(views) => merge_semantic_definition_module(
				request.target_path,
				views,
				request.context,
				request.interactive_handler,
				request.interactive_config_path,
			),
		}
	}
}
