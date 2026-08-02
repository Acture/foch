//! Structural merge seam used by the materialization shell.

use std::path::Path;

use foch_language::analyzer::content_family::DuplicateDefinitionPolicy;

use super::{
	StructuralMergeContext, StructuralMergeFailure, StructuralMergeOutput,
	structural::{
		merge_semantic_definition_module, merge_semantic_structural_file,
		reference::{
			merge_definition_module as merge_reference_definition_module,
			merge_structural_file as merge_reference_structural_file,
		},
	},
};
use crate::merge::planning::module_view::CrossFileModuleViews;
use crate::merge::resolution::conflict_handler::ConflictHandler;
use crate::workspace::ResolvedFileContributor;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StructuralBackendProfile {
	/// Stable historical label used in existing cache and evaluation artifacts.
	pub cache_label: &'static str,
	pub validate_semantic_units: bool,
	pub duplicate_definition_override: Option<DuplicateDefinitionPolicy>,
}

pub(crate) trait StructuralMergeBackend {
	fn profile(&self) -> StructuralBackendProfile;

	fn merge_file(
		&self,
		target_path: &str,
		contributors: &[ResolvedFileContributor],
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure>;

	fn merge_definition_module(
		&self,
		target_path: &str,
		views: &CrossFileModuleViews,
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure>;
}

pub(crate) struct SemanticStructuralBackend;

impl StructuralMergeBackend for SemanticStructuralBackend {
	fn profile(&self) -> StructuralBackendProfile {
		StructuralBackendProfile {
			cache_label: "structured",
			validate_semantic_units: true,
			duplicate_definition_override: Some(DuplicateDefinitionPolicy::LaterDefinitionWins),
		}
	}

	fn merge_file(
		&self,
		target_path: &str,
		contributors: &[ResolvedFileContributor],
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
		merge_semantic_structural_file(
			target_path,
			contributors,
			context,
			interactive_handler,
			interactive_config_path,
		)
	}

	fn merge_definition_module(
		&self,
		target_path: &str,
		views: &CrossFileModuleViews,
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
		merge_semantic_definition_module(
			target_path,
			views,
			context,
			interactive_handler,
			interactive_config_path,
		)
	}
}

pub(crate) struct ReferenceStructuralBackend;

impl StructuralMergeBackend for ReferenceStructuralBackend {
	fn profile(&self) -> StructuralBackendProfile {
		StructuralBackendProfile {
			cache_label: "legacy",
			validate_semantic_units: false,
			duplicate_definition_override: None,
		}
	}

	fn merge_file(
		&self,
		target_path: &str,
		contributors: &[ResolvedFileContributor],
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
		merge_reference_structural_file(
			target_path,
			contributors,
			context,
			interactive_handler,
			interactive_config_path,
		)
	}

	fn merge_definition_module(
		&self,
		target_path: &str,
		views: &CrossFileModuleViews,
		context: StructuralMergeContext<'_>,
		interactive_handler: Option<&mut (dyn ConflictHandler + '_)>,
		interactive_config_path: Option<&Path>,
	) -> Result<StructuralMergeOutput, StructuralMergeFailure> {
		merge_reference_definition_module(
			target_path,
			views,
			context,
			interactive_handler,
			interactive_config_path,
		)
	}
}
