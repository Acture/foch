use super::{
	BackendOutcome, BackendProfile, BackendRequest, BackendUnit, MergeBackend,
	MergeBackendDescriptor, MergeBackendId,
};
use crate::merge::output::materialize::structural::reference::{
	merge_definition_module, merge_structural_file,
};

pub(crate) struct AddressPatchBackend;

impl MergeBackend for AddressPatchBackend {
	fn descriptor(&self) -> MergeBackendDescriptor {
		MergeBackendId::AddressPatch.descriptor()
	}

	fn profile(&self) -> BackendProfile {
		BackendProfile {
			validate_semantic_units: false,
			duplicate_definition_override: None,
		}
	}

	fn analyze(&self, request: BackendRequest<'_, '_>) -> BackendOutcome {
		match request.unit {
			BackendUnit::File(contributors) => merge_structural_file(
				request.target_path,
				contributors,
				request.context,
				request.interactive_handler,
				request.interactive_config_path,
			),
			BackendUnit::DefinitionModule(views) => merge_definition_module(
				request.target_path,
				views,
				request.context,
				request.interactive_handler,
				request.interactive_config_path,
			),
		}
	}
}
