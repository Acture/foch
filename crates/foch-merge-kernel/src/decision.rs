use serde::{Deserialize, Serialize};

use crate::{ClassId, RevisionId, RevisionNode, SourceSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePolicyKind {
	ConservativeStructural,
	OneSidedRemoval,
	DeleteModify,
	AncestorClosure,
	SubtreeSelection,
	ChildSetSelection,
	DivergentNode,
	ScalarReducer,
	Ordering,
	ManualResolution,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeDecisionReason {
	Unchanged,
	OneSidedChange,
	EquivalentChanges,
	ExplicitDomainRule,
	Precedence,
	StructuralConstraint,
	ExplicitResolution,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MergeDecisionResult {
	SelectSource {
		source: RevisionNode,
	},
	Delete {
		deleted_by: Vec<RevisionId>,
		base: RevisionNode,
	},
	SynthesizeScalar {
		value: String,
	},
	CombineChildren,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeDecisionEvidence {
	pub affected_class: ClassId,
	pub policy: MergePolicyKind,
	pub reason: MergeDecisionReason,
	pub contributors: SourceSet,
	pub result: MergeDecisionResult,
}

#[cfg(test)]
mod tests {
	use crate::{NodeId, RevisionId, RevisionNode};

	use super::*;

	#[test]
	fn decision_evidence_serialization_is_deterministic() {
		let evidence = MergeDecisionEvidence {
			affected_class: ClassId::new(7),
			policy: MergePolicyKind::DivergentNode,
			reason: MergeDecisionReason::ExplicitDomainRule,
			contributors: SourceSet::new([
				RevisionNode::new(RevisionId::RIGHT, NodeId::new(4)),
				RevisionNode::new(RevisionId::LEFT, NodeId::new(8)),
			]),
			result: MergeDecisionResult::SelectSource {
				source: RevisionNode::new(RevisionId::RIGHT, NodeId::new(4)),
			},
		};

		assert_eq!(
			serde_json::to_string(&evidence).unwrap(),
			serde_json::to_string(&evidence.clone()).unwrap()
		);
	}
}
