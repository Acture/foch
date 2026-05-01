use super::binding::{DefinitionRecord, RuntimeState};
use foch_core::model::{Finding, FindingChannel, Severity, SymbolKind};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OverlapStatus {
	None,
	DiscardableBaseCopy,
	MergeCandidate,
	OvershadowConflict,
}

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct SymbolKey {
	kind: SymbolKind,
	name: String,
}

pub(crate) fn build_overlap_findings(state: &RuntimeState) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut grouped = HashMap::<(SymbolKind, String), Vec<&DefinitionRecord>>::new();
	for definition in &state.definitions {
		grouped
			.entry((definition.kind, definition.name.clone()))
			.or_default()
			.push(definition);
	}

	for ((kind, name), defs) in grouped {
		let statuses = defs
			.iter()
			.filter_map(|definition| {
				state
					.overlap_status_by_def
					.get(&definition.index)
					.copied()
					.map(|status| (definition, status))
			})
			.filter(|(_, status)| *status != OverlapStatus::None)
			.collect::<Vec<_>>();
		if statuses.is_empty() {
			continue;
		}

		// Runtime overlap findings only describe cross-mod overlaps. When all
		// participating definitions come from a single mod (e.g. the same key
		// appears under both `decisions/` and `events/decisions/` inside one
		// mod) the finding is misleading and pure noise. Such intra-mod
		// duplication is the responsibility of other rules such as
		// `file-overwrite-conflict` or `duplicate-scripted-effect`.
		let distinct_mods = defs
			.iter()
			.map(|definition| definition.mod_id.as_str())
			.collect::<HashSet<_>>();
		if distinct_mods.len() < 2 {
			continue;
		}

		let evidence = defs
			.iter()
			.map(|definition| {
				format!(
					"{}:{}:{}:{}",
					definition.mod_id, definition.path, definition.line, definition.column
				)
			})
			.collect::<Vec<_>>()
			.join("; ");
		let Some((focus, status)) = statuses.last().copied() else {
			continue;
		};
		match status {
			OverlapStatus::DiscardableBaseCopy => findings.push(Finding {
				rule_id: "mergeable-overlap".to_string(),
				severity: Severity::Info,
				channel: FindingChannel::Advisory,
				message: format!(
					"base game-equivalent override can be cleaned up: {} {}",
					symbol_kind_text(kind),
					name
				),
				mod_id: Some(focus.mod_id.clone()),
				path: Some(focus.path.clone().into()),
				evidence: Some(evidence),
				line: Some(focus.line),
				column: Some(focus.column),
				confidence: Some(0.9),
			}),
			OverlapStatus::MergeCandidate => findings.push(Finding {
				rule_id: "mergeable-overlap".to_string(),
				severity: Severity::Info,
				channel: FindingChannel::Advisory,
				message: format!(
					"cross-mod overlapping definition can be auto-merged: {} {}",
					symbol_kind_text(kind),
					name
				),
				mod_id: Some(focus.mod_id.clone()),
				path: Some(focus.path.clone().into()),
				evidence: Some(evidence),
				line: Some(focus.line),
				column: Some(focus.column),
				confidence: Some(0.8),
			}),
			OverlapStatus::OvershadowConflict => findings.push(Finding {
				rule_id: "cross-mod-overshadow".to_string(),
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: format!(
					"cross-mod overlapping definition changes resolution target: {} {}",
					symbol_kind_text(kind),
					name
				),
				mod_id: Some(focus.mod_id.clone()),
				path: Some(focus.path.clone().into()),
				evidence: Some(evidence),
				line: Some(focus.line),
				column: Some(focus.column),
				confidence: Some(1.0),
			}),
			OverlapStatus::None => {}
		}
	}

	findings
}

pub(crate) fn classify_definition_overlaps(
	definitions: &[DefinitionRecord],
	base_mod_id: Option<&str>,
) -> HashMap<usize, OverlapStatus> {
	let mut grouped = HashMap::<SymbolKey, Vec<&DefinitionRecord>>::new();
	for definition in definitions {
		grouped
			.entry(SymbolKey {
				kind: definition.kind,
				name: definition.name.clone(),
			})
			.or_default()
			.push(definition);
	}

	let mut statuses = HashMap::new();
	for defs in grouped.values() {
		if defs.len() < 2 {
			continue;
		}
		let base_definition = defs
			.iter()
			.find(|definition| base_mod_id.is_some_and(|base| definition.mod_id == base));
		for definition in defs {
			if let Some(base) = base_definition
				&& definition.mod_id != base.mod_id
				&& definition.normalized_statement == base.normalized_statement
			{
				statuses.insert(definition.index, OverlapStatus::DiscardableBaseCopy);
			}
		}
		let active = defs
			.iter()
			.filter(|definition| {
				statuses.get(&definition.index) != Some(&OverlapStatus::DiscardableBaseCopy)
			})
			.copied()
			.collect::<Vec<_>>();
		if active.len() < 2 {
			continue;
		}
		let group_status = if active.iter().all(|definition| definition.root_mergeable) {
			OverlapStatus::MergeCandidate
		} else {
			OverlapStatus::OvershadowConflict
		};
		for definition in active {
			statuses.insert(definition.index, group_status);
		}
	}

	statuses
}

fn symbol_kind_text(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "scripted_effect",
		SymbolKind::ScriptedTrigger => "scripted_trigger",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}
