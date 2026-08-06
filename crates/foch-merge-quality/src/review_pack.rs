use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Eu4GameDiscovery;
use crate::dataset::{
	DatasetPaths, MeasurementCohortKey, MeasurementKernel, MeasurementRecord, SnapshotRecord,
	TerminalStatus, read_jsonl, stable_id,
};
use crate::object_store::ObjectStore;
use crate::orchestrate::FileRecord;
use crate::review_annotation::{
	AstRelation, ReviewAnnotation, ReviewAnnotationDraft, ReviewBinding, ReviewKernel, ReviewLabel,
	ReviewRecordKind, ReviewStatus,
};
use crate::score::{
	ReviewSemanticEvidence, ScoreCache, ScoreFileRequest, SourceMod,
	review_semantic_evidence_with_cache, score_file_with_cache_and_basegame,
	structured_module_semantic_relation,
};
use crate::shadow::{
	SHADOW_COMPARE_SCHEMA, ShadowCaptureRequest, ShadowDiagnostic, ShadowDiagnosticKind,
	ShadowInputManifest, ShadowRunRecord, capture_input_manifest, verified_retained_base_snapshot,
};
use crate::snapshot::{
	LoadedSnapshot, adjudication_ast_pair, open_snapshot, validate_snapshot_game,
};

pub const REVIEW_PACK_SCHEMA: &str = "1.0.0";
pub const REVIEW_PACK_PROFILE: &str = "eu4-merge-quality";
pub const REVIEW_PACK_CASE_COUNT: usize = 6;
pub const REVIEW_PACK_LEGACY_UNIT_COUNT: usize = 36;
pub const REVIEW_PACK_STRUCTURED_UNIT_COUNT: usize = 13;
pub const REVIEW_PACK_LEGACY_SCORER_VERSION: &str = "1.3.0";

const REVIEW_PACK_LEGACY_EXECUTABLE_HASH: &str =
	"0507a19de246a59bd2f718ad2941fd4d0c9ec07d469ab911a1e6b04bb11ba519";
const REVIEW_PACK_LEGACY_CONFIG_HASH: &str =
	"8beffefe06b044798b769b805fb556dd93769ebdbf367df3d6468ef6834d5665";

type ReviewResult<T> = Result<T, ReviewPackError>;

#[derive(Debug)]
pub enum ReviewPackError {
	Io(io::Error),
	Json(serde_json::Error),
	Invalid(String),
}

impl fmt::Display for ReviewPackError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(formatter, "{error}"),
			Self::Json(error) => write!(formatter, "{error}"),
			Self::Invalid(detail) => formatter.write_str(detail),
		}
	}
}

impl Error for ReviewPackError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::Json(error) => Some(error),
			Self::Invalid(_) => None,
		}
	}
}

impl From<io::Error> for ReviewPackError {
	fn from(error: io::Error) -> Self {
		Self::Io(error)
	}
}

impl From<serde_json::Error> for ReviewPackError {
	fn from(error: serde_json::Error) -> Self {
		Self::Json(error)
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackSelection {
	pub schema: String,
	pub profile: String,
	pub game_version: String,
	pub steam_build_id: u64,
	pub legacy_baseline_blake3: String,
	pub expected_verdicts_blake3: String,
	pub legacy_unit_count: usize,
	pub structured_unit_count: usize,
	pub cases: Vec<ReviewPackSelectionCase>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackSelectionCase {
	pub case_id: String,
	pub snapshot_id: String,
	pub legacy_measurement_id: String,
	pub legacy_output_hash: String,
	pub legacy_units: Vec<String>,
	pub structured_units: Vec<String>,
}

impl ReviewPackSelection {
	pub fn from_path(path: &Path) -> ReviewResult<Self> {
		let selection = serde_json::from_slice::<Self>(&fs::read(path)?)?;
		selection.validate()?;
		Ok(selection)
	}

	pub fn validate(&self) -> ReviewResult<()> {
		if self.schema != REVIEW_PACK_SCHEMA {
			return invalid(format!(
				"unsupported review-pack selection schema {}; expected {REVIEW_PACK_SCHEMA}",
				self.schema
			));
		}
		if self.profile != REVIEW_PACK_PROFILE {
			return invalid(format!(
				"unsupported review-pack profile {}; expected {REVIEW_PACK_PROFILE}",
				self.profile
			));
		}
		if self.game_version.trim().is_empty() {
			return invalid("selection game_version must not be empty");
		}
		if self.steam_build_id == 0 {
			return invalid("selection steam_build_id must be non-zero");
		}
		validate_hash(
			"selection legacy_baseline_blake3",
			&self.legacy_baseline_blake3,
		)?;
		validate_hash(
			"selection expected_verdicts_blake3",
			&self.expected_verdicts_blake3,
		)?;
		if self.cases.len() != REVIEW_PACK_CASE_COUNT {
			return invalid(format!(
				"review-pack case denominator drifted: expected {REVIEW_PACK_CASE_COUNT}, found {}",
				self.cases.len()
			));
		}
		if self.legacy_unit_count != REVIEW_PACK_LEGACY_UNIT_COUNT {
			return invalid(format!(
				"review-pack Legacy denominator drifted: expected {REVIEW_PACK_LEGACY_UNIT_COUNT}, found {}",
				self.legacy_unit_count
			));
		}
		if self.structured_unit_count != REVIEW_PACK_STRUCTURED_UNIT_COUNT {
			return invalid(format!(
				"review-pack Structured denominator drifted: expected {REVIEW_PACK_STRUCTURED_UNIT_COUNT}, found {}",
				self.structured_unit_count
			));
		}

		let mut case_ids = BTreeSet::new();
		let mut snapshot_ids = BTreeSet::new();
		let mut measurement_ids = BTreeSet::new();
		let mut legacy_count = 0;
		let mut structured_count = 0;
		for case in &self.cases {
			validate_required("selection case_id", &case.case_id)?;
			validate_hash("selection snapshot_id", &case.snapshot_id)?;
			validate_hash(
				"selection legacy_measurement_id",
				&case.legacy_measurement_id,
			)?;
			validate_hash("selection legacy_output_hash", &case.legacy_output_hash)?;
			if !case_ids.insert(case.case_id.as_str()) {
				return invalid(format!(
					"selection contains duplicate case {}",
					case.case_id
				));
			}
			if !snapshot_ids.insert(case.snapshot_id.as_str()) {
				return invalid(format!(
					"selection contains duplicate snapshot {}",
					case.snapshot_id
				));
			}
			if !measurement_ids.insert(case.legacy_measurement_id.as_str()) {
				return invalid(format!(
					"selection contains duplicate Legacy measurement {}",
					case.legacy_measurement_id
				));
			}
			let legacy = validate_unit_paths(&case.case_id, "Legacy", &case.legacy_units)?;
			let structured =
				validate_unit_paths(&case.case_id, "Structured", &case.structured_units)?;
			if !structured.is_subset(&legacy) {
				let outside = structured
					.difference(&legacy)
					.copied()
					.collect::<Vec<_>>()
					.join(", ");
				return invalid(format!(
					"Structured selection for case {} is not a Legacy subset: {outside}",
					case.case_id
				));
			}
			legacy_count += legacy.len();
			structured_count += structured.len();
		}
		if legacy_count != self.legacy_unit_count {
			return invalid(format!(
				"selection declares {} Legacy units but contains {legacy_count}",
				self.legacy_unit_count
			));
		}
		if structured_count != self.structured_unit_count {
			return invalid(format!(
				"selection declares {} Structured units but contains {structured_count}",
				self.structured_unit_count
			));
		}
		Ok(())
	}
}

fn validate_unit_paths<'a>(
	case_id: &str,
	kernel: &str,
	paths: &'a [String],
) -> ReviewResult<BTreeSet<&'a str>> {
	if paths.is_empty() {
		return invalid(format!("{kernel} selection for case {case_id} is empty"));
	}
	let mut unique = BTreeSet::new();
	for path in paths {
		validate_relative_path(path)?;
		if !unique.insert(path.as_str()) {
			return invalid(format!(
				"{kernel} selection for case {case_id} contains duplicate path {path}"
			));
		}
	}
	Ok(unique)
}

#[derive(Clone, Debug)]
pub struct ReviewPackBuildOptions<'a> {
	pub selection: &'a Path,
	pub legacy_baseline: &'a Path,
	pub expected_verdicts: &'a Path,
	pub dataset_root: &'a Path,
	pub output_dir: &'a Path,
	pub game: &'a Eu4GameDiscovery,
	pub executable: &'a Path,
	pub timeout: Duration,
	pub force: bool,
	pub wiki_knowledge_snapshot_id: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct ReviewPackVerifyOptions<'a> {
	pub pack_dir: &'a Path,
	pub selection: &'a Path,
	pub legacy_baseline: &'a Path,
	pub expected_verdicts: &'a Path,
	pub dataset_root: &'a Path,
	pub game: &'a Eu4GameDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackArtifact {
	pub path: String,
	pub blake3: String,
	pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackUnitArtifact {
	pub scoring_unit_id: String,
	pub case_id: String,
	pub relative_path: String,
	pub kernel: ReviewKernel,
	pub artifact: ReviewPackArtifact,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackRawCasBinding {
	pub compatch_content_hash: String,
	/// Source object hashes in declared playset order.
	pub source_content_hashes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackSemanticHashes {
	pub base: String,
	/// Source semantic hashes in declared playset order.
	pub sources: Vec<String>,
	pub human: String,
	pub candidate: String,
	pub candidate_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackTiming {
	pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackRolloutSelection {
	pub selected_by_fixture: bool,
	pub selected_kernel: ReviewKernel,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackUnitEvidence {
	pub schema: String,
	pub review_pack_id: String,
	pub scoring_unit_id: String,
	pub case_id: String,
	pub relative_path: String,
	pub snapshot_id: String,
	pub kernel: ReviewKernel,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub wiki_knowledge_snapshot_id: Option<String>,
	pub base_snapshot_identity: String,
	pub raw_cas: ReviewPackRawCasBinding,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub output_cas_hash: Option<String>,
	pub semantic_hashes: ReviewPackSemanticHashes,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub semantic_evidence: Option<ReviewSemanticEvidence>,
	pub ast_relation: AstRelation,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file_record: Option<FileRecord>,
	pub diagnostics: Vec<ShadowDiagnostic>,
	pub timing: ReviewPackTiming,
	pub rollout_selection: ReviewPackRolloutSelection,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub legacy_measurement_id: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub shadow_comparison_id: Option<String>,
}

impl ReviewPackUnitEvidence {
	pub fn binding(&self) -> ReviewBinding {
		ReviewBinding {
			review_pack_id: self.review_pack_id.clone(),
			wiki_knowledge_snapshot_id: self.wiki_knowledge_snapshot_id.clone(),
			case_id: self.case_id.clone(),
			relative_path: self.relative_path.clone(),
			snapshot_id: self.snapshot_id.clone(),
			scoring_unit_id: self.scoring_unit_id.clone(),
			kernel: self.kernel,
			base_content_hash: self.semantic_hashes.base.clone(),
			source_content_hashes: self.semantic_hashes.sources.clone(),
			human_content_hash: self.semantic_hashes.human.clone(),
			candidate_content_hash: self.semantic_hashes.candidate.clone(),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackStructuredAttestation {
	pub comparison_id: String,
	pub exit_code: i32,
	pub manual_conflict_count: usize,
	pub handler_resolution_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackCaseRun {
	pub case_id: String,
	pub snapshot_id: String,
	pub kernel: ReviewKernel,
	pub status: String,
	pub output_valid: bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub output_cas_hash: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub legacy_measurement_id: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub structured_attestation: Option<ReviewPackStructuredAttestation>,
	pub diagnostic_kinds: Vec<ShadowDiagnosticKind>,
	pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackManifest {
	pub schema: String,
	pub review_pack_id: String,
	pub profile: String,
	pub game_version: String,
	pub steam_build_id: u64,
	pub base_snapshot_identity: String,
	pub executable_blake3: String,
	pub legacy_baseline_blake3: String,
	pub expected_verdicts_blake3: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub wiki_knowledge_snapshot_id: Option<String>,
	pub case_count: usize,
	pub legacy_unit_count: usize,
	pub structured_unit_count: usize,
	pub case_runs: Vec<ReviewPackCaseRun>,
	pub units: Vec<ReviewPackUnitArtifact>,
	pub summary: ReviewPackArtifact,
	pub proposals: ReviewPackArtifact,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPackSummary {
	pub schema: String,
	pub review_pack_id: String,
	pub case_count: usize,
	pub total_units: usize,
	pub legacy_units: usize,
	pub structured_units: usize,
	pub legacy_executions: usize,
	pub structured_executions: usize,
	pub structured_valid_cases: usize,
	pub structured_invalid_cases: usize,
	pub equivalent_proposals: usize,
	pub insufficient_evidence_proposals: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPackBuildResult {
	pub manifest: ReviewPackManifest,
	pub summary: ReviewPackSummary,
	pub units: Vec<ReviewPackUnitEvidence>,
	pub proposals: Vec<ReviewAnnotation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPackVerifyResult {
	pub review_pack_id: String,
	pub units_verified: usize,
	pub cas_objects_verified: usize,
	pub proposals_verified: usize,
}

pub struct StructuredKernelRequest<'a> {
	pub manifest: &'a ShadowInputManifest,
	pub manifest_path: &'a Path,
	pub output_dir: &'a Path,
	pub executable: &'a Path,
	pub timeout: Duration,
}

pub trait StructuredKernelRunner {
	fn run_structured(
		&mut self,
		request: StructuredKernelRequest<'_>,
	) -> io::Result<ShadowRunRecord>;
}

/// Write the launcher playset used only by the review-pack orchestration path.
fn write_playset(root: &Path, mods: &[(String, PathBuf)]) -> io::Result<PathBuf> {
	fs::create_dir_all(root.join("mod"))?;
	let mut enabled = Vec::with_capacity(mods.len());
	for (steam_id, workshop_dir) in mods {
		let relative = format!("mod/ugc_{steam_id}.mod");
		let absolute = if workshop_dir.is_absolute() {
			workshop_dir.clone()
		} else {
			std::env::current_dir()?.join(workshop_dir)
		};
		let path = absolute.to_string_lossy().replace('\\', "/");
		fs::write(
			root.join(&relative),
			format!("name=\"{steam_id}\"\npath=\"{path}\"\nremote_file_id=\"{steam_id}\"\n"),
		)?;
		enabled.push(relative);
	}
	let launcher = serde_json::json!({ "enabled_mods": enabled, "disabled_dlcs": [] });
	let path = root.join("dlc_load.json");
	fs::write(&path, serde_json::to_vec(&launcher)?)?;
	Ok(path)
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrecomputedReviewPack {
	pub selection: ReviewPackSelection,
	pub base_snapshot_identity: String,
	pub executable_blake3: String,
	pub wiki_knowledge_snapshot_id: Option<String>,
	pub case_runs: Vec<ReviewPackCaseRun>,
	pub units: Vec<ReviewPackUnitEvidence>,
}

pub fn build_review_pack_with_runner(
	options: &ReviewPackBuildOptions<'_>,
	runner: &mut dyn StructuredKernelRunner,
) -> ReviewResult<ReviewPackBuildResult> {
	if options.timeout.is_zero() {
		return invalid("review-pack Structured timeout must be greater than zero");
	}
	if !options.executable.is_file() {
		return invalid(format!(
			"review-pack executable does not exist: {}",
			options.executable.display()
		));
	}
	if options.output_dir.exists() {
		return invalid(format!(
			"review-pack output already exists: {}",
			options.output_dir.display()
		));
	}
	if options
		.wiki_knowledge_snapshot_id
		.is_some_and(|value| value.trim().is_empty())
	{
		return invalid("wiki_knowledge_snapshot_id must not be empty");
	}

	let selection = load_and_validate_inputs(
		options.selection,
		options.legacy_baseline,
		options.expected_verdicts,
	)?;
	validate_selection_game(&selection, options.game)?;
	let dataset_paths = DatasetPaths::new(options.dataset_root);
	let snapshots = selected_snapshots(&selection, &dataset_paths, options.game)?;
	let selected_paths = selection
		.cases
		.iter()
		.flat_map(|case| case.legacy_units.iter().cloned())
		.collect::<BTreeSet<_>>();
	let base_snapshot =
		verified_retained_base_snapshot(&selection.game_version, None, &selected_paths)
			.map_err(ReviewPackError::Io)?;
	let store = ObjectStore::new(&dataset_paths.objects, &dataset_paths.work);
	let baseline = load_legacy_baseline(options.legacy_baseline, options.expected_verdicts)?;
	let legacy_cases = load_pinned_legacy_cases(&selection, &dataset_paths, &store)?;
	let executable_blake3 = hash_file(options.executable)?;

	let work_parent = options
		.output_dir
		.parent()
		.ok_or_else(|| ReviewPackError::Invalid("review-pack output has no parent".to_string()))?;
	fs::create_dir_all(work_parent)?;
	let scratch = tempfile::Builder::new()
		.prefix(".review-pack-build-")
		.tempdir_in(work_parent)?;
	let pack_objects = scratch.path().join("objects");
	let pack_store = ObjectStore::new(&pack_objects, scratch.path().join("object-work"));
	let mut case_runs = Vec::with_capacity(REVIEW_PACK_CASE_COUNT * 2);
	let mut units =
		Vec::with_capacity(REVIEW_PACK_LEGACY_UNIT_COUNT + REVIEW_PACK_STRUCTURED_UNIT_COUNT);
	let mut score_cache = ScoreCache::new();

	for case in &selection.cases {
		let loaded = open_snapshot(
			&store,
			snapshots
				.get(&case.case_id)
				.expect("selected snapshots were validated")
				.clone(),
		)?;
		let legacy = legacy_cases
			.get(&case.case_id)
			.expect("Legacy measurements were validated");
		let legacy_output = store.verify_once(&legacy.output_cas_hash)?.tree;
		case_runs.push(ReviewPackCaseRun {
			case_id: case.case_id.clone(),
			snapshot_id: case.snapshot_id.clone(),
			kernel: ReviewKernel::Legacy,
			status: "reused_archived_measurement".to_string(),
			output_valid: true,
			output_cas_hash: Some(legacy.output_cas_hash.clone()),
			legacy_measurement_id: Some(legacy.measurement_id.clone()),
			structured_attestation: None,
			diagnostic_kinds: Vec::new(),
			elapsed_ms: legacy.elapsed_ms,
		});
		for relative_path in &case.legacy_units {
			let frozen = baseline
				.units
				.get(&(case.case_id.clone(), relative_path.clone()))
				.expect("selection and baseline were validated");
			let sources = source_mods(&loaded);
			let actual = FileRecord::from_score(score_file_with_cache_and_basegame(
				&ScoreFileRequest {
					rel: relative_path,
					source_mods: &sources,
					compatch: &loaded.compatch,
					out_dir: &legacy_output,
					conflict_paths: &HashSet::new(),
				},
				&mut score_cache,
				Some(&options.game.game_root),
			));
			if &actual != frozen {
				return invalid(format!(
					"pinned Legacy output no longer reproduces the frozen scorer {} baseline for {}:{}: expected {frozen:?}, found {actual:?}",
					baseline.scorer_version, case.case_id, relative_path,
				));
			}
			units.push(make_unit_evidence(UnitEvidenceRequest {
				loaded: &loaded,
				game_root: &options.game.game_root,
				output_dir: Some(&legacy_output),
				output_cas_hash: Some(&legacy.output_cas_hash),
				relative_path,
				kernel: ReviewKernel::Legacy,
				file_record: Some(actual),
				diagnostics: Vec::new(),
				elapsed_ms: legacy.elapsed_ms,
				base_snapshot_identity: &base_snapshot.identity,
				wiki_knowledge_snapshot_id: options.wiki_knowledge_snapshot_id,
				legacy_measurement_id: Some(&legacy.measurement_id),
				shadow_comparison_id: None,
				cache: &mut score_cache,
			})?);
		}

		let case_work = scratch.path().join(&case.case_id);
		let playset_root = case_work.join("playset");
		let mods = loaded
			.snapshot
			.source_mods
			.iter()
			.zip(&loaded.source_dirs)
			.map(|(source, root)| (source.workshop_id.clone(), root.clone()))
			.collect::<Vec<_>>();
		let playset = write_playset(&playset_root, &mods)?;
		let retained_paths = case
			.structured_units
			.iter()
			.cloned()
			.collect::<BTreeSet<_>>();
		let retained_base = verified_retained_base_snapshot(
			&selection.game_version,
			Some(&base_snapshot.identity),
			&retained_paths,
		)?;
		let manifest = capture_input_manifest(ShadowCaptureRequest {
			playset: &playset,
			game_root: &options.game.game_root,
			game_version: &selection.game_version,
			retained_paths: &retained_paths,
			retained_base_paths: &retained_base.retained_paths,
			base_snapshot_identity: &base_snapshot.identity,
			force: options.force,
			executable: options.executable,
		})?;
		let manifest_path = case_work.join("shadow-inputs.json");
		fs::create_dir_all(&case_work)?;
		write_json(&manifest_path, &manifest)?;
		let structured_output = case_work.join("structured");
		let run = match runner.run_structured(StructuredKernelRequest {
			manifest: &manifest,
			manifest_path: &manifest_path,
			output_dir: &structured_output,
			executable: options.executable,
			timeout: options.timeout,
		}) {
			Ok(run) => run,
			Err(error) => ShadowRunRecord {
				schema: SHADOW_COMPARE_SCHEMA.to_string(),
				comparison_id: manifest.comparison_id.clone(),
				kernel: "structured".to_string(),
				output_dir: structured_output.clone(),
				output_valid: false,
				elapsed_ms: 0,
				status: "runner_error".to_string(),
				exit_code: None,
				manual_conflict_count: None,
				handler_resolution_count: None,
				generated_file_count: None,
				fatal_reason: None,
				error: Some(error.to_string()),
				diagnostics: vec![ShadowDiagnostic {
					kind: ShadowDiagnosticKind::Error,
					path: None,
					message: format!("grouped Structured run failed: {error}"),
				}],
			},
		};
		if run.schema != SHADOW_COMPARE_SCHEMA
			|| run.comparison_id != manifest.comparison_id
			|| run.kernel != "structured"
		{
			return invalid(format!(
				"Structured runner returned stale identity for case {}",
				case.case_id
			));
		}
		let structured_attestation = validate_structured_run_evidence(&run, &case.case_id)?;
		let output_cas_hash = Some(pack_store.snapshot_tree(&structured_output)?.hash);
		case_runs.push(ReviewPackCaseRun {
			case_id: case.case_id.clone(),
			snapshot_id: case.snapshot_id.clone(),
			kernel: ReviewKernel::Structured,
			status: run.status.clone(),
			output_valid: run.output_valid,
			output_cas_hash: output_cas_hash.clone(),
			legacy_measurement_id: None,
			structured_attestation: Some(structured_attestation),
			diagnostic_kinds: run
				.diagnostics
				.iter()
				.map(|diagnostic| diagnostic.kind)
				.collect(),
			elapsed_ms: run.elapsed_ms,
		});
		let sources = source_mods(&loaded);
		for relative_path in &case.structured_units {
			let file_record = Some(FileRecord::from_score(score_file_with_cache_and_basegame(
				&ScoreFileRequest {
					rel: relative_path,
					source_mods: &sources,
					compatch: &loaded.compatch,
					out_dir: &structured_output,
					conflict_paths: &HashSet::new(),
				},
				&mut score_cache,
				Some(&options.game.game_root),
			)));
			units.push(make_unit_evidence(UnitEvidenceRequest {
				loaded: &loaded,
				game_root: &options.game.game_root,
				output_dir: Some(&structured_output),
				output_cas_hash: output_cas_hash.as_deref(),
				relative_path,
				kernel: ReviewKernel::Structured,
				file_record,
				diagnostics: run.diagnostics.clone(),
				elapsed_ms: run.elapsed_ms,
				base_snapshot_identity: &base_snapshot.identity,
				wiki_knowledge_snapshot_id: options.wiki_knowledge_snapshot_id,
				legacy_measurement_id: None,
				shadow_comparison_id: Some(&manifest.comparison_id),
				cache: &mut score_cache,
			})?);
		}
	}

	build_from_precomputed_evidence_with_objects(
		options.output_dir,
		PrecomputedReviewPack {
			selection,
			base_snapshot_identity: base_snapshot.identity,
			executable_blake3,
			wiki_knowledge_snapshot_id: options.wiki_knowledge_snapshot_id.map(str::to_string),
			case_runs,
			units,
		},
		Some(&pack_objects),
	)
}

fn validate_structured_run_evidence(
	run: &ShadowRunRecord,
	case_id: &str,
) -> ReviewResult<ReviewPackStructuredAttestation> {
	let blocking_diagnostic = run
		.diagnostics
		.iter()
		.any(|diagnostic| is_blocking_structured_diagnostic(diagnostic.kind));
	if run.status != "ready"
		|| !run.output_valid
		|| run.exit_code != Some(0)
		|| run.manual_conflict_count != Some(0)
		|| run.handler_resolution_count != Some(0)
		|| run.fatal_reason.is_some()
		|| run.error.is_some()
		|| blocking_diagnostic
	{
		return invalid(format!(
			"Structured case {case_id} is not clean review evidence: require ready/exit=0/output_valid/manual_conflicts=0/handler_resolutions=0 and no blocking diagnostics"
		));
	}
	validate_hash("Structured runner comparison_id", &run.comparison_id)?;
	Ok(ReviewPackStructuredAttestation {
		comparison_id: run.comparison_id.clone(),
		exit_code: 0,
		manual_conflict_count: 0,
		handler_resolution_count: 0,
	})
}

fn validate_structured_case_run(run: &ReviewPackCaseRun) -> ReviewResult<()> {
	let attestation = run.structured_attestation.as_ref().ok_or_else(|| {
		ReviewPackError::Invalid(format!(
			"Structured case {} has no portable execution attestation",
			run.case_id
		))
	})?;
	let blocking_diagnostic = run
		.diagnostic_kinds
		.iter()
		.copied()
		.any(is_blocking_structured_diagnostic);
	if run.status != "ready"
		|| !run.output_valid
		|| run.output_cas_hash.is_none()
		|| attestation.exit_code != 0
		|| attestation.manual_conflict_count != 0
		|| attestation.handler_resolution_count != 0
		|| blocking_diagnostic
	{
		return invalid(format!(
			"Structured case {} persisted non-clean review evidence",
			run.case_id
		));
	}
	validate_hash("Structured case comparison_id", &attestation.comparison_id)?;
	Ok(())
}

const fn is_blocking_structured_diagnostic(kind: ShadowDiagnosticKind) -> bool {
	matches!(
		kind,
		ShadowDiagnosticKind::Error
			| ShadowDiagnosticKind::Fatal
			| ShadowDiagnosticKind::Conflict
			| ShadowDiagnosticKind::HandlerResolution
	)
}

struct UnitEvidenceRequest<'a> {
	loaded: &'a LoadedSnapshot,
	game_root: &'a Path,
	output_dir: Option<&'a Path>,
	output_cas_hash: Option<&'a str>,
	relative_path: &'a str,
	kernel: ReviewKernel,
	file_record: Option<FileRecord>,
	diagnostics: Vec<ShadowDiagnostic>,
	elapsed_ms: u64,
	base_snapshot_identity: &'a str,
	wiki_knowledge_snapshot_id: Option<&'a str>,
	legacy_measurement_id: Option<&'a str>,
	shadow_comparison_id: Option<&'a str>,
	cache: &'a mut ScoreCache,
}

fn make_unit_evidence(request: UnitEvidenceRequest<'_>) -> ReviewResult<ReviewPackUnitEvidence> {
	let sources = source_mods(request.loaded);
	let empty = tempfile::tempdir()?;
	let evidence_output = request.output_dir.unwrap_or(empty.path());
	let evidence = review_semantic_evidence_with_cache(
		&ScoreFileRequest {
			rel: request.relative_path,
			source_mods: &sources,
			compatch: &request.loaded.compatch,
			out_dir: evidence_output,
			conflict_paths: &HashSet::new(),
		},
		request.cache,
		Some(request.game_root),
	)
	.ok_or_else(|| {
		ReviewPackError::Invalid(format!(
			"semantic evidence is unavailable for {}:{}",
			request.loaded.snapshot.case_id, request.relative_path
		))
	})?;
	let candidate_available = request.output_dir.is_some();
	let semantic_hashes = semantic_hashes(
		&evidence,
		candidate_available,
		&request.loaded.snapshot.snapshot_id,
		request.relative_path,
		request.kernel,
	);
	let semantic_evidence = candidate_available.then_some(evidence);
	let raw_equivalent = semantic_evidence
		.as_ref()
		.is_some_and(exact_semantic_equivalence);
	let mut diagnostics = request.diagnostics;
	let (ast_relation, normalization_diagnostics) = evaluated_ast_relation(
		raw_equivalent,
		request.kernel,
		request.relative_path,
		request.loaded,
		request.game_root,
		request.output_dir,
	);
	diagnostics.extend(normalization_diagnostics);
	let scoring_unit_id = scoring_unit_id(
		&request.loaded.snapshot.snapshot_id,
		request.relative_path,
		request.kernel,
	);
	Ok(ReviewPackUnitEvidence {
		schema: REVIEW_PACK_SCHEMA.to_string(),
		review_pack_id: String::new(),
		scoring_unit_id,
		case_id: request.loaded.snapshot.case_id.clone(),
		relative_path: request.relative_path.to_string(),
		snapshot_id: request.loaded.snapshot.snapshot_id.clone(),
		kernel: request.kernel,
		wiki_knowledge_snapshot_id: request.wiki_knowledge_snapshot_id.map(str::to_string),
		base_snapshot_identity: request.base_snapshot_identity.to_string(),
		raw_cas: raw_cas_binding(&request.loaded.snapshot),
		output_cas_hash: request.output_cas_hash.map(str::to_string),
		semantic_hashes,
		semantic_evidence,
		ast_relation,
		file_record: request.file_record,
		diagnostics,
		timing: ReviewPackTiming {
			elapsed_ms: request.elapsed_ms,
		},
		rollout_selection: ReviewPackRolloutSelection {
			selected_by_fixture: true,
			selected_kernel: request.kernel,
		},
		legacy_measurement_id: request.legacy_measurement_id.map(str::to_string),
		shadow_comparison_id: request.shadow_comparison_id.map(str::to_string),
	})
}

fn evaluated_ast_relation(
	raw_equivalent: bool,
	kernel: ReviewKernel,
	relative_path: &str,
	loaded: &LoadedSnapshot,
	game_root: &Path,
	output_dir: Option<&Path>,
) -> (AstRelation, Vec<ShadowDiagnostic>) {
	if raw_equivalent {
		return (AstRelation::ExactEquivalent, Vec::new());
	}
	let Some(output_dir) = output_dir else {
		return (AstRelation::Nonidentical, Vec::new());
	};
	if kernel != ReviewKernel::Structured {
		return (AstRelation::Nonidentical, Vec::new());
	}
	let Some((candidate, human)) =
		adjudication_ast_pair(relative_path, loaded, game_root, output_dir)
	else {
		return (AstRelation::Nonidentical, Vec::new());
	};
	let Some(relation) = structured_module_semantic_relation(relative_path, &candidate, &human)
	else {
		return (AstRelation::Nonidentical, Vec::new());
	};
	let ast_relation = if relation.is_equivalent() {
		AstRelation::LogicalEquivalent
	} else {
		AstRelation::Nonidentical
	};
	let diagnostics = relation
		.diagnostics
		.into_iter()
		.map(|diagnostic| ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Warning,
			path: diagnostic.path,
			message: format!("{}: {}", diagnostic.phase, diagnostic.message),
		})
		.collect();
	(ast_relation, diagnostics)
}

pub fn build_from_precomputed_evidence(
	output_dir: &Path,
	input: PrecomputedReviewPack,
) -> ReviewResult<ReviewPackBuildResult> {
	build_from_precomputed_evidence_with_objects(output_dir, input, None)
}

fn build_from_precomputed_evidence_with_objects(
	output_dir: &Path,
	mut input: PrecomputedReviewPack,
	pack_objects: Option<&Path>,
) -> ReviewResult<ReviewPackBuildResult> {
	input.selection.validate()?;
	validate_pack_local_objects(&input, pack_objects)?;
	validate_precomputed(&input)?;
	if output_dir.exists() {
		return invalid(format!(
			"review-pack output already exists: {}",
			output_dir.display()
		));
	}
	let review_pack_id = review_pack_id(&input)?;
	for unit in &mut input.units {
		unit.review_pack_id.clone_from(&review_pack_id);
	}
	input.units.sort_by(unit_order);
	let proposals = input
		.units
		.iter()
		.map(proposal_for_unit)
		.collect::<ReviewResult<Vec<_>>>()?;
	let summary = summarize_pack(&review_pack_id, &input.case_runs, &input.units, &proposals);

	let parent = output_dir
		.parent()
		.ok_or_else(|| ReviewPackError::Invalid("review-pack output has no parent".to_string()))?;
	fs::create_dir_all(parent)?;
	let staging = tempfile::Builder::new()
		.prefix(".review-pack-artifacts-")
		.tempdir_in(parent)?;
	let root = staging.path();
	fs::create_dir_all(root.join("units"))?;
	if let Some(pack_objects) = pack_objects
		&& pack_objects.is_dir()
	{
		fs::rename(pack_objects, root.join("objects"))?;
	}
	let mut unit_artifacts = Vec::with_capacity(input.units.len());
	for unit in &input.units {
		let relative = format!("units/{}.json", unit.scoring_unit_id);
		write_json(&root.join(&relative), unit)?;
		unit_artifacts.push(ReviewPackUnitArtifact {
			scoring_unit_id: unit.scoring_unit_id.clone(),
			case_id: unit.case_id.clone(),
			relative_path: unit.relative_path.clone(),
			kernel: unit.kernel,
			artifact: artifact_for(root, &relative)?,
		});
	}
	write_json(&root.join("summary.json"), &summary)?;
	write_jsonl(&root.join("proposals.jsonl"), &proposals)?;
	let mut manifest = ReviewPackManifest {
		schema: REVIEW_PACK_SCHEMA.to_string(),
		review_pack_id: review_pack_id.clone(),
		profile: input.selection.profile.clone(),
		game_version: input.selection.game_version.clone(),
		steam_build_id: input.selection.steam_build_id,
		base_snapshot_identity: input.base_snapshot_identity,
		executable_blake3: input.executable_blake3,
		legacy_baseline_blake3: input.selection.legacy_baseline_blake3.clone(),
		expected_verdicts_blake3: input.selection.expected_verdicts_blake3.clone(),
		wiki_knowledge_snapshot_id: input.wiki_knowledge_snapshot_id,
		case_count: input.selection.cases.len(),
		legacy_unit_count: input.selection.legacy_unit_count,
		structured_unit_count: input.selection.structured_unit_count,
		case_runs: input.case_runs,
		units: unit_artifacts,
		summary: artifact_for(root, "summary.json")?,
		proposals: artifact_for(root, "proposals.jsonl")?,
	};
	manifest.case_runs.sort_by(case_run_order);
	write_json(&root.join("manifest.json"), &manifest)?;
	let staging_path = staging.keep();
	fs::rename(staging_path, output_dir)?;
	Ok(ReviewPackBuildResult {
		manifest,
		summary,
		units: input.units,
		proposals,
	})
}

fn validate_pack_local_objects(
	input: &PrecomputedReviewPack,
	pack_objects: Option<&Path>,
) -> ReviewResult<()> {
	let required_hashes = input
		.case_runs
		.iter()
		.filter(|run| run.kernel == ReviewKernel::Structured)
		.filter_map(|run| run.output_cas_hash.as_deref())
		.chain(
			input
				.units
				.iter()
				.filter(|unit| unit.kernel == ReviewKernel::Structured)
				.filter_map(|unit| unit.output_cas_hash.as_deref()),
		)
		.collect::<BTreeSet<_>>();
	if required_hashes.is_empty() {
		return Ok(());
	}
	for hash in &required_hashes {
		validate_hash("Structured pack-local output CAS hash", hash)?;
	}
	let pack_objects = pack_objects.ok_or_else(|| {
		ReviewPackError::Invalid(
			"Structured output hashes require verified pack-local CAS objects".to_string(),
		)
	})?;
	if !pack_objects.is_dir() {
		return invalid(format!(
			"pack-local CAS object directory does not exist: {}",
			pack_objects.display()
		));
	}
	let store = ObjectStore::new(pack_objects, pack_objects.join(".verification-work"));
	for hash in required_hashes {
		store.verify_object(hash)?;
	}
	Ok(())
}

pub fn verify_review_pack(
	options: &ReviewPackVerifyOptions<'_>,
) -> ReviewResult<ReviewPackVerifyResult> {
	let selection = load_and_validate_inputs(
		options.selection,
		options.legacy_baseline,
		options.expected_verdicts,
	)?;
	validate_selection_game(&selection, options.game)?;
	let manifest = read_json::<ReviewPackManifest>(&options.pack_dir.join("manifest.json"))?;
	validate_manifest_against_selection(&manifest, &selection)?;
	let dataset_paths = DatasetPaths::new(options.dataset_root);
	let snapshots = selected_snapshots(&selection, &dataset_paths, options.game)?;
	let store = ObjectStore::new(&dataset_paths.objects, &dataset_paths.work);
	let pack_store = ObjectStore::new(
		options.pack_dir.join("objects"),
		options.pack_dir.join(".verification-work"),
	);
	let baseline = load_legacy_baseline(options.legacy_baseline, options.expected_verdicts)?;
	let legacy_cases = load_pinned_legacy_cases(&selection, &dataset_paths, &store)?;

	if manifest.units.len() != selection.legacy_unit_count + selection.structured_unit_count {
		return invalid(format!(
			"manifest unit artifact count drifted: expected {}, found {}",
			selection.legacy_unit_count + selection.structured_unit_count,
			manifest.units.len()
		));
	}
	verify_artifact(options.pack_dir, &manifest.summary)?;
	verify_artifact(options.pack_dir, &manifest.proposals)?;
	let summary = read_json::<ReviewPackSummary>(&options.pack_dir.join("summary.json"))?;
	if summary.review_pack_id != manifest.review_pack_id {
		return invalid("summary review_pack_id does not match manifest");
	}

	let mut units = Vec::with_capacity(manifest.units.len());
	let mut seen_artifact_paths = BTreeSet::new();
	for artifact in &manifest.units {
		if !seen_artifact_paths.insert(artifact.artifact.path.as_str()) {
			return invalid(format!(
				"manifest contains duplicate unit artifact {}",
				artifact.artifact.path
			));
		}
		verify_artifact(options.pack_dir, &artifact.artifact)?;
		let unit =
			read_json::<ReviewPackUnitEvidence>(&options.pack_dir.join(&artifact.artifact.path))?;
		if unit.scoring_unit_id != artifact.scoring_unit_id
			|| unit.case_id != artifact.case_id
			|| unit.relative_path != artifact.relative_path
			|| unit.kernel != artifact.kernel
		{
			return invalid(format!(
				"unit artifact binding mismatch for {}",
				artifact.artifact.path
			));
		}
		validate_unit_binding(&unit, &manifest, &selection)?;
		units.push(unit);
	}
	units.sort_by(unit_order);

	let expected_base = verified_retained_base_snapshot(
		&selection.game_version,
		Some(&manifest.base_snapshot_identity),
		&selection
			.cases
			.iter()
			.flat_map(|case| case.legacy_units.iter().cloned())
			.collect(),
	)?;
	if expected_base.identity != manifest.base_snapshot_identity {
		return invalid("installed base snapshot identity changed");
	}

	let mut loaded = BTreeMap::new();
	let mut cas_hashes = BTreeSet::new();
	for case in &selection.cases {
		let snapshot = snapshots
			.get(&case.case_id)
			.expect("selected snapshots were validated")
			.clone();
		cas_hashes.insert(snapshot.compatch.content_hash.clone());
		cas_hashes.extend(
			snapshot
				.source_mods
				.iter()
				.map(|source| source.content_hash.clone()),
		);
		loaded.insert(case.case_id.clone(), open_snapshot(&store, snapshot)?);
	}
	for run in &manifest.case_runs {
		if run.kernel == ReviewKernel::Structured
			&& let Some(output_hash) = &run.output_cas_hash
		{
			pack_store.verify_object(output_hash)?;
			cas_hashes.insert(output_hash.clone());
		}
	}
	let mut score_cache = ScoreCache::new();
	let verification = ReviewVerificationContext {
		dataset_store: &store,
		pack_store: &pack_store,
		game: options.game,
		baseline: &baseline,
		legacy_cases: &legacy_cases,
	};
	for unit in &units {
		if let Some(output_hash) = &unit.output_cas_hash {
			match unit.kernel {
				ReviewKernel::Legacy => {
					store.verify_once(output_hash)?;
				}
				ReviewKernel::Structured => {
					pack_store.verify_object(output_hash)?;
				}
			}
			cas_hashes.insert(output_hash.clone());
		}
		recompute_unit(
			unit,
			loaded
				.get(&unit.case_id)
				.expect("unit selection was validated"),
			&verification,
			&mut score_cache,
		)?;
	}

	let proposals = read_jsonl::<ReviewAnnotation>(&options.pack_dir.join("proposals.jsonl"))?;
	if proposals.len() != units.len() {
		return invalid(format!(
			"proposal count drifted: expected {}, found {}",
			units.len(),
			proposals.len()
		));
	}
	let units_by_id = units
		.iter()
		.map(|unit| (unit.scoring_unit_id.as_str(), unit))
		.collect::<BTreeMap<_, _>>();
	let mut proposal_ids = BTreeSet::new();
	for proposal in &proposals {
		proposal
			.validate()
			.map_err(|error| ReviewPackError::Invalid(error.to_string()))?;
		if !proposal_ids.insert(proposal.annotation_id.as_str()) {
			return invalid(format!(
				"duplicate proposal annotation {}",
				proposal.annotation_id
			));
		}
		let unit = units_by_id
			.get(proposal.scoring_unit_id.as_str())
			.ok_or_else(|| {
				ReviewPackError::Invalid(format!(
					"proposal {} binds an unknown scoring unit",
					proposal.annotation_id
				))
			})?;
		let expected = proposal_for_unit(unit)?;
		if proposal != &expected {
			return invalid(format!(
				"proposal {} is stale or tampered",
				proposal.annotation_id
			));
		}
	}

	let reconstructed = PrecomputedReviewPack {
		selection: selection.clone(),
		base_snapshot_identity: manifest.base_snapshot_identity.clone(),
		executable_blake3: manifest.executable_blake3.clone(),
		wiki_knowledge_snapshot_id: manifest.wiki_knowledge_snapshot_id.clone(),
		case_runs: manifest.case_runs.clone(),
		units: units.clone(),
	};
	if review_pack_id(&reconstructed)? != manifest.review_pack_id {
		return invalid("review_pack_id does not match verified pack contents");
	}
	let expected_summary = summarize_pack(
		&manifest.review_pack_id,
		&manifest.case_runs,
		&units,
		&proposals,
	);
	if summary != expected_summary {
		return invalid("summary does not match verified units and proposals");
	}
	Ok(ReviewPackVerifyResult {
		review_pack_id: manifest.review_pack_id,
		units_verified: units.len(),
		cas_objects_verified: cas_hashes.len(),
		proposals_verified: proposals.len(),
	})
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyBaselineFile {
	schema: String,
	scorer_version: String,
	expected_content_id: String,
	units: Vec<LegacyBaselineUnit>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyBaselineUnit {
	case_id: String,
	score: FileRecord,
}

struct LegacyBaseline {
	scorer_version: String,
	units: BTreeMap<(String, String), FileRecord>,
}

#[derive(Debug)]
struct LegacyCase {
	measurement_id: String,
	output_cas_hash: String,
	elapsed_ms: u64,
}

fn load_and_validate_inputs(
	selection_path: &Path,
	baseline_path: &Path,
	expected_path: &Path,
) -> ReviewResult<ReviewPackSelection> {
	let selection = ReviewPackSelection::from_path(selection_path)?;
	let baseline_hash = hash_file(baseline_path)?;
	if baseline_hash != selection.legacy_baseline_blake3 {
		return invalid(format!(
			"Legacy baseline BLAKE3 mismatch: selection={} actual={baseline_hash}",
			selection.legacy_baseline_blake3
		));
	}
	let expected_hash = hash_file(expected_path)?;
	if expected_hash != selection.expected_verdicts_blake3 {
		return invalid(format!(
			"expected verdicts BLAKE3 mismatch: selection={} actual={expected_hash}",
			selection.expected_verdicts_blake3
		));
	}
	let baseline = load_legacy_baseline(baseline_path, expected_path)?;
	let selected = selection
		.cases
		.iter()
		.flat_map(|case| {
			case.legacy_units
				.iter()
				.map(|path| (case.case_id.clone(), path.clone()))
		})
		.collect::<BTreeSet<_>>();
	let frozen = baseline.units.keys().cloned().collect::<BTreeSet<_>>();
	if selected != frozen {
		return invalid(
			"review-pack Legacy selection does not exactly match frozen baseline units",
		);
	}
	Ok(selection)
}

fn load_legacy_baseline(
	baseline_path: &Path,
	expected_path: &Path,
) -> ReviewResult<LegacyBaseline> {
	let baseline_bytes = fs::read(baseline_path)?;
	let expected_bytes = fs::read(expected_path)?;
	let baseline = serde_json::from_slice::<LegacyBaselineFile>(&baseline_bytes)?;
	let expected =
		serde_json::from_slice::<BTreeMap<String, BTreeMap<String, usize>>>(&expected_bytes)?;
	if baseline.schema != REVIEW_PACK_SCHEMA {
		return invalid(format!(
			"unsupported Legacy baseline schema {}",
			baseline.schema
		));
	}
	validate_required("Legacy baseline scorer_version", &baseline.scorer_version)?;
	let expected_content_id = stable_id("legacy-expected-v1", &[&expected_bytes]);
	if baseline.expected_content_id != expected_content_id {
		return invalid("Legacy baseline is not bound to expected verdicts");
	}
	if baseline.units.len() != REVIEW_PACK_LEGACY_UNIT_COUNT {
		return invalid(format!(
			"Legacy baseline denominator drifted: expected {REVIEW_PACK_LEGACY_UNIT_COUNT}, found {}",
			baseline.units.len()
		));
	}
	let mut actual = BTreeMap::<String, BTreeMap<String, usize>>::new();
	let mut units = BTreeMap::new();
	for unit in baseline.units {
		if !unit.score.multi_source {
			return invalid(format!(
				"Legacy baseline unit {}:{} is not multi-source",
				unit.case_id, unit.score.rel
			));
		}
		*actual
			.entry(unit.case_id.clone())
			.or_default()
			.entry(unit.score.verdict.clone())
			.or_default() += 1;
		let key = (unit.case_id, unit.score.rel.clone());
		if units.insert(key.clone(), unit.score).is_some() {
			return invalid(format!(
				"Legacy baseline contains duplicate unit {}:{}",
				key.0, key.1
			));
		}
	}
	if actual != expected {
		return invalid("Legacy baseline does not reproduce expected verdict counts");
	}
	Ok(LegacyBaseline {
		scorer_version: baseline.scorer_version,
		units,
	})
}

fn validate_selection_game(
	selection: &ReviewPackSelection,
	game: &Eu4GameDiscovery,
) -> ReviewResult<()> {
	if selection.game_version != game.game_version {
		return invalid(format!(
			"review-pack game version mismatch: selection={} local={}",
			selection.game_version, game.game_version
		));
	}
	if game.steam_build_id != Some(selection.steam_build_id) {
		return invalid(format!(
			"review-pack Steam build mismatch: selection={} local={:?}",
			selection.steam_build_id, game.steam_build_id
		));
	}
	Ok(())
}

fn selected_snapshots(
	selection: &ReviewPackSelection,
	paths: &DatasetPaths,
	game: &Eu4GameDiscovery,
) -> ReviewResult<BTreeMap<String, SnapshotRecord>> {
	let snapshots = read_jsonl::<SnapshotRecord>(&paths.snapshots)?;
	let mut by_id = BTreeMap::new();
	for snapshot in snapshots {
		let snapshot_id = snapshot.snapshot_id.clone();
		if by_id.insert(snapshot_id.clone(), snapshot).is_some() {
			return invalid(format!("dataset contains duplicate snapshot {snapshot_id}"));
		}
	}
	let mut selected = BTreeMap::new();
	for case in &selection.cases {
		let snapshot = by_id.get(&case.snapshot_id).ok_or_else(|| {
			ReviewPackError::Invalid(format!(
				"pinned dataset snapshot is missing for case {}: {}",
				case.case_id, case.snapshot_id
			))
		})?;
		if !snapshot.identity_is_valid() {
			return invalid(format!(
				"pinned snapshot has an invalid identity for case {}",
				case.case_id
			));
		}
		if snapshot.case_id != case.case_id {
			return invalid(format!(
				"pinned snapshot {} belongs to case {}, not {}",
				case.snapshot_id, snapshot.case_id, case.case_id
			));
		}
		validate_snapshot_game(snapshot, game)
			.map_err(|error| ReviewPackError::Invalid(error.to_string()))?;
		selected.insert(case.case_id.clone(), snapshot.clone());
	}
	Ok(selected)
}

fn load_pinned_legacy_cases(
	selection: &ReviewPackSelection,
	paths: &DatasetPaths,
	store: &ObjectStore,
) -> ReviewResult<BTreeMap<String, LegacyCase>> {
	let measurements = read_jsonl::<MeasurementRecord>(&paths.measurements)?;
	let registry = crate::report::committed_measurement_cohort_registry()?;
	let expected_cohort = review_pack_legacy_cohort_key();
	let registered = registry
		.cohorts
		.iter()
		.find(|cohort| cohort.identity == expected_cohort)
		.ok_or_else(|| {
			ReviewPackError::Invalid(
				"the exact review-pack Legacy scorer 1.3.0 cohort is not registered".to_string(),
			)
		})?;
	if registered.merge_kernel != MeasurementKernel::LegacyAddressPatchReference {
		return invalid(
			"the exact review-pack Legacy scorer 1.3.0 cohort has the wrong kernel contract",
		);
	}
	let mut by_id = BTreeMap::new();
	for measurement in &measurements {
		if by_id
			.insert(measurement.measurement_id(), measurement)
			.is_some()
		{
			return invalid(format!(
				"dataset contains duplicate measurement {}",
				measurement.measurement_id()
			));
		}
	}
	let mut selected = BTreeMap::new();
	for case in &selection.cases {
		let measurement = by_id
			.get(case.legacy_measurement_id.as_str())
			.ok_or_else(|| {
				ReviewPackError::Invalid(format!(
					"pinned Legacy measurement is missing for case {}: {}",
					case.case_id, case.legacy_measurement_id
				))
			})?;
		if !measurement.identity_is_valid() {
			return invalid(format!(
				"pinned Legacy measurement has an invalid identity for case {}",
				case.case_id
			));
		}
		if measurement.cohort_key() != expected_cohort {
			return invalid(format!(
				"pinned Legacy measurement does not match the exact scorer 1.3.0 executable/config cohort for case {}",
				case.case_id
			));
		}
		if measurement.snapshot_id() != case.snapshot_id
			|| measurement.status() != TerminalStatus::Completed
			|| measurement.merged_output_hash() != Some(case.legacy_output_hash.as_str())
		{
			return invalid(format!(
				"pinned Legacy measurement/output binding changed for case {}",
				case.case_id
			));
		}
		store.verify_once(&case.legacy_output_hash)?;
		selected.insert(
			case.case_id.clone(),
			LegacyCase {
				measurement_id: case.legacy_measurement_id.clone(),
				output_cas_hash: case.legacy_output_hash.clone(),
				elapsed_ms: measurement.summary().map_or(0, |summary| summary.total_ms),
			},
		);
	}
	Ok(selected)
}

fn review_pack_legacy_cohort_key() -> MeasurementCohortKey {
	MeasurementCohortKey::OrchestratorBoundV1 {
		executable_hash: REVIEW_PACK_LEGACY_EXECUTABLE_HASH.to_string(),
		scorer_version: REVIEW_PACK_LEGACY_SCORER_VERSION.to_string(),
		config_hash: REVIEW_PACK_LEGACY_CONFIG_HASH.to_string(),
	}
}

fn source_mods(loaded: &LoadedSnapshot) -> Vec<SourceMod<'_>> {
	loaded
		.snapshot
		.source_mods
		.iter()
		.zip(&loaded.source_dirs)
		.map(|(source, root)| SourceMod {
			id: &source.workshop_id,
			root,
		})
		.collect()
}

fn raw_cas_binding(snapshot: &SnapshotRecord) -> ReviewPackRawCasBinding {
	ReviewPackRawCasBinding {
		compatch_content_hash: snapshot.compatch.content_hash.clone(),
		source_content_hashes: snapshot
			.source_mods
			.iter()
			.map(|source| source.content_hash.clone())
			.collect(),
	}
}

fn semantic_hashes(
	evidence: &ReviewSemanticEvidence,
	candidate_available: bool,
	snapshot_id: &str,
	relative_path: &str,
	kernel: ReviewKernel,
) -> ReviewPackSemanticHashes {
	let candidate = if candidate_available {
		evidence.candidate.semantic_content_id.clone()
	} else {
		unavailable_candidate_hash(snapshot_id, relative_path, kernel)
	};
	ReviewPackSemanticHashes {
		base: evidence.base.semantic_content_id.clone(),
		sources: evidence
			.sources
			.iter()
			.map(|source| source.layer.semantic_content_id.clone())
			.collect(),
		human: evidence.human.semantic_content_id.clone(),
		candidate,
		candidate_available,
	}
}

fn unavailable_candidate_hash(snapshot_id: &str, path: &str, kernel: ReviewKernel) -> String {
	let kernel = serde_json::to_vec(&kernel).expect("ReviewKernel serializes");
	stable_id(
		"review-pack-unavailable-candidate-v1",
		&[snapshot_id.as_bytes(), path.as_bytes(), &kernel],
	)
}

fn exact_semantic_equivalence(evidence: &ReviewSemanticEvidence) -> bool {
	evidence.candidate_vs_human.left_only.is_empty()
		&& evidence.candidate_vs_human.right_only.is_empty()
}

fn scoring_unit_id(snapshot_id: &str, relative_path: &str, kernel: ReviewKernel) -> String {
	let kernel = serde_json::to_vec(&kernel).expect("ReviewKernel serializes");
	stable_id(
		"review-pack-scoring-unit-v1",
		&[snapshot_id.as_bytes(), relative_path.as_bytes(), &kernel],
	)
}

fn proposal_for_unit(unit: &ReviewPackUnitEvidence) -> ReviewResult<ReviewAnnotation> {
	let equivalent = unit.semantic_hashes.candidate_available && unit.ast_relation.is_equivalent();
	let label = if equivalent {
		ReviewLabel::Equivalent
	} else {
		ReviewLabel::InsufficientEvidence
	};
	ReviewAnnotation::new(ReviewAnnotationDraft {
		kind: ReviewRecordKind::Proposal,
		status: ReviewStatus::Proposed,
		label,
		ast_relation: unit.ast_relation,
		binding: unit.binding(),
		supersedes: None,
		reviewer: None,
		model: None,
		provenance: Some("review-pack-build".to_string()),
		reason: (!equivalent).then(|| {
			if unit.semantic_hashes.candidate_available {
				"candidate is not exactly AST/module-semantic equivalent to the human output"
					.to_string()
			} else {
				"grouped Structured run did not produce valid candidate evidence".to_string()
			}
		}),
		family_invariants: Vec::new(),
		runtime_evidence: None,
	})
	.map_err(|error| ReviewPackError::Invalid(error.to_string()))
}

#[derive(Serialize)]
struct PackIdentity<'a> {
	schema: &'static str,
	profile: &'a str,
	game_version: &'a str,
	steam_build_id: u64,
	base_snapshot_identity: &'a str,
	executable_blake3: &'a str,
	legacy_baseline_blake3: &'a str,
	expected_verdicts_blake3: &'a str,
	wiki_knowledge_snapshot_id: Option<&'a str>,
	cases: Vec<CaseRunIdentity<'a>>,
	units: Vec<UnitIdentity<'a>>,
}

#[derive(Serialize)]
struct CaseRunIdentity<'a> {
	case_id: &'a str,
	snapshot_id: &'a str,
	kernel: ReviewKernel,
	status: &'a str,
	output_valid: bool,
	output_cas_hash: Option<&'a str>,
	legacy_measurement_id: Option<&'a str>,
	structured_attestation: Option<&'a ReviewPackStructuredAttestation>,
	diagnostic_kinds: &'a [ShadowDiagnosticKind],
}

#[derive(Serialize)]
struct UnitIdentity<'a> {
	scoring_unit_id: &'a str,
	case_id: &'a str,
	relative_path: &'a str,
	snapshot_id: &'a str,
	kernel: ReviewKernel,
	raw_cas: &'a ReviewPackRawCasBinding,
	output_cas_hash: Option<&'a str>,
	semantic_hashes: &'a ReviewPackSemanticHashes,
	semantic_evidence: &'a Option<ReviewSemanticEvidence>,
	ast_relation: AstRelation,
	file_record: &'a Option<FileRecord>,
	diagnostics: &'a [ShadowDiagnostic],
	rollout_selection: &'a ReviewPackRolloutSelection,
	legacy_measurement_id: Option<&'a str>,
	shadow_comparison_id: Option<&'a str>,
}

fn review_pack_id(input: &PrecomputedReviewPack) -> ReviewResult<String> {
	let mut case_runs = input.case_runs.iter().collect::<Vec<_>>();
	case_runs.sort_by(|left, right| case_run_order(left, right));
	let mut units = input.units.iter().collect::<Vec<_>>();
	units.sort_by(|left, right| unit_order(left, right));
	let identity = PackIdentity {
		schema: REVIEW_PACK_SCHEMA,
		profile: &input.selection.profile,
		game_version: &input.selection.game_version,
		steam_build_id: input.selection.steam_build_id,
		base_snapshot_identity: &input.base_snapshot_identity,
		executable_blake3: &input.executable_blake3,
		legacy_baseline_blake3: &input.selection.legacy_baseline_blake3,
		expected_verdicts_blake3: &input.selection.expected_verdicts_blake3,
		wiki_knowledge_snapshot_id: input.wiki_knowledge_snapshot_id.as_deref(),
		cases: case_runs
			.into_iter()
			.map(|run| CaseRunIdentity {
				case_id: &run.case_id,
				snapshot_id: &run.snapshot_id,
				kernel: run.kernel,
				status: &run.status,
				output_valid: run.output_valid,
				output_cas_hash: run.output_cas_hash.as_deref(),
				legacy_measurement_id: run.legacy_measurement_id.as_deref(),
				structured_attestation: run.structured_attestation.as_ref(),
				diagnostic_kinds: &run.diagnostic_kinds,
			})
			.collect(),
		units: units
			.into_iter()
			.map(|unit| UnitIdentity {
				scoring_unit_id: &unit.scoring_unit_id,
				case_id: &unit.case_id,
				relative_path: &unit.relative_path,
				snapshot_id: &unit.snapshot_id,
				kernel: unit.kernel,
				raw_cas: &unit.raw_cas,
				output_cas_hash: unit.output_cas_hash.as_deref(),
				semantic_hashes: &unit.semantic_hashes,
				semantic_evidence: &unit.semantic_evidence,
				ast_relation: unit.ast_relation,
				file_record: &unit.file_record,
				diagnostics: &unit.diagnostics,
				rollout_selection: &unit.rollout_selection,
				legacy_measurement_id: unit.legacy_measurement_id.as_deref(),
				shadow_comparison_id: unit.shadow_comparison_id.as_deref(),
			})
			.collect(),
	};
	let encoded = serde_json::to_vec(&identity)?;
	Ok(stable_id("review-pack-v1", &[&encoded]))
}

fn validate_precomputed(input: &PrecomputedReviewPack) -> ReviewResult<()> {
	validate_required(
		"precomputed base_snapshot_identity",
		&input.base_snapshot_identity,
	)?;
	validate_hash("precomputed executable_blake3", &input.executable_blake3)?;
	if input.case_runs.len() != REVIEW_PACK_CASE_COUNT * 2 {
		return invalid(format!(
			"precomputed case-run count drifted: expected {}, found {}",
			REVIEW_PACK_CASE_COUNT * 2,
			input.case_runs.len()
		));
	}
	let cases = input
		.selection
		.cases
		.iter()
		.map(|case| (case.case_id.as_str(), case))
		.collect::<BTreeMap<_, _>>();
	let mut runs = BTreeMap::new();
	for run in &input.case_runs {
		let case = cases.get(run.case_id.as_str()).ok_or_else(|| {
			ReviewPackError::Invalid(format!(
				"precomputed case run binds unknown case {}",
				run.case_id
			))
		})?;
		if run.snapshot_id != case.snapshot_id {
			return invalid(format!(
				"precomputed case run binds stale snapshot for {}",
				run.case_id
			));
		}
		if let Some(hash) = &run.output_cas_hash {
			validate_hash("case-run output CAS hash", hash)?;
		}
		match run.kernel {
			ReviewKernel::Legacy => {
				if run.status != "reused_archived_measurement"
					|| !run.output_valid
					|| run.output_cas_hash.as_deref() != Some(case.legacy_output_hash.as_str())
					|| run.legacy_measurement_id.as_deref()
						!= Some(case.legacy_measurement_id.as_str())
					|| run.structured_attestation.is_some()
					|| !run.diagnostic_kinds.is_empty()
				{
					return invalid(format!(
						"precomputed Legacy run is not pinned for case {}",
						run.case_id
					));
				}
			}
			ReviewKernel::Structured => {
				validate_structured_case_run(run)?;
				if run.legacy_measurement_id.is_some() {
					return invalid(format!(
						"precomputed Structured run is inconsistent for case {}",
						run.case_id
					));
				}
			}
		}
		if runs
			.insert((run.case_id.as_str(), run.kernel), run)
			.is_some()
		{
			return invalid(format!(
				"precomputed case run is duplicated for {}:{:?}",
				run.case_id, run.kernel
			));
		}
	}
	let legacy = input
		.units
		.iter()
		.filter(|unit| unit.kernel == ReviewKernel::Legacy)
		.count();
	let structured = input
		.units
		.iter()
		.filter(|unit| unit.kernel == ReviewKernel::Structured)
		.count();
	if legacy != REVIEW_PACK_LEGACY_UNIT_COUNT || structured != REVIEW_PACK_STRUCTURED_UNIT_COUNT {
		return invalid(format!(
			"precomputed unit denominator drifted: Legacy={legacy}, Structured={structured}"
		));
	}
	let mut keys = BTreeSet::new();
	for unit in &input.units {
		if unit.schema != REVIEW_PACK_SCHEMA {
			return invalid(format!("unsupported unit evidence schema {}", unit.schema));
		}
		validate_relative_path(&unit.relative_path)?;
		validate_hash(
			"unit raw compatch CAS hash",
			&unit.raw_cas.compatch_content_hash,
		)?;
		for hash in &unit.raw_cas.source_content_hashes {
			validate_hash("unit raw source CAS hash", hash)?;
		}
		validate_hash("unit base semantic hash", &unit.semantic_hashes.base)?;
		for hash in &unit.semantic_hashes.sources {
			validate_hash("unit source semantic hash", hash)?;
		}
		validate_hash("unit human semantic hash", &unit.semantic_hashes.human)?;
		validate_hash(
			"unit candidate semantic hash",
			&unit.semantic_hashes.candidate,
		)?;
		if let Some(hash) = &unit.output_cas_hash {
			validate_hash("unit output CAS hash", hash)?;
		}
		if unit.base_snapshot_identity != input.base_snapshot_identity
			|| unit.wiki_knowledge_snapshot_id != input.wiki_knowledge_snapshot_id
		{
			return invalid(format!(
				"precomputed unit has stale base/knowledge binding for {}:{}",
				unit.case_id, unit.relative_path
			));
		}
		let case = cases.get(unit.case_id.as_str()).ok_or_else(|| {
			ReviewPackError::Invalid(format!(
				"precomputed unit binds unknown case {}",
				unit.case_id
			))
		})?;
		if unit.snapshot_id != case.snapshot_id {
			return invalid(format!(
				"precomputed unit binds stale snapshot for {}:{}",
				unit.case_id, unit.relative_path
			));
		}
		let run = runs
			.get(&(unit.case_id.as_str(), unit.kernel))
			.expect("precomputed runs contain both kernels for every selected case");
		let expected_shadow_id = run
			.structured_attestation
			.as_ref()
			.map(|attestation| attestation.comparison_id.as_str());
		let binding_valid = match unit.kernel {
			ReviewKernel::Legacy => {
				unit.output_cas_hash.as_deref() == Some(case.legacy_output_hash.as_str())
					&& unit.legacy_measurement_id.as_deref()
						== Some(case.legacy_measurement_id.as_str())
					&& unit.shadow_comparison_id.is_none()
			}
			ReviewKernel::Structured => {
				unit.output_cas_hash == run.output_cas_hash
					&& unit.legacy_measurement_id.is_none()
					&& unit.shadow_comparison_id.as_deref() == expected_shadow_id
			}
		};
		if !binding_valid
			|| unit.semantic_hashes.candidate_available != unit.output_cas_hash.is_some()
			|| unit.semantic_evidence.is_some() != unit.output_cas_hash.is_some()
			|| unit.file_record.is_some() != unit.output_cas_hash.is_some()
		{
			return invalid(format!(
				"precomputed unit evidence is inconsistent for {}:{}:{:?}",
				unit.case_id, unit.relative_path, unit.kernel
			));
		}
		if unit.kernel == ReviewKernel::Structured
			&& unit
				.diagnostics
				.iter()
				.any(|diagnostic| is_blocking_structured_diagnostic(diagnostic.kind))
		{
			return invalid(format!(
				"precomputed Structured unit contains blocking diagnostics for {}:{}",
				unit.case_id, unit.relative_path
			));
		}
		if let Some(evidence) = &unit.semantic_evidence {
			let hashes = semantic_hashes(
				evidence,
				true,
				&unit.snapshot_id,
				&unit.relative_path,
				unit.kernel,
			);
			let raw_equivalent = exact_semantic_equivalence(evidence);
			let relation_is_valid = if raw_equivalent {
				unit.ast_relation == AstRelation::ExactEquivalent
			} else if unit.kernel == ReviewKernel::Structured {
				matches!(
					unit.ast_relation,
					AstRelation::LogicalEquivalent | AstRelation::Nonidentical
				)
			} else {
				unit.ast_relation == AstRelation::Nonidentical
			};
			if unit.semantic_hashes != hashes || !relation_is_valid {
				return invalid(format!(
					"precomputed semantic evidence is inconsistent for {}:{}:{:?}",
					unit.case_id, unit.relative_path, unit.kernel
				));
			}
		} else if unit.ast_relation != AstRelation::Nonidentical {
			return invalid(format!(
				"unavailable precomputed candidate cannot be AST-equivalent for {}:{}:{:?}",
				unit.case_id, unit.relative_path, unit.kernel
			));
		}
		if !keys.insert((
			unit.case_id.as_str(),
			unit.relative_path.as_str(),
			unit.kernel,
		)) {
			return invalid(format!(
				"precomputed evidence contains duplicate unit {}:{}:{:?}",
				unit.case_id, unit.relative_path, unit.kernel
			));
		}
		let expected = scoring_unit_id(&unit.snapshot_id, &unit.relative_path, unit.kernel);
		if unit.scoring_unit_id != expected {
			return invalid(format!(
				"scoring_unit_id mismatch for {}:{}:{:?}",
				unit.case_id, unit.relative_path, unit.kernel
			));
		}
	}
	let selected = input
		.selection
		.cases
		.iter()
		.flat_map(|case| {
			case.legacy_units
				.iter()
				.map(|path| (case.case_id.as_str(), path.as_str(), ReviewKernel::Legacy))
				.chain(case.structured_units.iter().map(|path| {
					(
						case.case_id.as_str(),
						path.as_str(),
						ReviewKernel::Structured,
					)
				}))
		})
		.collect::<BTreeSet<_>>();
	if selected != keys {
		return invalid("precomputed unit bindings do not exactly match selection");
	}
	Ok(())
}

fn summarize_pack(
	review_pack_id: &str,
	case_runs: &[ReviewPackCaseRun],
	units: &[ReviewPackUnitEvidence],
	proposals: &[ReviewAnnotation],
) -> ReviewPackSummary {
	ReviewPackSummary {
		schema: REVIEW_PACK_SCHEMA.to_string(),
		review_pack_id: review_pack_id.to_string(),
		case_count: case_runs
			.iter()
			.map(|run| run.case_id.as_str())
			.collect::<BTreeSet<_>>()
			.len(),
		total_units: units.len(),
		legacy_units: units
			.iter()
			.filter(|unit| unit.kernel == ReviewKernel::Legacy)
			.count(),
		structured_units: units
			.iter()
			.filter(|unit| unit.kernel == ReviewKernel::Structured)
			.count(),
		legacy_executions: 0,
		structured_executions: case_runs
			.iter()
			.filter(|run| run.kernel == ReviewKernel::Structured)
			.count(),
		structured_valid_cases: case_runs
			.iter()
			.filter(|run| run.kernel == ReviewKernel::Structured && run.output_valid)
			.count(),
		structured_invalid_cases: case_runs
			.iter()
			.filter(|run| run.kernel == ReviewKernel::Structured && !run.output_valid)
			.count(),
		equivalent_proposals: proposals
			.iter()
			.filter(|proposal| proposal.label == ReviewLabel::Equivalent)
			.count(),
		insufficient_evidence_proposals: proposals
			.iter()
			.filter(|proposal| proposal.label == ReviewLabel::InsufficientEvidence)
			.count(),
	}
}

fn validate_manifest_against_selection(
	manifest: &ReviewPackManifest,
	selection: &ReviewPackSelection,
) -> ReviewResult<()> {
	if manifest.schema != REVIEW_PACK_SCHEMA
		|| manifest.profile != selection.profile
		|| manifest.game_version != selection.game_version
		|| manifest.steam_build_id != selection.steam_build_id
		|| manifest.legacy_baseline_blake3 != selection.legacy_baseline_blake3
		|| manifest.expected_verdicts_blake3 != selection.expected_verdicts_blake3
		|| manifest.case_count != selection.cases.len()
		|| manifest.legacy_unit_count != selection.legacy_unit_count
		|| manifest.structured_unit_count != selection.structured_unit_count
	{
		return invalid("review-pack manifest does not match pinned selection");
	}
	if manifest.case_runs.len() != REVIEW_PACK_CASE_COUNT * 2 {
		return invalid("review-pack manifest case-run count is invalid");
	}
	let mut expected_runs = BTreeSet::new();
	for case in &selection.cases {
		expected_runs.insert((
			case.case_id.as_str(),
			case.snapshot_id.as_str(),
			ReviewKernel::Legacy,
		));
		expected_runs.insert((
			case.case_id.as_str(),
			case.snapshot_id.as_str(),
			ReviewKernel::Structured,
		));
	}
	let actual_runs = manifest
		.case_runs
		.iter()
		.map(|run| (run.case_id.as_str(), run.snapshot_id.as_str(), run.kernel))
		.collect::<BTreeSet<_>>();
	if expected_runs != actual_runs {
		return invalid("review-pack manifest case runs do not match selection");
	}
	for run in &manifest.case_runs {
		let selected_case = selection
			.cases
			.iter()
			.find(|case| case.case_id == run.case_id)
			.expect("case-run identities were validated above");
		match run.kernel {
			ReviewKernel::Legacy => {
				if run.status != "reused_archived_measurement"
					|| !run.output_valid
					|| run.legacy_measurement_id.as_deref()
						!= Some(selected_case.legacy_measurement_id.as_str())
					|| run.output_cas_hash.as_deref()
						!= Some(selected_case.legacy_output_hash.as_str())
					|| run.structured_attestation.is_some()
					|| !run.diagnostic_kinds.is_empty()
				{
					return invalid(format!(
						"Legacy case run {} is not an archived-measurement reuse",
						run.case_id
					));
				}
			}
			ReviewKernel::Structured => {
				validate_structured_case_run(run)?;
				if run.legacy_measurement_id.is_some() {
					return invalid(format!(
						"Structured case run {} has an invalid captured manifest",
						run.case_id
					));
				}
			}
		}
	}
	Ok(())
}

fn validate_unit_binding(
	unit: &ReviewPackUnitEvidence,
	manifest: &ReviewPackManifest,
	selection: &ReviewPackSelection,
) -> ReviewResult<()> {
	if unit.schema != REVIEW_PACK_SCHEMA || unit.review_pack_id != manifest.review_pack_id {
		return invalid(format!(
			"unit {} has stale schema or review_pack_id",
			unit.scoring_unit_id
		));
	}
	if unit.base_snapshot_identity != manifest.base_snapshot_identity
		|| unit.wiki_knowledge_snapshot_id != manifest.wiki_knowledge_snapshot_id
	{
		return invalid(format!(
			"unit {} has stale base/knowledge binding",
			unit.scoring_unit_id
		));
	}
	let case = selection
		.cases
		.iter()
		.find(|case| case.case_id == unit.case_id)
		.ok_or_else(|| {
			ReviewPackError::Invalid(format!(
				"unit {} binds unknown case {}",
				unit.scoring_unit_id, unit.case_id
			))
		})?;
	if unit.snapshot_id != case.snapshot_id {
		return invalid(format!(
			"unit {} binds stale snapshot",
			unit.scoring_unit_id
		));
	}
	let selected = match unit.kernel {
		ReviewKernel::Legacy => &case.legacy_units,
		ReviewKernel::Structured => &case.structured_units,
	};
	if !selected.contains(&unit.relative_path)
		|| !unit.rollout_selection.selected_by_fixture
		|| unit.rollout_selection.selected_kernel != unit.kernel
	{
		return invalid(format!(
			"unit {} is not selected by the pinned rollout",
			unit.scoring_unit_id
		));
	}
	if unit.scoring_unit_id != scoring_unit_id(&unit.snapshot_id, &unit.relative_path, unit.kernel)
	{
		return invalid(format!(
			"unit {} has invalid scoring_unit_id",
			unit.scoring_unit_id
		));
	}
	let case_run = manifest
		.case_runs
		.iter()
		.find(|run| run.case_id == unit.case_id && run.kernel == unit.kernel)
		.expect("manifest case-run identities were validated before unit bindings");
	match unit.kernel {
		ReviewKernel::Legacy => {
			if unit.legacy_measurement_id.as_deref() != Some(case.legacy_measurement_id.as_str())
				|| unit.shadow_comparison_id.is_some()
			{
				return invalid(format!(
					"unit {} has an invalid Legacy execution binding",
					unit.scoring_unit_id
				));
			}
		}
		ReviewKernel::Structured => {
			let comparison_id = case_run
				.structured_attestation
				.as_ref()
				.expect("Structured case run validation requires an attestation")
				.comparison_id
				.as_str();
			if unit.shadow_comparison_id.as_deref() != Some(comparison_id)
				|| unit.legacy_measurement_id.is_some()
				|| unit
					.diagnostics
					.iter()
					.any(|diagnostic| is_blocking_structured_diagnostic(diagnostic.kind))
			{
				return invalid(format!(
					"unit {} has invalid Structured execution evidence",
					unit.scoring_unit_id
				));
			}
		}
	}
	Ok(())
}

struct ReviewVerificationContext<'a> {
	dataset_store: &'a ObjectStore,
	pack_store: &'a ObjectStore,
	game: &'a Eu4GameDiscovery,
	baseline: &'a LegacyBaseline,
	legacy_cases: &'a BTreeMap<String, LegacyCase>,
}

fn recompute_unit(
	unit: &ReviewPackUnitEvidence,
	loaded: &LoadedSnapshot,
	verification: &ReviewVerificationContext<'_>,
	score_cache: &mut ScoreCache,
) -> ReviewResult<()> {
	if unit.raw_cas != raw_cas_binding(&loaded.snapshot) {
		return invalid(format!(
			"raw CAS binding changed for unit {}",
			unit.scoring_unit_id
		));
	}
	let legacy = verification
		.legacy_cases
		.get(&unit.case_id)
		.ok_or_else(|| ReviewPackError::Invalid(format!("missing Legacy case {}", unit.case_id)))?;
	let output = match unit.kernel {
		ReviewKernel::Legacy => {
			if unit.legacy_measurement_id.as_deref() != Some(&legacy.measurement_id)
				|| unit.output_cas_hash.as_deref() != Some(&legacy.output_cas_hash)
			{
				return invalid(format!(
					"Legacy measurement/output binding changed for unit {}",
					unit.scoring_unit_id
				));
			}
			Some(
				verification
					.dataset_store
					.verify_once(&legacy.output_cas_hash)?
					.tree,
			)
		}
		ReviewKernel::Structured => unit
			.output_cas_hash
			.as_deref()
			.map(|hash| {
				verification
					.pack_store
					.verify_object(hash)
					.map(|object| object.tree)
			})
			.transpose()?,
	};
	if unit.semantic_hashes.candidate_available != output.is_some()
		|| unit.semantic_evidence.is_some() != output.is_some()
	{
		return invalid(format!(
			"candidate availability is inconsistent for unit {}",
			unit.scoring_unit_id
		));
	}
	let sources = source_mods(loaded);
	let conflict_paths = unit
		.diagnostics
		.iter()
		.filter(|diagnostic| diagnostic.kind == ShadowDiagnosticKind::Conflict)
		.filter_map(|diagnostic| diagnostic.path.clone())
		.collect::<HashSet<_>>();
	let current_file_record = output.as_ref().map(|output| {
		FileRecord::from_score(score_file_with_cache_and_basegame(
			&ScoreFileRequest {
				rel: &unit.relative_path,
				source_mods: &sources,
				compatch: &loaded.compatch,
				out_dir: output,
				conflict_paths: &conflict_paths,
			},
			score_cache,
			Some(&verification.game.game_root),
		))
	});
	if unit.file_record.as_ref() != current_file_record.as_ref() {
		return invalid(format!(
			"current scorer result changed for unit {}",
			unit.scoring_unit_id
		));
	}
	let empty = tempfile::tempdir()?;
	let evidence = review_semantic_evidence_with_cache(
		&ScoreFileRequest {
			rel: &unit.relative_path,
			source_mods: &sources,
			compatch: &loaded.compatch,
			out_dir: output.as_deref().unwrap_or(empty.path()),
			conflict_paths: &HashSet::new(),
		},
		score_cache,
		Some(&verification.game.game_root),
	)
	.ok_or_else(|| {
		ReviewPackError::Invalid(format!(
			"cannot recompute semantic evidence for unit {}",
			unit.scoring_unit_id
		))
	})?;
	let expected_hashes = semantic_hashes(
		&evidence,
		output.is_some(),
		&unit.snapshot_id,
		&unit.relative_path,
		unit.kernel,
	);
	if unit.semantic_hashes != expected_hashes
		|| output
			.as_ref()
			.is_some_and(|_| unit.semantic_evidence.as_ref() != Some(&evidence))
	{
		return invalid(format!(
			"semantic evidence changed for unit {}",
			unit.scoring_unit_id
		));
	}
	let raw_equivalent = output
		.as_ref()
		.is_some_and(|_| exact_semantic_equivalence(&evidence));
	let (expected_relation, _) = evaluated_ast_relation(
		raw_equivalent,
		unit.kernel,
		&unit.relative_path,
		loaded,
		&verification.game.game_root,
		output.as_deref(),
	);
	if unit.ast_relation != expected_relation {
		return invalid(format!(
			"AST relation changed for unit {}",
			unit.scoring_unit_id
		));
	}
	if unit.kernel == ReviewKernel::Legacy {
		let frozen = verification
			.baseline
			.units
			.get(&(unit.case_id.clone(), unit.relative_path.clone()));
		if current_file_record.as_ref() != frozen {
			return invalid(format!(
				"pinned Legacy output no longer reproduces frozen baseline for unit {}: expected {frozen:?}, found {current_file_record:?}",
				unit.scoring_unit_id,
			));
		}
	}
	Ok(())
}

fn artifact_for(root: &Path, relative: &str) -> ReviewResult<ReviewPackArtifact> {
	validate_relative_path(relative)?;
	let path = root.join(relative);
	let bytes = fs::metadata(&path)?.len();
	Ok(ReviewPackArtifact {
		path: relative.to_string(),
		blake3: hash_file(&path)?,
		bytes,
	})
}

fn verify_artifact(root: &Path, artifact: &ReviewPackArtifact) -> ReviewResult<()> {
	validate_relative_path(&artifact.path)?;
	validate_hash("artifact blake3", &artifact.blake3)?;
	let path = root.join(&artifact.path);
	let metadata = fs::metadata(&path)?;
	if !metadata.is_file() || metadata.len() != artifact.bytes {
		return invalid(format!("artifact size/type mismatch: {}", artifact.path));
	}
	let actual = hash_file(&path)?;
	if actual != artifact.blake3 {
		return invalid(format!(
			"artifact BLAKE3 mismatch for {}: expected {}, found {actual}",
			artifact.path, artifact.blake3
		));
	}
	Ok(())
}

fn hash_file(path: &Path) -> ReviewResult<String> {
	Ok(blake3::hash(&fs::read(path)?).to_hex().to_string())
}

fn pretty_json_bytes<T: Serialize>(value: &T) -> ReviewResult<Vec<u8>> {
	let mut bytes = serde_json::to_vec_pretty(value)?;
	bytes.push(b'\n');
	Ok(bytes)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> ReviewResult<()> {
	let bytes = pretty_json_bytes(value)?;
	fs::write(path, bytes)?;
	Ok(())
}

fn write_jsonl<T: Serialize>(path: &Path, values: &[T]) -> ReviewResult<()> {
	let mut bytes = Vec::new();
	for value in values {
		serde_json::to_writer(&mut bytes, value)?;
		bytes.push(b'\n');
	}
	fs::write(path, bytes)?;
	Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> ReviewResult<T> {
	Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn unit_order(left: &ReviewPackUnitEvidence, right: &ReviewPackUnitEvidence) -> std::cmp::Ordering {
	(&left.case_id, &left.relative_path, left.kernel).cmp(&(
		&right.case_id,
		&right.relative_path,
		right.kernel,
	))
}

fn case_run_order(left: &ReviewPackCaseRun, right: &ReviewPackCaseRun) -> std::cmp::Ordering {
	(&left.case_id, left.kernel).cmp(&(&right.case_id, right.kernel))
}

fn validate_relative_path(path: &str) -> ReviewResult<()> {
	let parsed = Path::new(path);
	let valid = !path.is_empty()
		&& !path.contains('\\')
		&& !path.contains("//")
		&& !path.ends_with('/')
		&& !parsed.is_absolute()
		&& parsed
			.components()
			.all(|component| matches!(component, Component::Normal(_)));
	if valid {
		Ok(())
	} else {
		invalid(format!("path is not normalized and relative: {path}"))
	}
}

fn validate_hash(field: &str, value: &str) -> ReviewResult<()> {
	let valid = value.len() == 64
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
	if valid {
		Ok(())
	} else {
		invalid(format!(
			"{field} is not a lowercase 64-character hex hash: {value}"
		))
	}
}

fn validate_required(field: &str, value: &str) -> ReviewResult<()> {
	if value.trim().is_empty() {
		invalid(format!("{field} must not be empty"))
	} else {
		Ok(())
	}
}

fn invalid<T>(detail: impl Into<String>) -> ReviewResult<T> {
	Err(ReviewPackError::Invalid(detail.into()))
}

#[cfg(test)]
mod tests {
	use std::cell::Cell;

	use super::*;
	use crate::dataset::{GameIdentity, SnapshotObjectRef};

	const FIXTURE: &str = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/tests/fixtures/review-pack-selection.json"
	);
	const LEGACY_BASELINE_FIXTURE: &str = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/tests/fixtures/legacy-baseline.json"
	);
	const EXPECTED_FIXTURE: &str =
		concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/expected.json");

	#[test]
	fn review_playset_makes_relative_mod_roots_absolute() {
		let root = tempfile::tempdir_in(".").expect("relative fixture root");
		let current = std::env::current_dir().expect("current directory");
		let relative_root = root
			.path()
			.strip_prefix(&current)
			.unwrap_or(root.path())
			.to_path_buf();
		let playset = tempfile::tempdir().expect("playset root");
		write_playset(
			playset.path(),
			&[("123".to_string(), relative_root.clone())],
		)
		.expect("write review playset");

		let descriptor = fs::read_to_string(playset.path().join("mod/ugc_123.mod"))
			.expect("read generated descriptor");
		let expected = current
			.join(relative_root)
			.to_string_lossy()
			.replace('\\', "/");
		assert!(descriptor.contains(&format!("path=\"{expected}\"")));
	}

	#[test]
	fn pinned_selection_is_exact_and_structured_is_a_legacy_subset() {
		let selection = ReviewPackSelection::from_path(Path::new(FIXTURE)).unwrap();
		assert_eq!(selection.cases.len(), REVIEW_PACK_CASE_COUNT);
		assert_eq!(selection.legacy_unit_count, REVIEW_PACK_LEGACY_UNIT_COUNT);
		assert_eq!(
			selection.structured_unit_count,
			REVIEW_PACK_STRUCTURED_UNIT_COUNT
		);
		for case in selection.cases {
			let legacy = case.legacy_units.into_iter().collect::<BTreeSet<_>>();
			assert!(
				case.structured_units
					.into_iter()
					.all(|path| legacy.contains(&path))
			);
		}
	}

	#[test]
	fn pinned_selection_uses_the_exact_legacy_scorer_1_3_cohort() {
		let selection = ReviewPackSelection::from_path(Path::new(FIXTURE)).unwrap();
		let dataset = Path::new(env!("CARGO_MANIFEST_DIR")).join("dataset/measurements.jsonl");
		let measurements = read_jsonl::<MeasurementRecord>(&dataset).unwrap();
		let by_id = measurements
			.iter()
			.map(|measurement| (measurement.measurement_id(), measurement))
			.collect::<BTreeMap<_, _>>();
		let expected_cohort = review_pack_legacy_cohort_key();

		for case in &selection.cases {
			let measurement = by_id
				.get(case.legacy_measurement_id.as_str())
				.expect("pinned Legacy measurement exists");
			assert_eq!(measurement.cohort_key(), expected_cohort);
			assert_eq!(
				measurement.scorer_version(),
				REVIEW_PACK_LEGACY_SCORER_VERSION
			);
			assert_eq!(measurement.config_hash(), REVIEW_PACK_LEGACY_CONFIG_HASH);
			assert_eq!(
				measurement.legacy_executable_hash(),
				Some(REVIEW_PACK_LEGACY_EXECUTABLE_HASH)
			);
			assert_eq!(measurement.snapshot_id(), case.snapshot_id);
			assert_eq!(
				measurement.merged_output_hash(),
				Some(case.legacy_output_hash.as_str())
			);
		}
	}

	#[test]
	fn selection_drift_is_rejected() {
		let mut selection = ReviewPackSelection::from_path(Path::new(FIXTURE)).unwrap();
		selection.cases[0]
			.structured_units
			.push("events/not-selected.txt".to_string());
		assert!(
			selection
				.validate()
				.unwrap_err()
				.to_string()
				.contains("not a Legacy subset")
		);
	}

	#[test]
	fn committed_historical_baseline_is_hash_bound_and_loadable() {
		let selection = load_and_validate_inputs(
			Path::new(FIXTURE),
			Path::new(LEGACY_BASELINE_FIXTURE),
			Path::new(EXPECTED_FIXTURE),
		)
		.unwrap();
		let baseline = load_legacy_baseline(
			Path::new(LEGACY_BASELINE_FIXTURE),
			Path::new(EXPECTED_FIXTURE),
		)
		.unwrap();

		assert_eq!(baseline.scorer_version, "1.3.0");
		assert_eq!(baseline.units.len(), REVIEW_PACK_LEGACY_UNIT_COUNT);
		assert_eq!(
			hash_file(Path::new(LEGACY_BASELINE_FIXTURE)).unwrap(),
			selection.legacy_baseline_blake3
		);
		assert_eq!(
			hash_file(Path::new(EXPECTED_FIXTURE)).unwrap(),
			selection.expected_verdicts_blake3
		);
	}

	struct SpyRunner {
		calls: Cell<usize>,
	}

	fn clean_structured_run() -> ShadowRunRecord {
		ShadowRunRecord {
			schema: SHADOW_COMPARE_SCHEMA.to_string(),
			comparison_id: "a".repeat(64),
			kernel: "structured".to_string(),
			output_dir: PathBuf::from("/Users/private/review-output"),
			output_valid: true,
			elapsed_ms: 1,
			status: "ready".to_string(),
			exit_code: Some(0),
			manual_conflict_count: Some(0),
			handler_resolution_count: Some(0),
			generated_file_count: Some(1),
			fatal_reason: None,
			error: None,
			diagnostics: Vec::new(),
		}
	}

	impl StructuredKernelRunner for SpyRunner {
		fn run_structured(
			&mut self,
			request: StructuredKernelRequest<'_>,
		) -> io::Result<ShadowRunRecord> {
			self.calls.set(self.calls.get() + 1);
			Ok(ShadowRunRecord {
				schema: crate::shadow::SHADOW_COMPARE_SCHEMA.to_string(),
				comparison_id: request.manifest.comparison_id.clone(),
				kernel: "structured".to_string(),
				output_dir: request.output_dir.to_path_buf(),
				output_valid: false,
				elapsed_ms: 1,
				status: "blocked".to_string(),
				exit_code: Some(0),
				manual_conflict_count: Some(1),
				handler_resolution_count: Some(0),
				generated_file_count: Some(0),
				fatal_reason: None,
				error: None,
				diagnostics: Vec::new(),
			})
		}
	}

	#[test]
	fn runner_surface_can_only_execute_structured_and_is_bounded_by_cases() {
		let runner = SpyRunner {
			calls: Cell::new(0),
		};
		for _ in 0..REVIEW_PACK_CASE_COUNT {
			runner.calls.set(runner.calls.get() + 1);
		}
		assert_eq!(runner.calls.get(), 6);
		// No Legacy method exists on StructuredKernelRunner.
		let _runner: &dyn StructuredKernelRunner = &runner;
	}

	#[test]
	fn structured_run_evidence_is_strictly_fail_closed() {
		let clean = clean_structured_run();
		assert!(validate_structured_run_evidence(&clean, "case").is_ok());

		let mut blocked = clean.clone();
		blocked.status = "blocked".to_string();
		assert!(validate_structured_run_evidence(&blocked, "case").is_err());
		let mut nonzero = clean.clone();
		nonzero.exit_code = Some(1);
		assert!(validate_structured_run_evidence(&nonzero, "case").is_err());
		let mut invalid_output = clean.clone();
		invalid_output.output_valid = false;
		assert!(validate_structured_run_evidence(&invalid_output, "case").is_err());
		let mut manual = clean.clone();
		manual.manual_conflict_count = Some(1);
		assert!(validate_structured_run_evidence(&manual, "case").is_err());
		let mut handler = clean.clone();
		handler.handler_resolution_count = Some(1);
		assert!(validate_structured_run_evidence(&handler, "case").is_err());
		for kind in [
			ShadowDiagnosticKind::Error,
			ShadowDiagnosticKind::Fatal,
			ShadowDiagnosticKind::Conflict,
			ShadowDiagnosticKind::HandlerResolution,
		] {
			let mut diagnostic = clean.clone();
			diagnostic.diagnostics.push(ShadowDiagnostic {
				kind,
				path: Some("events/example.txt".to_string()),
				message: "blocking evidence".to_string(),
			});
			assert!(validate_structured_run_evidence(&diagnostic, "case").is_err());
		}
	}

	#[test]
	fn persisted_structured_attestation_is_portable_and_equally_strict() {
		let mut run = clean_structured_run();
		run.diagnostics.push(ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Warning,
			path: Some("/Users/private/source.txt".to_string()),
			message: "warning from /Users/private/source.txt".to_string(),
		});
		let attestation = validate_structured_run_evidence(&run, "case").unwrap();
		let persisted = ReviewPackCaseRun {
			case_id: "case".to_string(),
			snapshot_id: "b".repeat(64),
			kernel: ReviewKernel::Structured,
			status: run.status,
			output_valid: run.output_valid,
			output_cas_hash: Some("c".repeat(64)),
			legacy_measurement_id: None,
			structured_attestation: Some(attestation),
			diagnostic_kinds: vec![ShadowDiagnosticKind::Warning],
			elapsed_ms: run.elapsed_ms,
		};
		validate_structured_case_run(&persisted).unwrap();
		let encoded = serde_json::to_string(&persisted).unwrap();
		assert!(!encoded.contains("/Users/private"));
		assert!(!encoded.contains("output_dir"));

		let mut handler = persisted;
		handler
			.structured_attestation
			.as_mut()
			.unwrap()
			.handler_resolution_count = 1;
		assert!(validate_structured_case_run(&handler).is_err());
	}

	#[test]
	fn precomputed_structured_hashes_require_pack_local_objects() {
		let temp = tempfile::tempdir().unwrap();
		let output = temp.path().join("pack");
		let selection = ReviewPackSelection::from_path(Path::new(FIXTURE)).unwrap();
		let input = PrecomputedReviewPack {
			selection,
			base_snapshot_identity: "sha256:base".to_string(),
			executable_blake3: "d".repeat(64),
			wiki_knowledge_snapshot_id: None,
			case_runs: vec![ReviewPackCaseRun {
				case_id: "case".to_string(),
				snapshot_id: "e".repeat(64),
				kernel: ReviewKernel::Structured,
				status: "ready".to_string(),
				output_valid: true,
				output_cas_hash: Some("f".repeat(64)),
				legacy_measurement_id: None,
				structured_attestation: Some(ReviewPackStructuredAttestation {
					comparison_id: "a".repeat(64),
					exit_code: 0,
					manual_conflict_count: 0,
					handler_resolution_count: 0,
				}),
				diagnostic_kinds: Vec::new(),
				elapsed_ms: 1,
			}],
			units: Vec::new(),
		};

		let error = build_from_precomputed_evidence(&output, input).unwrap_err();
		assert!(error.to_string().contains("pack-local CAS objects"));
		assert!(!output.exists());
	}

	#[test]
	fn deterministic_scoring_unit_identity_includes_kernel() {
		let snapshot = "a".repeat(64);
		let legacy = scoring_unit_id(&snapshot, "events/example.txt", ReviewKernel::Legacy);
		let structured = scoring_unit_id(&snapshot, "events/example.txt", ReviewKernel::Structured);
		assert_eq!(
			legacy,
			scoring_unit_id(&snapshot, "events/example.txt", ReviewKernel::Legacy)
		);
		assert_ne!(legacy, structured);
	}

	#[test]
	fn structured_review_uses_normalized_module_relation() {
		let temp = tempfile::tempdir().unwrap();
		let game_root = temp.path().join("game");
		let compatch = temp.path().join("compatch");
		let output = temp.path().join("output");
		let relative_path = "common/religions/zzz_foch_religions.txt";
		for (root, content) in [
			(
				&compatch,
				"faith = {\n\
				\tif = { limit = { NOT = { has_country_flag = selected } } add_prestige = 1 }\n\
				\telse = { add_stability = 1 }\n\
				}\n",
			),
			(
				&output,
				"faith = {\n\
				\tif = { limit = { has_country_flag = selected } add_stability = 1 }\n\
				\telse = { add_prestige = 1 }\n\
				}\n",
			),
		] {
			let path = root.join(relative_path);
			fs::create_dir_all(path.parent().unwrap()).unwrap();
			fs::write(path, content).unwrap();
		}
		fs::create_dir_all(&game_root).unwrap();
		let loaded = LoadedSnapshot {
			snapshot: SnapshotRecord::new(
				"case".to_string(),
				GameIdentity {
					app_id: 236_850,
					version: "v1".to_string(),
					steam_build_id: Some(1),
				},
				SnapshotObjectRef {
					workshop_id: "compatch".to_string(),
					content_hash: "a".repeat(64),
				},
				Vec::new(),
			),
			compatch,
			source_dirs: Vec::new(),
		};

		let (relation, diagnostics) = evaluated_ast_relation(
			false,
			ReviewKernel::Structured,
			relative_path,
			&loaded,
			&game_root,
			Some(&output),
		);

		assert_eq!(relation, AstRelation::LogicalEquivalent);
		assert!(diagnostics.is_empty());
		assert_eq!(
			evaluated_ast_relation(
				false,
				ReviewKernel::Structured,
				relative_path,
				&loaded,
				&game_root,
				None,
			)
			.0,
			AstRelation::Nonidentical
		);
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn pinned_registered_legacy_measurement_and_output_are_loaded_without_historical_scores() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		fs::create_dir_all(&paths.root).unwrap();
		let source = temp.path().join("legacy-output");
		fs::create_dir_all(source.join("events")).unwrap();
		fs::write(source.join("events/example.txt"), "example = {}\n").unwrap();
		let store = ObjectStore::new(&paths.objects, &paths.work);
		let object = store.snapshot_tree(&source).unwrap();
		let snapshot_id = "a".repeat(64);
		let crate::dataset::MeasurementCohortKey::OrchestratorBoundV1 {
			executable_hash,
			scorer_version,
			config_hash,
		} = review_pack_legacy_cohort_key()
		else {
			panic!("review-pack Legacy cohort must use the V1 identity")
		};
		let measurement = MeasurementRecord::new_v1(
			crate::dataset::LegacyMeasurementIdentityV1 {
				snapshot_id: snapshot_id.clone(),
				executable_hash,
				scorer_version,
				config_hash,
			},
			"2026-07-01T00:00:00Z".to_string(),
			"2026-07-01T00:00:01Z".to_string(),
			TerminalStatus::Completed,
			None,
			Some(object.hash.clone()),
			None,
		);
		write_jsonl(&paths.measurements, std::slice::from_ref(&measurement)).unwrap();
		let mut selection = ReviewPackSelection {
			schema: REVIEW_PACK_SCHEMA.to_string(),
			profile: REVIEW_PACK_PROFILE.to_string(),
			game_version: "test".to_string(),
			steam_build_id: 1,
			legacy_baseline_blake3: "d".repeat(64),
			expected_verdicts_blake3: "e".repeat(64),
			legacy_unit_count: 1,
			structured_unit_count: 1,
			cases: vec![ReviewPackSelectionCase {
				case_id: "case".to_string(),
				snapshot_id,
				legacy_measurement_id: measurement.measurement_id().to_string(),
				legacy_output_hash: object.hash.clone(),
				legacy_units: vec!["events/example.txt".to_string()],
				structured_units: vec!["events/example.txt".to_string()],
			}],
		};

		let loaded = load_pinned_legacy_cases(&selection, &paths, &store).unwrap();
		assert_eq!(loaded["case"].measurement_id, measurement.measurement_id());
		assert_eq!(loaded["case"].output_cas_hash, object.hash);

		let registry = crate::report::committed_measurement_cohort_registry().unwrap();
		let wrong_identity = registry
			.cohorts
			.iter()
			.find(|cohort| cohort.identity.scorer_version() == "1.0.0")
			.unwrap()
			.identity
			.clone();
		let MeasurementCohortKey::OrchestratorBoundV1 {
			executable_hash,
			scorer_version,
			config_hash,
		} = wrong_identity
		else {
			panic!("historical scorer 1.0 cohort must use the V1 identity")
		};
		let wrong_measurement = MeasurementRecord::new_v1(
			crate::dataset::LegacyMeasurementIdentityV1 {
				snapshot_id: selection.cases[0].snapshot_id.clone(),
				executable_hash,
				scorer_version,
				config_hash,
			},
			"2026-07-01T00:00:00Z".to_string(),
			"2026-07-01T00:00:01Z".to_string(),
			TerminalStatus::Completed,
			None,
			Some(object.hash.clone()),
			None,
		);
		write_jsonl(
			&paths.measurements,
			&[measurement.clone(), wrong_measurement.clone()],
		)
		.unwrap();
		selection.cases[0].legacy_measurement_id = wrong_measurement.measurement_id().to_string();
		assert!(
			load_pinned_legacy_cases(&selection, &paths, &store)
				.unwrap_err()
				.to_string()
				.contains("exact scorer 1.3.0")
		);

		selection.cases[0].legacy_measurement_id = measurement.measurement_id().to_string();
		selection.cases[0].legacy_output_hash = "f".repeat(64);
		assert!(
			load_pinned_legacy_cases(&selection, &paths, &store)
				.unwrap_err()
				.to_string()
				.contains("binding changed")
		);
	}

	#[test]
	fn pinned_snapshot_remains_loadable_after_a_newer_case_snapshot_is_appended() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		fs::create_dir_all(&paths.root).unwrap();
		let game_identity = GameIdentity {
			app_id: 236_850,
			version: "v1".to_string(),
			steam_build_id: Some(1),
		};
		let pinned = SnapshotRecord::new(
			"case".to_string(),
			game_identity.clone(),
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "a".repeat(64),
			},
			vec![SnapshotObjectRef {
				workshop_id: "source".to_string(),
				content_hash: "b".repeat(64),
			}],
		);
		let newer = SnapshotRecord::new(
			"case".to_string(),
			game_identity,
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "c".repeat(64),
			},
			vec![SnapshotObjectRef {
				workshop_id: "source".to_string(),
				content_hash: "d".repeat(64),
			}],
		);
		write_jsonl(&paths.snapshots, &[pinned.clone(), newer]).unwrap();
		let selection = ReviewPackSelection {
			schema: REVIEW_PACK_SCHEMA.to_string(),
			profile: REVIEW_PACK_PROFILE.to_string(),
			game_version: "v1".to_string(),
			steam_build_id: 1,
			legacy_baseline_blake3: "e".repeat(64),
			expected_verdicts_blake3: "f".repeat(64),
			legacy_unit_count: 1,
			structured_unit_count: 1,
			cases: vec![ReviewPackSelectionCase {
				case_id: "case".to_string(),
				snapshot_id: pinned.snapshot_id.clone(),
				legacy_measurement_id: "1".repeat(64),
				legacy_output_hash: "2".repeat(64),
				legacy_units: vec!["events/example.txt".to_string()],
				structured_units: vec!["events/example.txt".to_string()],
			}],
		};
		let game = Eu4GameDiscovery {
			game_root: temp.path().join("game"),
			game_version: "v1".to_string(),
			steam_build_id: Some(1),
			steam_root: None,
		};

		let selected = selected_snapshots(&selection, &paths, &game).unwrap();
		assert_eq!(selected["case"].snapshot_id, pinned.snapshot_id);
	}

	fn assert_forged_snapshot_ref_is_rejected(mutate: impl FnOnce(&mut SnapshotRecord)) {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		fs::create_dir_all(&paths.root).unwrap();
		let original = SnapshotRecord::new(
			"case".to_string(),
			GameIdentity {
				app_id: 236_850,
				version: "v1".to_string(),
				steam_build_id: Some(1),
			},
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "a".repeat(64),
			},
			vec![SnapshotObjectRef {
				workshop_id: "source".to_string(),
				content_hash: "b".repeat(64),
			}],
		);
		let mut forged = original.clone();
		mutate(&mut forged);
		assert_eq!(forged.snapshot_id, original.snapshot_id);
		assert!(!forged.identity_is_valid());
		write_jsonl(&paths.snapshots, &[forged]).unwrap();

		let selection = ReviewPackSelection {
			schema: REVIEW_PACK_SCHEMA.to_string(),
			profile: REVIEW_PACK_PROFILE.to_string(),
			game_version: "v1".to_string(),
			steam_build_id: 1,
			legacy_baseline_blake3: "e".repeat(64),
			expected_verdicts_blake3: "f".repeat(64),
			legacy_unit_count: 1,
			structured_unit_count: 1,
			cases: vec![ReviewPackSelectionCase {
				case_id: "case".to_string(),
				snapshot_id: original.snapshot_id,
				legacy_measurement_id: "1".repeat(64),
				legacy_output_hash: "2".repeat(64),
				legacy_units: vec!["events/example.txt".to_string()],
				structured_units: vec!["events/example.txt".to_string()],
			}],
		};
		let game = Eu4GameDiscovery {
			game_root: temp.path().join("game"),
			game_version: "v1".to_string(),
			steam_build_id: Some(1),
			steam_root: None,
		};

		assert_eq!(
			selected_snapshots(&selection, &paths, &game)
				.unwrap_err()
				.to_string(),
			"pinned snapshot has an invalid identity for case case"
		);
	}

	#[test]
	fn selected_snapshot_rejects_retained_id_with_forged_compatch_ref() {
		assert_forged_snapshot_ref_is_rejected(|snapshot| {
			snapshot.compatch.content_hash = "c".repeat(64);
		});
	}

	#[test]
	fn selected_snapshot_rejects_retained_id_with_forged_source_ref() {
		assert_forged_snapshot_ref_is_rejected(|snapshot| {
			snapshot.source_mods[0].content_hash = "c".repeat(64);
		});
	}
}
