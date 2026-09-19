//! Per-unit semantic analysis, kept apart from applying its result.
//!
//! Analysis reads only frozen inputs and returns an owned result: it never
//! touches the output tree, the report or the review ledger. Results are
//! applied strictly in plan order, so the order in which units are analyzed
//! cannot change the merged output.

use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::localisation_merge::{LocalisationMergeOutcome, merge_localisation_file};
use super::{StructuralMergeContext, effective_vanilla_base_mode, validate_structured_merge_entry};
use crate::game::eu4::Eu4;
use crate::game::eu4::content::ContentFamilyDescriptor;
use crate::game::eu4::script::emit::EmitOptions;
use crate::input::{ResolvedInput, ResolvedInputContributor};
use crate::merge::backend::{BackendOutcome, BackendRequest, BackendUnit, MergeBackend};
use crate::merge::conflict_handler::ConflictHandler;
use crate::merge::dag::{IgnoreReplacePath, ModDag};
use crate::merge::model::VanillaBaseMode;
use crate::merge::planning::module_view::{
	CrossFileModuleViewError, build_cross_file_module_views,
};
use crate::model::{
	DeferredUnitReason, DepMisuseFinding, MergeModuleOutput, MergePlanEntry, MergePlanStrategy,
	MergePlanTarget,
};
use crate::project::{DepOverride, ResolutionMap};

/// Frozen inputs that every unit's analysis borrows. Nothing here changes
/// while units are analyzed.
pub(super) struct UnitAnalysisContext<'a> {
	pub(super) input: &'a ResolvedInput,
	pub(super) profile: &'a Eu4,
	pub(super) backend: &'a dyn MergeBackend,
	pub(super) mod_dag: &'a ModDag,
	pub(super) ignore_replace_path: &'a IgnoreReplacePath,
	pub(super) dep_overrides: &'a [DepOverride],
	/// Dependency-misuse findings as detected before any unit ran. Analysis
	/// reads only which (mod, dependency) pairs are suspicious; the evidence
	/// counts a unit adds are applied to the report, never read back here.
	pub(super) dep_misuse: &'a [DepMisuseFinding],
	pub(super) resolution_map: &'a ResolutionMap,
	pub(super) mod_versions: &'a HashMap<String, String>,
	pub(super) mod_display_names: &'a HashMap<String, String>,
	pub(super) cache_game_version: &'a str,
	pub(super) emit_options: &'a EmitOptions,
	pub(super) include_game_base: bool,
	pub(super) gui_scroll_merge: bool,
	pub(super) provenance: bool,
}

/// The interactive conflict prompt. Only the thread that applies results may
/// hold it: prompts, and the resolutions they persist, must follow plan order.
pub(super) struct InteractivePrompt<'a> {
	pub(super) handler: Option<&'a mut (dyn ConflictHandler + 'static)>,
	pub(super) config_path: Option<&'a Path>,
}

impl InteractivePrompt<'_> {
	/// For worker threads, which never prompt.
	pub(super) fn none() -> Self {
		Self {
			handler: None,
			config_path: None,
		}
	}

	fn reborrow(&mut self) -> InteractivePrompt<'_> {
		InteractivePrompt {
			handler: self.handler.as_deref_mut(),
			config_path: self.config_path,
		}
	}
}

/// A backend outcome, or the payload of a panic the backend raised. Boxed so
/// that results waiting to be applied stay small.
pub(super) type CaughtOutcome = Box<std::thread::Result<BackendOutcome>>;

pub(super) enum UnitAnalysis {
	/// The strategy has nothing to analyze, or plan validation rejects the
	/// unit and applying it fails the merge.
	Nothing,
	/// `None` when the plan has no contributors for the path.
	Localisation(Option<Result<LocalisationMergeOutcome, String>>),
	File(FileAnalysis),
	Module(ModuleAnalysis),
}

pub(super) enum FileAnalysis {
	Merged(CaughtOutcome),
	/// Fewer than two mods contribute, or the family has no merge-key
	/// contract: the highest-precedence contributor is copied.
	NoMergeNeeded,
	/// No observed vanilla base, and the family does not permit a known-empty
	/// semantic ancestor.
	NoVerifiedBase,
}

pub(super) struct ModuleAnalysis {
	/// One analysis per output namespace in output order, ending at the first
	/// namespace that did not merge: a unit that stops there writes nothing,
	/// so the namespaces after it are never needed.
	pub(super) namespaces: Vec<NamespaceAnalysis>,
	pub(super) elapsed: Duration,
}

pub(super) enum NamespaceAnalysis {
	Analyzed(CaughtOutcome),
	Failed(DeferredUnitReason, String),
}

impl NamespaceAnalysis {
	fn merged(&self) -> bool {
		matches!(self, Self::Analyzed(outcome) if matches!(**outcome, Ok(Ok(_))))
	}
}

/// Peak memory analyzing a unit may take, per byte of its inputs (every
/// contributor, vanilla included). Measured with one worker on a retained
/// 30-mod Workshop page (2026-09-19): province and country history files
/// peaked at about 500 to 900 bytes per input byte, and the nine largest
/// definition modules at 325 to 1,758, up to 6.9 GB for the 5 MB of scripted
/// effects. Estimates only decide when a unit starts, never what it writes.
const WORKING_SET_PER_INPUT_BYTE: u64 = 2_000;

/// Estimated peak memory, in bytes, of analyzing `entry`.
pub(super) fn working_set_estimate(input: &ResolvedInput, entry: &MergePlanEntry) -> u64 {
	let input_bytes: u64 = entry
		.target
		.input_paths()
		.iter()
		.filter_map(|path| input.file_inventory.get(path))
		.flatten()
		.filter_map(|contributor| fs::metadata(&contributor.absolute_path).ok())
		.map(|metadata| metadata.len())
		.sum();
	input_bytes.saturating_mul(WORKING_SET_PER_INPUT_BYTE)
}

/// Whether a unit's strategy has analysis worth handing to a worker; the
/// others only copy or defer, which applying does itself.
pub(super) fn unit_needs_analysis(entry: &MergePlanEntry) -> bool {
	matches!(
		entry.strategy,
		MergePlanStrategy::LocalisationMerge | MergePlanStrategy::StructuralMerge
	)
}

pub(super) fn analyze_unit(
	context: &UnitAnalysisContext<'_>,
	entry: &MergePlanEntry,
	prompt: InteractivePrompt<'_>,
) -> UnitAnalysis {
	match entry.strategy {
		MergePlanStrategy::LocalisationMerge => UnitAnalysis::Localisation(
			context
				.input
				.file_inventory
				.get(entry.output_path())
				.map(|contributors| merge_localisation_file(entry.output_path(), contributors)),
		),
		MergePlanStrategy::StructuralMerge => analyze_structural_unit(context, entry, prompt),
		MergePlanStrategy::CopyThrough
		| MergePlanStrategy::LastWriterOverlay
		| MergePlanStrategy::ManualConflict => UnitAnalysis::Nothing,
	}
}

fn analyze_structural_unit(
	context: &UnitAnalysisContext<'_>,
	entry: &MergePlanEntry,
	prompt: InteractivePrompt<'_>,
) -> UnitAnalysis {
	let path: &str = entry.output_path();
	let contributors: Option<&[ResolvedInputContributor]> =
		context.input.file_inventory.get(path).map(Vec::as_slice);
	let descriptor: Option<&ContentFamilyDescriptor> =
		context.profile.classify_content_family(Path::new(path));
	let vanilla_base_mode: VanillaBaseMode = effective_vanilla_base_mode(
		descriptor,
		contributors,
		VanillaBaseMode::from_include_game_base(context.include_game_base),
		context.input.verified_absent_base_paths.contains(path),
	);
	// Applying validates the unit first and fails the whole merge when it is
	// rejected. A rejected unit must not reach the backend here, where it could
	// panic and surface as a different failure.
	if context.backend.profile().validate_semantic_units
		&& validate_structured_merge_entry(entry, contributors, vanilla_base_mode, context.profile)
			.is_err()
	{
		return UnitAnalysis::Nothing;
	}
	if matches!(&entry.target, MergePlanTarget::Module { .. }) {
		let started: Instant = Instant::now();
		let namespaces: Vec<NamespaceAnalysis> = analyze_module(context, entry, prompt);
		return UnitAnalysis::Module(ModuleAnalysis {
			namespaces,
			elapsed: started.elapsed(),
		});
	}
	UnitAnalysis::File(analyze_file(
		context,
		path,
		contributors,
		descriptor,
		vanilla_base_mode,
		prompt,
	))
}

fn analyze_file(
	context: &UnitAnalysisContext<'_>,
	path: &str,
	contributors: Option<&[ResolvedInputContributor]>,
	descriptor: Option<&ContentFamilyDescriptor>,
	vanilla_base_mode: VanillaBaseMode,
	prompt: InteractivePrompt<'_>,
) -> FileAnalysis {
	let can_run_semantic_merge: bool = !vanilla_base_mode.requires_non_empty()
		|| contributors.is_some_and(|contributors| contributors.iter().any(|c| c.is_base_game));
	let Some(contributors) = contributors.filter(|_| can_run_semantic_merge) else {
		return FileAnalysis::NoVerifiedBase;
	};
	// Only invoke the tree merge when 2+ non-base mods contribute (single-mod
	// overlap with base is just last-writer).
	let non_base_count: usize = contributors
		.iter()
		.filter(|c| !c.is_base_game && !c.is_synthetic_base)
		.count();
	let Some((descriptor, merge_key_source)) = descriptor
		.filter(|_| non_base_count >= 2)
		.and_then(|descriptor| Some((descriptor, descriptor.merge_key_source?)))
	else {
		return FileAnalysis::NoMergeNeeded;
	};
	let merge_context: StructuralMergeContext<'_> = StructuralMergeContext {
		descriptor,
		merge_key_source,
		gui_scroll_merge: context.gui_scroll_merge,
		mod_dag: context.mod_dag,
		ignore_replace_path: context.ignore_replace_path,
		dep_overrides: context.dep_overrides,
		dep_misuse_findings: context.dep_misuse,
		resolution_map: context.resolution_map,
		mod_versions: context.mod_versions,
		mod_display_names: context.mod_display_names,
		cache_game_version: context.cache_game_version,
		emit_options: context.emit_options,
		provenance: context.provenance,
		script_cache: &context.input.script_cache,
		vanilla_base_mode,
	};
	FileAnalysis::Merged(Box::new(catch_unwind(AssertUnwindSafe(|| {
		context.backend.analyze(BackendRequest {
			target_path: path,
			unit: BackendUnit::File(contributors),
			context: merge_context,
			interactive_handler: prompt.handler,
			interactive_config_path: prompt.config_path,
		})
	}))))
}

fn analyze_module(
	context: &UnitAnalysisContext<'_>,
	entry: &MergePlanEntry,
	mut prompt: InteractivePrompt<'_>,
) -> Vec<NamespaceAnalysis> {
	let mut analyses: Vec<NamespaceAnalysis> = Vec::new();
	for namespace in entry.target.module_outputs() {
		let analysis: NamespaceAnalysis =
			analyze_module_namespace(context, entry, namespace, prompt.reborrow());
		let merged: bool = analysis.merged();
		analyses.push(analysis);
		if !merged {
			break;
		}
	}
	analyses
}

fn analyze_module_namespace(
	context: &UnitAnalysisContext<'_>,
	entry: &MergePlanEntry,
	namespace: &MergeModuleOutput,
	prompt: InteractivePrompt<'_>,
) -> NamespaceAnalysis {
	let output_path: &str = namespace.output_path.as_str();
	eprintln!("[merge] definition module: start {output_path}");
	// The descriptor comes from this namespace's own output path: the
	// extractors dispatch on the directory a definition was read from.
	let Some(descriptor) = context
		.profile
		.classify_content_family(Path::new(output_path))
	else {
		return NamespaceAnalysis::Failed(
			DeferredUnitReason::EngineFailure,
			format!("missing content-family descriptor for {output_path}"),
		);
	};
	let Some(merge_key_source) = descriptor.merge_key_source else {
		return NamespaceAnalysis::Failed(
			DeferredUnitReason::EngineFailure,
			format!("missing merge-key policy for {output_path}"),
		);
	};
	let views = match build_cross_file_module_views(
		entry,
		namespace,
		context.input,
		descriptor,
		context.mod_dag,
		context.ignore_replace_path,
		context.dep_overrides,
		context.backend.profile().duplicate_definition_override,
	) {
		Ok(views) => views,
		Err(CrossFileModuleViewError::UnsupportedInput(reason)) => {
			return NamespaceAnalysis::Failed(DeferredUnitReason::UnsupportedInput, reason);
		}
		Err(CrossFileModuleViewError::EngineFailure(reason)) => {
			return NamespaceAnalysis::Failed(DeferredUnitReason::EngineFailure, reason);
		}
	};
	let merge_context: StructuralMergeContext<'_> = StructuralMergeContext {
		descriptor,
		merge_key_source,
		gui_scroll_merge: context.gui_scroll_merge,
		mod_dag: context.mod_dag,
		ignore_replace_path: context.ignore_replace_path,
		dep_overrides: context.dep_overrides,
		dep_misuse_findings: context.dep_misuse,
		resolution_map: context.resolution_map,
		mod_versions: context.mod_versions,
		mod_display_names: context.mod_display_names,
		cache_game_version: context.cache_game_version,
		emit_options: context.emit_options,
		provenance: context.provenance,
		script_cache: &context.input.script_cache,
		vanilla_base_mode: VanillaBaseMode::from_include_game_base(context.include_game_base),
	};
	NamespaceAnalysis::Analyzed(Box::new(catch_unwind(AssertUnwindSafe(|| {
		context.backend.analyze(BackendRequest {
			target_path: output_path,
			unit: BackendUnit::DefinitionModule(&views),
			context: merge_context,
			interactive_handler: prompt.handler,
			interactive_config_path: prompt.config_path,
		})
	}))))
}
