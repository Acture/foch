use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::dataset::{IdentifiedRecord, stable_id};

pub const REVIEW_ANNOTATION_SCHEMA: &str = "1.0.0";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRecordKind {
	Proposal,
	Adjudication,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
	Proposed,
	Accepted,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLabel {
	FochBetter,
	Equivalent,
	HumanBetter,
	BothProblematic,
	InsufficientEvidence,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKernel {
	Legacy,
	Structured,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AstRelation {
	ExactEquivalent,
	Nonidentical,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct ReviewBinding {
	pub review_pack_id: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub wiki_knowledge_snapshot_id: Option<String>,
	pub case_id: String,
	pub relative_path: String,
	pub snapshot_id: String,
	pub scoring_unit_id: String,
	pub kernel: ReviewKernel,
	pub base_content_hash: String,
	/// Source hashes in declared playset order. Their order is part of the binding.
	pub source_content_hashes: Vec<String>,
	pub human_content_hash: String,
	pub candidate_content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct FamilyInvariantEvidence {
	pub invariant_id: String,
	pub supports_label: bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RuntimeEvidence {
	pub evidence_id: String,
	pub supports_label: bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReviewAnnotationDraft {
	pub kind: ReviewRecordKind,
	pub status: ReviewStatus,
	pub label: ReviewLabel,
	pub ast_relation: AstRelation,
	pub binding: ReviewBinding,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supersedes: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reviewer: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub model: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub provenance: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub family_invariants: Vec<FamilyInvariantEvidence>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub runtime_evidence: Option<RuntimeEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReviewAnnotation {
	pub schema: String,
	pub annotation_id: String,
	pub kind: ReviewRecordKind,
	pub status: ReviewStatus,
	pub label: ReviewLabel,
	pub ast_relation: AstRelation,
	pub review_pack_id: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub wiki_knowledge_snapshot_id: Option<String>,
	pub case_id: String,
	pub relative_path: String,
	pub snapshot_id: String,
	pub scoring_unit_id: String,
	pub kernel: ReviewKernel,
	pub base_content_hash: String,
	/// Source hashes in declared playset order. Their order is part of the identity.
	pub source_content_hashes: Vec<String>,
	pub human_content_hash: String,
	pub candidate_content_hash: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supersedes: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reviewer: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub model: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub provenance: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub family_invariants: Vec<FamilyInvariantEvidence>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub runtime_evidence: Option<RuntimeEvidence>,
}

impl ReviewAnnotation {
	pub fn new(draft: ReviewAnnotationDraft) -> Result<Self, AnnotationValidationError> {
		let binding = draft.binding;
		let mut annotation = Self {
			schema: REVIEW_ANNOTATION_SCHEMA.to_string(),
			annotation_id: String::new(),
			kind: draft.kind,
			status: draft.status,
			label: draft.label,
			ast_relation: draft.ast_relation,
			review_pack_id: binding.review_pack_id,
			wiki_knowledge_snapshot_id: binding.wiki_knowledge_snapshot_id,
			case_id: binding.case_id,
			relative_path: binding.relative_path,
			snapshot_id: binding.snapshot_id,
			scoring_unit_id: binding.scoring_unit_id,
			kernel: binding.kernel,
			base_content_hash: binding.base_content_hash,
			source_content_hashes: binding.source_content_hashes,
			human_content_hash: binding.human_content_hash,
			candidate_content_hash: binding.candidate_content_hash,
			supersedes: draft.supersedes,
			reviewer: draft.reviewer,
			model: draft.model,
			provenance: draft.provenance,
			reason: draft.reason,
			family_invariants: draft.family_invariants,
			runtime_evidence: draft.runtime_evidence,
		};
		annotation.annotation_id = annotation.expected_annotation_id();
		annotation.validate()?;
		Ok(annotation)
	}

	pub fn binding(&self) -> ReviewBinding {
		ReviewBinding {
			review_pack_id: self.review_pack_id.clone(),
			wiki_knowledge_snapshot_id: self.wiki_knowledge_snapshot_id.clone(),
			case_id: self.case_id.clone(),
			relative_path: self.relative_path.clone(),
			snapshot_id: self.snapshot_id.clone(),
			scoring_unit_id: self.scoring_unit_id.clone(),
			kernel: self.kernel,
			base_content_hash: self.base_content_hash.clone(),
			source_content_hashes: self.source_content_hashes.clone(),
			human_content_hash: self.human_content_hash.clone(),
			candidate_content_hash: self.candidate_content_hash.clone(),
		}
	}

	pub fn expected_annotation_id(&self) -> String {
		let identity = AnnotationIdentity {
			schema: &self.schema,
			kind: self.kind,
			status: self.status,
			label: self.label,
			ast_relation: self.ast_relation,
			binding: self.binding(),
			supersedes: self.supersedes.as_deref(),
			reviewer: self.reviewer.as_deref(),
			model: self.model.as_deref(),
			provenance: self.provenance.as_deref(),
			reason: self.reason.as_deref(),
			family_invariants: &self.family_invariants,
			runtime_evidence: self.runtime_evidence.as_ref(),
		};
		let payload =
			serde_json::to_vec(&identity).expect("review annotation identity always serializes");
		stable_id("merge-quality-review-annotation", &[&payload])
	}

	pub fn validate(&self) -> Result<(), AnnotationValidationError> {
		if self.schema != REVIEW_ANNOTATION_SCHEMA {
			return Err(AnnotationValidationError::UnsupportedSchema(
				self.schema.clone(),
			));
		}
		validate_binding(&self.binding())?;
		validate_optional("supersedes", self.supersedes.as_deref())?;
		validate_optional("reviewer", self.reviewer.as_deref())?;
		validate_optional("model", self.model.as_deref())?;
		validate_optional("provenance", self.provenance.as_deref())?;
		validate_optional("reason", self.reason.as_deref())?;

		for invariant in &self.family_invariants {
			validate_required("family_invariants[].invariant_id", &invariant.invariant_id)?;
			validate_optional("family_invariants[].detail", invariant.detail.as_deref())?;
		}
		if let Some(evidence) = &self.runtime_evidence {
			validate_required("runtime_evidence.evidence_id", &evidence.evidence_id)?;
			validate_optional("runtime_evidence.detail", evidence.detail.as_deref())?;
		}

		if self.status == ReviewStatus::Accepted && self.kind != ReviewRecordKind::Adjudication {
			return Err(AnnotationValidationError::AcceptedRecordMustBeAdjudication);
		}

		let has_family_support = self
			.family_invariants
			.iter()
			.any(|invariant| invariant.supports_label);
		let has_runtime_support = self
			.runtime_evidence
			.as_ref()
			.is_some_and(|evidence| evidence.supports_label);
		if self.ast_relation == AstRelation::Nonidentical
			&& matches!(
				self.label,
				ReviewLabel::FochBetter | ReviewLabel::Equivalent
			) && !has_family_support
			&& !has_runtime_support
		{
			return Err(AnnotationValidationError::NonidenticalPositiveLabelNeedsEvidence);
		}
		if self.status == ReviewStatus::Accepted
			&& self.ast_relation == AstRelation::Nonidentical
			&& is_gui_path(&self.relative_path)
			&& !has_runtime_support
		{
			return Err(AnnotationValidationError::GuiNonidenticalNeedsRuntimeEvidence);
		}

		if self.annotation_id != self.expected_annotation_id() {
			return Err(AnnotationValidationError::AnnotationIdMismatch {
				expected: self.expected_annotation_id(),
				actual: self.annotation_id.clone(),
			});
		}
		if self.supersedes.as_deref() == Some(self.annotation_id.as_str()) {
			return Err(AnnotationValidationError::SelfSupersession);
		}
		Ok(())
	}
}

impl IdentifiedRecord for ReviewAnnotation {
	fn record_id(&self) -> &str {
		&self.annotation_id
	}
}

#[derive(Serialize)]
struct AnnotationIdentity<'a> {
	schema: &'a str,
	kind: ReviewRecordKind,
	status: ReviewStatus,
	label: ReviewLabel,
	ast_relation: AstRelation,
	binding: ReviewBinding,
	supersedes: Option<&'a str>,
	reviewer: Option<&'a str>,
	model: Option<&'a str>,
	provenance: Option<&'a str>,
	reason: Option<&'a str>,
	family_invariants: &'a [FamilyInvariantEvidence],
	runtime_evidence: Option<&'a RuntimeEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationValidationError {
	UnsupportedSchema(String),
	EmptyField(&'static str),
	InvalidRelativePath(String),
	MissingSourceContentHashes,
	InvalidContentHash { field: &'static str, value: String },
	AcceptedRecordMustBeAdjudication,
	NonidenticalPositiveLabelNeedsEvidence,
	GuiNonidenticalNeedsRuntimeEvidence,
	AnnotationIdMismatch { expected: String, actual: String },
	SelfSupersession,
}

impl fmt::Display for AnnotationValidationError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnsupportedSchema(schema) => {
				write!(formatter, "unsupported review annotation schema {schema}")
			}
			Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
			Self::InvalidRelativePath(path) => {
				write!(formatter, "relative_path is not normalized and relative: {path}")
			}
			Self::MissingSourceContentHashes => {
				formatter.write_str("source_content_hashes must not be empty")
			}
			Self::InvalidContentHash { field, value } => {
				write!(
					formatter,
					"{field} is not a lowercase 64-character hex hash: {value}"
				)
			}
			Self::AcceptedRecordMustBeAdjudication => formatter.write_str(
				"accepted records must be adjudications; proposals cannot be accepted",
			),
			Self::NonidenticalPositiveLabelNeedsEvidence => formatter.write_str(
				"nonidentical foch_better/equivalent records require a supporting family invariant or runtime evidence",
			),
			Self::GuiNonidenticalNeedsRuntimeEvidence => formatter.write_str(
				"accepted nonidentical GUI records require supporting runtime evidence",
			),
			Self::AnnotationIdMismatch { expected, actual } => {
				write!(
					formatter,
					"annotation_id mismatch: expected {expected}, found {actual}"
				)
			}
			Self::SelfSupersession => formatter.write_str("an annotation cannot supersede itself"),
		}
	}
}

impl Error for AnnotationValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationResolutionError {
	DuplicateAnnotationId(String),
	InvalidRecord {
		annotation_id: String,
		source: AnnotationValidationError,
	},
	InvalidCurrentBinding(AnnotationValidationError),
	MissingSupersedes {
		annotation_id: String,
		missing_id: String,
	},
	SupersessionFork {
		superseded_id: String,
		first_id: String,
		second_id: String,
	},
	SupersessionCycle(Vec<String>),
	StaleSupersessionBinding {
		newer_id: String,
		older_id: String,
		field: &'static str,
	},
	DuplicateCurrentBinding {
		review_pack_id: String,
		scoring_unit_id: String,
		kernel: ReviewKernel,
	},
	MissingCurrentBinding {
		annotation_id: String,
	},
	StaleCurrentBinding {
		annotation_id: String,
		field: &'static str,
	},
	AmbiguousEffectiveBinding {
		first_id: String,
		second_id: String,
	},
}

impl fmt::Display for AnnotationResolutionError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::DuplicateAnnotationId(id) => write!(formatter, "duplicate annotation_id {id}"),
			Self::InvalidRecord {
				annotation_id,
				source,
			} => write!(formatter, "invalid annotation {annotation_id}: {source}"),
			Self::InvalidCurrentBinding(source) => {
				write!(formatter, "invalid current review binding: {source}")
			}
			Self::MissingSupersedes {
				annotation_id,
				missing_id,
			} => write!(
				formatter,
				"annotation {annotation_id} supersedes missing annotation {missing_id}"
			),
			Self::SupersessionFork {
				superseded_id,
				first_id,
				second_id,
			} => write!(
				formatter,
				"annotation {superseded_id} is superseded by both {first_id} and {second_id}"
			),
			Self::SupersessionCycle(ids) => {
				write!(formatter, "supersession cycle: {}", ids.join(" -> "))
			}
			Self::StaleSupersessionBinding {
				newer_id,
				older_id,
				field,
			} => write!(
				formatter,
				"annotation {newer_id} cannot supersede {older_id}: {field} changed"
			),
			Self::DuplicateCurrentBinding {
				review_pack_id,
				scoring_unit_id,
				kernel,
			} => write!(
				formatter,
				"duplicate current binding for {review_pack_id}/{scoring_unit_id}/{kernel:?}"
			),
			Self::MissingCurrentBinding { annotation_id } => write!(
				formatter,
				"annotation {annotation_id} has no current review-pack binding"
			),
			Self::StaleCurrentBinding {
				annotation_id,
				field,
			} => write!(
				formatter,
				"annotation {annotation_id} is stale: current {field} differs"
			),
			Self::AmbiguousEffectiveBinding {
				first_id,
				second_id,
			} => write!(
				formatter,
				"effective annotations {first_id} and {second_id} bind the same scoring unit"
			),
		}
	}
}

impl Error for AnnotationResolutionError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		match self {
			Self::InvalidRecord { source, .. } | Self::InvalidCurrentBinding(source) => {
				Some(source)
			}
			_ => None,
		}
	}
}

type BindingKey = (String, String, ReviewKernel);

/// Resolves append-only records against the exact bindings in the current review pack.
pub fn resolve_effective_records(
	records: &[ReviewAnnotation],
	current_bindings: &[ReviewBinding],
) -> Result<Vec<ReviewAnnotation>, AnnotationResolutionError> {
	let mut by_id = BTreeMap::new();
	for record in records {
		if by_id.insert(record.annotation_id.clone(), record).is_some() {
			return Err(AnnotationResolutionError::DuplicateAnnotationId(
				record.annotation_id.clone(),
			));
		}
	}

	let mut superseded_by: BTreeMap<String, String> = BTreeMap::new();
	for record in records {
		let Some(older_id) = &record.supersedes else {
			continue;
		};
		if !by_id.contains_key(older_id) {
			return Err(AnnotationResolutionError::MissingSupersedes {
				annotation_id: record.annotation_id.clone(),
				missing_id: older_id.clone(),
			});
		}
		if let Some(first_id) = superseded_by.insert(older_id.clone(), record.annotation_id.clone())
		{
			return Err(AnnotationResolutionError::SupersessionFork {
				superseded_id: older_id.clone(),
				first_id,
				second_id: record.annotation_id.clone(),
			});
		}
	}
	reject_supersession_cycles(&by_id)?;

	for record in records {
		record
			.validate()
			.map_err(|source| AnnotationResolutionError::InvalidRecord {
				annotation_id: record.annotation_id.clone(),
				source,
			})?;
		if let Some(older_id) = &record.supersedes {
			let older = by_id
				.get(older_id)
				.expect("missing supersedes was checked above");
			if let Some(field) = binding_mismatch_field(&record.binding(), &older.binding()) {
				return Err(AnnotationResolutionError::StaleSupersessionBinding {
					newer_id: record.annotation_id.clone(),
					older_id: older_id.clone(),
					field,
				});
			}
		}
	}

	let mut current_by_key = BTreeMap::new();
	for binding in current_bindings {
		validate_binding(binding).map_err(AnnotationResolutionError::InvalidCurrentBinding)?;
		let key = binding_key(binding);
		if current_by_key.insert(key.clone(), binding).is_some() {
			return Err(AnnotationResolutionError::DuplicateCurrentBinding {
				review_pack_id: key.0,
				scoring_unit_id: key.1,
				kernel: key.2,
			});
		}
	}
	let current_pack_ids = current_bindings
		.iter()
		.map(|binding| binding.review_pack_id.as_str())
		.collect::<std::collections::BTreeSet<_>>();
	for record in records {
		let binding = record.binding();
		let Some(current) = current_by_key.get(&binding_key(&binding)) else {
			if !current_pack_ids.contains(record.review_pack_id.as_str()) {
				continue;
			}
			return Err(AnnotationResolutionError::MissingCurrentBinding {
				annotation_id: record.annotation_id.clone(),
			});
		};
		if let Some(field) = binding_mismatch_field(&binding, current) {
			return Err(AnnotationResolutionError::StaleCurrentBinding {
				annotation_id: record.annotation_id.clone(),
				field,
			});
		}
	}

	let mut effective = records
		.iter()
		.filter(|record| current_pack_ids.contains(record.review_pack_id.as_str()))
		.filter(|record| !superseded_by.contains_key(&record.annotation_id))
		.cloned()
		.collect::<Vec<_>>();
	effective.sort_by_key(|record| (record.binding(), record.annotation_id.clone()));

	let mut effective_by_binding = BTreeMap::new();
	for record in &effective {
		if let Some(first_id) =
			effective_by_binding.insert(record.binding(), record.annotation_id.clone())
		{
			return Err(AnnotationResolutionError::AmbiguousEffectiveBinding {
				first_id,
				second_id: record.annotation_id.clone(),
			});
		}
	}
	Ok(effective)
}

fn reject_supersession_cycles(
	by_id: &BTreeMap<String, &ReviewAnnotation>,
) -> Result<(), AnnotationResolutionError> {
	for start in by_id.keys() {
		let mut positions = BTreeMap::new();
		let mut path = Vec::new();
		let mut current = Some(start.as_str());
		while let Some(id) = current {
			if let Some(position) = positions.insert(id.to_string(), path.len()) {
				let mut cycle = path[position..].to_vec();
				cycle.push(id.to_string());
				return Err(AnnotationResolutionError::SupersessionCycle(cycle));
			}
			path.push(id.to_string());
			current = by_id
				.get(id)
				.and_then(|record| record.supersedes.as_deref());
		}
	}
	Ok(())
}

fn binding_key(binding: &ReviewBinding) -> BindingKey {
	(
		binding.review_pack_id.clone(),
		binding.scoring_unit_id.clone(),
		binding.kernel,
	)
}

fn binding_mismatch_field(
	actual: &ReviewBinding,
	expected: &ReviewBinding,
) -> Option<&'static str> {
	if actual.review_pack_id != expected.review_pack_id {
		Some("review_pack_id")
	} else if actual.wiki_knowledge_snapshot_id != expected.wiki_knowledge_snapshot_id {
		Some("wiki_knowledge_snapshot_id")
	} else if actual.case_id != expected.case_id {
		Some("case_id")
	} else if actual.relative_path != expected.relative_path {
		Some("relative_path")
	} else if actual.snapshot_id != expected.snapshot_id {
		Some("snapshot_id")
	} else if actual.scoring_unit_id != expected.scoring_unit_id {
		Some("scoring_unit_id")
	} else if actual.kernel != expected.kernel {
		Some("kernel")
	} else if actual.base_content_hash != expected.base_content_hash {
		Some("base_content_hash")
	} else if actual.source_content_hashes != expected.source_content_hashes {
		Some("source_content_hashes")
	} else if actual.human_content_hash != expected.human_content_hash {
		Some("human_content_hash")
	} else if actual.candidate_content_hash != expected.candidate_content_hash {
		Some("candidate_content_hash")
	} else {
		None
	}
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct RolloutSelection {
	pub review_pack_id: String,
	pub scoring_unit_id: String,
	pub kernel: ReviewKernel,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct LabelCounts {
	pub total: usize,
	pub labels: BTreeMap<ReviewLabel, usize>,
}

impl LabelCounts {
	fn add(&mut self, label: ReviewLabel) {
		self.total += 1;
		*self.labels.entry(label).or_default() += 1;
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct EvidenceSummary {
	pub strict: LabelCounts,
	pub adjudicated: LabelCounts,
	pub provisional: LabelCounts,
}

impl EvidenceSummary {
	fn add(&mut self, record: &ReviewAnnotation) {
		if record.status == ReviewStatus::Proposed {
			self.provisional.add(record.label);
		} else if record.ast_relation == AstRelation::ExactEquivalent {
			self.strict.add(record.label);
		} else {
			self.adjudicated.add(record.label);
		}
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct ReviewAnnotationSummary {
	pub by_kernel: BTreeMap<ReviewKernel, EvidenceSummary>,
	pub rollout_selection: EvidenceSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationSummaryError {
	EmptySelectionField(&'static str),
	ConflictingRolloutSelection {
		review_pack_id: String,
		scoring_unit_id: String,
		first: ReviewKernel,
		second: ReviewKernel,
	},
}

impl fmt::Display for AnnotationSummaryError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::EmptySelectionField(field) => write!(formatter, "{field} must not be empty"),
			Self::ConflictingRolloutSelection {
				review_pack_id,
				scoring_unit_id,
				first,
				second,
			} => write!(
				formatter,
				"rollout selects both {first:?} and {second:?} for {review_pack_id}/{scoring_unit_id}"
			),
		}
	}
}

impl Error for AnnotationSummaryError {}

pub fn summarize_annotations(
	effective_records: &[ReviewAnnotation],
	rollout_selections: &[RolloutSelection],
) -> Result<ReviewAnnotationSummary, AnnotationSummaryError> {
	let mut selected = BTreeMap::new();
	for selection in rollout_selections {
		if selection.review_pack_id.trim().is_empty() {
			return Err(AnnotationSummaryError::EmptySelectionField(
				"review_pack_id",
			));
		}
		if selection.scoring_unit_id.trim().is_empty() {
			return Err(AnnotationSummaryError::EmptySelectionField(
				"scoring_unit_id",
			));
		}
		let key = (
			selection.review_pack_id.clone(),
			selection.scoring_unit_id.clone(),
		);
		if let Some(first) = selected.insert(key.clone(), selection.kernel)
			&& first != selection.kernel
		{
			return Err(AnnotationSummaryError::ConflictingRolloutSelection {
				review_pack_id: key.0,
				scoring_unit_id: key.1,
				first,
				second: selection.kernel,
			});
		}
	}

	let mut summary = ReviewAnnotationSummary::default();
	summary.by_kernel.entry(ReviewKernel::Legacy).or_default();
	summary
		.by_kernel
		.entry(ReviewKernel::Structured)
		.or_default();
	for record in effective_records {
		summary
			.by_kernel
			.entry(record.kernel)
			.or_default()
			.add(record);
		if selected.get(&(
			record.review_pack_id.clone(),
			record.scoring_unit_id.clone(),
		)) == Some(&record.kernel)
		{
			summary.rollout_selection.add(record);
		}
	}
	Ok(summary)
}

fn validate_binding(binding: &ReviewBinding) -> Result<(), AnnotationValidationError> {
	validate_required("review_pack_id", &binding.review_pack_id)?;
	validate_optional(
		"wiki_knowledge_snapshot_id",
		binding.wiki_knowledge_snapshot_id.as_deref(),
	)?;
	validate_required("case_id", &binding.case_id)?;
	validate_required("relative_path", &binding.relative_path)?;
	validate_relative_path(&binding.relative_path)?;
	validate_required("snapshot_id", &binding.snapshot_id)?;
	validate_required("scoring_unit_id", &binding.scoring_unit_id)?;
	validate_hash("base_content_hash", &binding.base_content_hash)?;
	if binding.source_content_hashes.is_empty() {
		return Err(AnnotationValidationError::MissingSourceContentHashes);
	}
	for hash in &binding.source_content_hashes {
		validate_hash("source_content_hashes[]", hash)?;
	}
	validate_hash("human_content_hash", &binding.human_content_hash)?;
	validate_hash("candidate_content_hash", &binding.candidate_content_hash)
}

fn validate_required(field: &'static str, value: &str) -> Result<(), AnnotationValidationError> {
	if value.trim().is_empty() {
		Err(AnnotationValidationError::EmptyField(field))
	} else {
		Ok(())
	}
}

fn validate_optional(
	field: &'static str,
	value: Option<&str>,
) -> Result<(), AnnotationValidationError> {
	if value.is_some_and(|value| value.trim().is_empty()) {
		Err(AnnotationValidationError::EmptyField(field))
	} else {
		Ok(())
	}
}

fn validate_relative_path(path: &str) -> Result<(), AnnotationValidationError> {
	let parsed = Path::new(path);
	let normalized = !path.contains('\\')
		&& !path.contains("//")
		&& !path.ends_with('/')
		&& !parsed.is_absolute()
		&& parsed
			.components()
			.all(|component| matches!(component, Component::Normal(_)));
	if normalized {
		Ok(())
	} else {
		Err(AnnotationValidationError::InvalidRelativePath(
			path.to_string(),
		))
	}
}

fn validate_hash(field: &'static str, value: &str) -> Result<(), AnnotationValidationError> {
	let valid = value.len() == 64
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
	if valid {
		Ok(())
	} else {
		Err(AnnotationValidationError::InvalidContentHash {
			field,
			value: value.to_string(),
		})
	}
}

fn is_gui_path(relative_path: &str) -> bool {
	relative_path.to_ascii_lowercase().ends_with(".gui")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn hash(byte: char) -> String {
		byte.to_string().repeat(64)
	}

	fn binding(unit: &str, kernel: ReviewKernel) -> ReviewBinding {
		ReviewBinding {
			review_pack_id: "review-pack".to_string(),
			wiki_knowledge_snapshot_id: Some("wiki-snapshot".to_string()),
			case_id: format!("case-{unit}"),
			relative_path: format!("common/scripted_triggers/{unit}.txt"),
			snapshot_id: format!("snapshot-{unit}"),
			scoring_unit_id: unit.to_string(),
			kernel,
			base_content_hash: hash('a'),
			source_content_hashes: vec![hash('b'), hash('c')],
			human_content_hash: hash('d'),
			candidate_content_hash: hash('e'),
		}
	}

	fn draft(unit: &str, kernel: ReviewKernel) -> ReviewAnnotationDraft {
		ReviewAnnotationDraft {
			kind: ReviewRecordKind::Proposal,
			status: ReviewStatus::Proposed,
			label: ReviewLabel::InsufficientEvidence,
			ast_relation: AstRelation::Nonidentical,
			binding: binding(unit, kernel),
			supersedes: None,
			reviewer: None,
			model: None,
			provenance: None,
			reason: None,
			family_invariants: Vec::new(),
			runtime_evidence: None,
		}
	}

	#[test]
	fn stale_candidate_hash_is_rejected() {
		let record = ReviewAnnotation::new(draft("unit", ReviewKernel::Structured)).unwrap();
		let mut current = record.binding();
		current.candidate_content_hash = hash('f');

		assert!(matches!(
			resolve_effective_records(&[record], &[current]),
			Err(AnnotationResolutionError::StaleCurrentBinding {
				field: "candidate_content_hash",
				..
			})
		));
	}

	#[test]
	fn historical_pack_records_are_retained_but_not_resolved_as_current() {
		let mut historical_draft = draft("unit", ReviewKernel::Structured);
		historical_draft.binding.review_pack_id = "historical-pack".to_string();
		let historical = ReviewAnnotation::new(historical_draft).unwrap();
		let current = binding("unit", ReviewKernel::Structured);

		assert!(
			resolve_effective_records(&[historical], &[current])
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn supersession_chain_resolves_to_latest_record() {
		let first = ReviewAnnotation::new(draft("unit", ReviewKernel::Structured)).unwrap();
		let mut second_draft = draft("unit", ReviewKernel::Structured);
		second_draft.supersedes = Some(first.annotation_id.clone());
		second_draft.kind = ReviewRecordKind::Adjudication;
		second_draft.status = ReviewStatus::Accepted;
		second_draft.label = ReviewLabel::Equivalent;
		second_draft.ast_relation = AstRelation::ExactEquivalent;
		let second = ReviewAnnotation::new(second_draft).unwrap();
		let mut third_draft = draft("unit", ReviewKernel::Structured);
		third_draft.supersedes = Some(second.annotation_id.clone());
		third_draft.kind = ReviewRecordKind::Adjudication;
		third_draft.status = ReviewStatus::Accepted;
		third_draft.label = ReviewLabel::Equivalent;
		third_draft.ast_relation = AstRelation::ExactEquivalent;
		third_draft.reason = Some("final review".to_string());
		let third = ReviewAnnotation::new(third_draft).unwrap();

		let effective = resolve_effective_records(
			&[third.clone(), first, second],
			std::slice::from_ref(&third.binding()),
		)
		.unwrap();

		assert_eq!(effective, vec![third]);
	}

	#[test]
	fn supersession_forks_are_rejected() {
		let first = ReviewAnnotation::new(draft("unit", ReviewKernel::Structured)).unwrap();
		let mut second_draft = draft("unit", ReviewKernel::Structured);
		second_draft.supersedes = Some(first.annotation_id.clone());
		second_draft.reason = Some("review A".to_string());
		let second = ReviewAnnotation::new(second_draft).unwrap();
		let mut third_draft = draft("unit", ReviewKernel::Structured);
		third_draft.supersedes = Some(first.annotation_id.clone());
		third_draft.reason = Some("review B".to_string());
		let third = ReviewAnnotation::new(third_draft).unwrap();

		assert!(matches!(
			resolve_effective_records(
				&[first.clone(), second, third],
				std::slice::from_ref(&first.binding())
			),
			Err(AnnotationResolutionError::SupersessionFork { .. })
		));
	}

	#[test]
	fn supersession_cycles_are_rejected_before_record_resolution() {
		let mut first = ReviewAnnotation::new(draft("first", ReviewKernel::Structured)).unwrap();
		let mut second = ReviewAnnotation::new(draft("second", ReviewKernel::Structured)).unwrap();
		first.annotation_id = "cycle-a".to_string();
		first.supersedes = Some("cycle-b".to_string());
		second.annotation_id = "cycle-b".to_string();
		second.supersedes = Some("cycle-a".to_string());

		assert!(matches!(
			resolve_effective_records(&[first, second], &[]),
			Err(AnnotationResolutionError::SupersessionCycle(_))
		));
	}

	#[test]
	fn nonidentical_gui_acceptance_requires_runtime_evidence() {
		let mut gui = draft("gui", ReviewKernel::Structured);
		gui.binding.relative_path = "interface/example.gui".to_string();
		gui.kind = ReviewRecordKind::Adjudication;
		gui.status = ReviewStatus::Accepted;
		gui.label = ReviewLabel::Equivalent;
		gui.family_invariants.push(FamilyInvariantEvidence {
			invariant_id: "gui-order-preserved".to_string(),
			supports_label: true,
			detail: None,
		});

		assert_eq!(
			ReviewAnnotation::new(gui).unwrap_err(),
			AnnotationValidationError::GuiNonidenticalNeedsRuntimeEvidence
		);
	}

	#[test]
	fn nonidentical_positive_label_requires_supporting_evidence() {
		let mut unsupported = draft("unit", ReviewKernel::Structured);
		unsupported.label = ReviewLabel::FochBetter;
		assert_eq!(
			ReviewAnnotation::new(unsupported).unwrap_err(),
			AnnotationValidationError::NonidenticalPositiveLabelNeedsEvidence
		);

		let mut exact = draft("unit", ReviewKernel::Structured);
		exact.kind = ReviewRecordKind::Adjudication;
		exact.status = ReviewStatus::Accepted;
		exact.label = ReviewLabel::Equivalent;
		exact.ast_relation = AstRelation::ExactEquivalent;
		assert!(ReviewAnnotation::new(exact).is_ok());
	}

	#[test]
	fn summary_separates_strict_adjudicated_and_provisional_evidence() {
		let mut strict = draft("strict", ReviewKernel::Legacy);
		strict.kind = ReviewRecordKind::Adjudication;
		strict.status = ReviewStatus::Accepted;
		strict.label = ReviewLabel::Equivalent;
		strict.ast_relation = AstRelation::ExactEquivalent;
		let strict = ReviewAnnotation::new(strict).unwrap();

		let mut adjudicated = draft("adjudicated", ReviewKernel::Legacy);
		adjudicated.kind = ReviewRecordKind::Adjudication;
		adjudicated.status = ReviewStatus::Accepted;
		adjudicated.label = ReviewLabel::HumanBetter;
		let adjudicated = ReviewAnnotation::new(adjudicated).unwrap();

		let provisional =
			ReviewAnnotation::new(draft("provisional", ReviewKernel::Structured)).unwrap();
		let selection = RolloutSelection {
			review_pack_id: provisional.review_pack_id.clone(),
			scoring_unit_id: provisional.scoring_unit_id.clone(),
			kernel: ReviewKernel::Structured,
		};

		let summary =
			summarize_annotations(&[strict, adjudicated, provisional], &[selection]).unwrap();

		assert_eq!(summary.by_kernel[&ReviewKernel::Legacy].strict.total, 1);
		assert_eq!(
			summary.by_kernel[&ReviewKernel::Legacy].adjudicated.total,
			1
		);
		assert_eq!(
			summary.by_kernel[&ReviewKernel::Structured]
				.provisional
				.total,
			1
		);
		assert_eq!(summary.rollout_selection.provisional.total, 1);
		assert_eq!(summary.rollout_selection.strict.total, 0);
		assert_eq!(summary.rollout_selection.adjudicated.total, 0);
	}
}
