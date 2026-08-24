//! Crate-private adapter seam for structural merge implementations.

mod address_patch;
mod gumtree_pcs_nway;

use std::path::Path;

use foch_language::analyzer::content_family::DuplicateDefinitionPolicy;

use super::output::materialize::{
	StructuralMergeContext, StructuralMergeFailure, StructuralMergeOutput,
};
use super::planning::module_view::CrossFileModuleViews;
use super::resolution::conflict_handler::ConflictHandler;
use crate::workspace::ResolvedFileContributor;

pub(crate) use super::kernel::{MergeBackendDescriptor, MergeBackendId};
pub(crate) use address_patch::AddressPatchBackend;
pub(crate) use gumtree_pcs_nway::GumtreePcsNwayBackend;

#[derive(Clone, Copy, Debug)]
pub(crate) struct BackendProfile {
	pub validate_semantic_units: bool,
	pub duplicate_definition_override: Option<DuplicateDefinitionPolicy>,
}

pub(crate) enum BackendUnit<'a> {
	File(&'a [ResolvedFileContributor]),
	DefinitionModule(&'a CrossFileModuleViews),
}

pub(crate) struct BackendRequest<'data, 'handler> {
	pub target_path: &'data str,
	pub unit: BackendUnit<'data>,
	pub context: StructuralMergeContext<'data>,
	pub interactive_handler: Option<&'handler mut (dyn ConflictHandler + 'static)>,
	pub interactive_config_path: Option<&'data Path>,
}

pub(crate) type BackendOutcome = Result<StructuralMergeOutput, StructuralMergeFailure>;

pub(crate) trait MergeBackend {
	fn descriptor(&self) -> MergeBackendDescriptor;
	fn profile(&self) -> BackendProfile;
	fn analyze(&self, request: BackendRequest<'_, '_>) -> BackendOutcome;
}

pub(crate) fn backend_for(id: MergeBackendId) -> Box<dyn MergeBackend> {
	let backend: Box<dyn MergeBackend> = match id {
		MergeBackendId::GumtreePcsNway => Box::new(GumtreePcsNwayBackend),
		MergeBackendId::AddressPatch => Box::new(AddressPatchBackend),
	};
	debug_assert_eq!(backend.descriptor().id, id);
	backend
}

#[cfg(test)]
mod tests {
	use super::{MergeBackendId, backend_for};

	#[test]
	fn backend_factory_preserves_stable_identity_and_maturity() {
		let stable = backend_for(MergeBackendId::default()).descriptor();
		assert_eq!(stable.id, MergeBackendId::GumtreePcsNway);
		assert_eq!(stable.id.as_str(), "gumtree-pcs-nway");
		assert!(!stable.experimental);

		let experimental = backend_for(MergeBackendId::AddressPatch).descriptor();
		assert_eq!(experimental.id.as_str(), "address-patch");
		assert!(experimental.experimental);
	}
}
