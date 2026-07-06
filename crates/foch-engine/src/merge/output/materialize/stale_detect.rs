use super::super::super::error::MergeError;
use super::super::super::patch::ClausewitzPatch;
use super::super::super::patch_merge::PatchAddress;
use super::super::stale_vanilla::detect_stale_vanilla_targets;
use super::DepMisuseRemoveCount;
use crate::emit::{EmitOptions, emit_clausewitz_statements_with_options};
use crate::workspace::{ResolvedFileContributor, WorkspaceScriptCache};
use foch_core::model::{DepMisuseFinding, StaleVanillaTargetDescriptor};
use foch_language::analyzer::content_family::MergeKeySource;
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};
use std::collections::{HashMap, HashSet};

pub(super) fn vanilla_snippet_for_address(
	vanilla: Option<&ParsedScriptFile>,
	address: &PatchAddress,
	emit_options: &EmitOptions,
) -> Option<String> {
	let vanilla = vanilla?;
	let statements = vanilla_statements_at_address(&vanilla.ast.statements, address);
	Some(match statements {
		Some(statements) if !statements.is_empty() => {
			emit_clausewitz_statements_with_options(&statements, emit_options)
				.unwrap_or_else(|err| format!("(failed to render vanilla snippet: {err})"))
		}
		_ => "(key not present in vanilla)".to_string(),
	})
}

fn vanilla_statements_at_address(
	statements: &[AstStatement],
	address: &PatchAddress,
) -> Option<Vec<AstStatement>> {
	let mut current = statements;
	for segment in &address.path {
		current = current.iter().find_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} if key == segment => Some(items.as_slice()),
			_ => None,
		})?;
	}

	let Some(key) = vanilla_address_lookup_key(&address.key) else {
		return Some(current.to_vec());
	};
	if key.is_empty() {
		return Some(current.to_vec());
	}

	let matches = current
		.iter()
		.filter(|statement| {
			matches!(statement, AstStatement::Assignment { key: statement_key, .. } if statement_key == key)
		})
		.cloned()
		.collect::<Vec<_>>();
	(!matches.is_empty()).then_some(matches)
}

fn vanilla_address_lookup_key(address_key: &str) -> Option<&str> {
	if let Some(rest) = address_key.strip_prefix("__node__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__list_item__::") {
		return rest.split("::").next();
	}
	if let Some(rest) = address_key.strip_prefix("__rename__::") {
		return Some(rest);
	}
	if address_key.starts_with("__append_block_item__::")
		|| address_key.starts_with("__remove_block_item__::")
	{
		return None;
	}
	Some(address_key)
}

pub(super) fn parse_vanilla_for_stale_detection(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	script_cache: &WorkspaceScriptCache,
) -> Result<Option<ParsedScriptFile>, MergeError> {
	let Some(base) = contributors
		.iter()
		.find(|contributor| contributor.is_base_game)
	else {
		return Ok(None);
	};
	if let Ok(relative) = base.absolute_path.strip_prefix(&base.root_path)
		&& let Some(parsed) = script_cache.get(&base.mod_id, relative)
	{
		return Ok(Some(parsed.clone()));
	}
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| MergeError::Validation {
			path: Some(file_path.to_string()),
			message: format!(
				"failed to parse vanilla file {} for stale target detection",
				base.absolute_path.display()
			),
		})
}

pub(super) fn collect_stale_vanilla_targets(
	file_path: &str,
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
	vanilla: Option<&ParsedScriptFile>,
	merge_key_source: MergeKeySource,
	mod_versions: &HashMap<String, String>,
) -> Vec<StaleVanillaTargetDescriptor> {
	mod_patches
		.iter()
		.flat_map(|(mod_id, _, patches)| {
			let mod_version = mod_versions
				.get(mod_id)
				.map(String::as_str)
				.unwrap_or("unknown");
			detect_stale_vanilla_targets(
				patches,
				file_path,
				mod_id,
				mod_version,
				vanilla,
				merge_key_source,
			)
		})
		.collect()
}

pub(super) fn collect_dep_misuse_remove_counts(
	findings: &[DepMisuseFinding],
	contributors: &[ResolvedFileContributor],
	mod_patches: &[(String, usize, Vec<ClausewitzPatch>)],
) -> Vec<DepMisuseRemoveCount> {
	if findings.is_empty() {
		return Vec::new();
	}

	let contributor_mods = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| contributor.mod_id.as_str())
		.collect::<HashSet<_>>();
	let mut counts = Vec::new();
	for finding in findings {
		if !contributor_mods.contains(finding.mod_id.as_str())
			|| !contributor_mods.contains(finding.suspicious_dep_id.as_str())
		{
			continue;
		}

		let count = mod_patches
			.iter()
			.filter(|(mod_id, _, _)| mod_id == &finding.mod_id)
			.flat_map(|(_, _, patches)| patches)
			.filter(|patch| is_remove_patch(patch))
			.count();
		if count == 0 {
			continue;
		}
		counts.push(DepMisuseRemoveCount {
			mod_id: finding.mod_id.clone(),
			dep_id: finding.suspicious_dep_id.clone(),
			count: count.min(u32::MAX as usize) as u32,
		});
	}
	counts
}

fn is_remove_patch(patch: &ClausewitzPatch) -> bool {
	matches!(
		patch,
		ClausewitzPatch::RemoveNode { .. }
			| ClausewitzPatch::RemoveListItem { .. }
			| ClausewitzPatch::RemoveBlockItem { .. }
	)
}

pub(super) fn apply_dep_misuse_remove_counts(
	findings: &mut [DepMisuseFinding],
	counts: Vec<DepMisuseRemoveCount>,
) {
	for count in counts {
		if let Some(finding) = findings.iter_mut().find(|finding| {
			finding.mod_id == count.mod_id && finding.suspicious_dep_id == count.dep_id
		}) {
			finding.evidence.false_remove_count = finding
				.evidence
				.false_remove_count
				.saturating_add(count.count);
		}
	}
}
