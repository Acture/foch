//! Kernel-neutral preparation of parsed inputs for one DAG merge.

use std::collections::HashMap;

use foch_core::config::DepOverride;
use foch_language::analyzer::parser::AstStatement;
use foch_language::analyzer::semantic_index::{ParsedScriptFile, parse_script_file};

use super::dag::{FileDag, IgnoreReplacePath, ModDag, ModId, induced_file_dag_with_overrides};
use crate::workspace::{ResolvedFileContributor, WorkspaceScriptCache};

pub(crate) struct DagMergeInputRequest<'a> {
	pub file_path: &'a str,
	pub contributors: &'a [ResolvedFileContributor],
	pub mod_dag: &'a ModDag,
	pub ignore_replace_path: &'a IgnoreReplacePath,
	pub dep_overrides: &'a [DepOverride],
	pub script_cache: Option<&'a WorkspaceScriptCache>,
}

pub(crate) struct PreparedDagMergeInput {
	pub file_dag: FileDag,
	pub vanilla: Option<ParsedScriptFile>,
	pub contributors: HashMap<ModId, ParsedScriptFile>,
}

pub(crate) fn prepare_dag_merge_input(
	request: DagMergeInputRequest<'_>,
) -> Result<PreparedDagMergeInput, String> {
	let file_dag = induced_file_dag_with_overrides(
		request.mod_dag,
		request.file_path,
		request.contributors,
		request.ignore_replace_path,
		request.dep_overrides,
	);
	let vanilla = parse_vanilla_contributor(
		request.file_path,
		request.contributors,
		request.script_cache,
	)?;
	let contributors = parse_active_mod_contributors(
		request.file_path,
		request.contributors,
		&file_dag,
		request.script_cache,
	)?;
	Ok(PreparedDagMergeInput {
		file_dag,
		vanilla,
		contributors,
	})
}

pub(crate) fn contributor_mod_hashes(
	contributors: &[ResolvedFileContributor],
	file_dag: &FileDag,
) -> HashMap<ModId, String> {
	let by_mod: HashMap<ModId, &ResolvedFileContributor> = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| (ModId(contributor.mod_id.clone()), contributor))
		.collect();
	file_dag
		.contributors()
		.iter()
		.filter_map(|mod_id| {
			let contributor = by_mod.get(mod_id)?;
			let hash = contributor.mod_hash.as_ref()?;
			Some((mod_id.clone(), hash.clone()))
		})
		.collect()
}

pub(crate) fn final_base_statements(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
) -> Vec<AstStatement> {
	if file_dag
		.contributors()
		.iter()
		.any(|mod_id| file_dag.replaces_path(mod_id))
	{
		Vec::new()
	} else {
		vanilla
			.map(|base| base.ast.statements.clone())
			.unwrap_or_default()
	}
}

pub(crate) fn template_for<'a>(
	file_dag: &FileDag,
	vanilla: Option<&'a ParsedScriptFile>,
	contributors: &'a HashMap<ModId, ParsedScriptFile>,
) -> Option<&'a ParsedScriptFile> {
	vanilla.or_else(|| {
		file_dag
			.contributors()
			.iter()
			.find_map(|mod_id| contributors.get(mod_id))
	})
}

fn parse_vanilla_contributor(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	script_cache: Option<&WorkspaceScriptCache>,
) -> Result<Option<ParsedScriptFile>, String> {
	let Some(base) = contributors
		.iter()
		.find(|contributor| contributor.is_base_game)
	else {
		return Ok(None);
	};
	if let Some(parsed) = parsed_from_cache(base, script_cache) {
		return Ok(Some(parsed));
	}
	parse_script_file(&base.mod_id, &base.root_path, &base.absolute_path)
		.map(Some)
		.ok_or_else(|| {
			format!(
				"failed to parse vanilla file {} for {file_path}",
				base.absolute_path.display()
			)
		})
}

fn parse_active_mod_contributors(
	file_path: &str,
	contributors: &[ResolvedFileContributor],
	file_dag: &FileDag,
	script_cache: Option<&WorkspaceScriptCache>,
) -> Result<HashMap<ModId, ParsedScriptFile>, String> {
	let by_mod: HashMap<ModId, &ResolvedFileContributor> = contributors
		.iter()
		.filter(|contributor| !contributor.is_base_game && !contributor.is_synthetic_base)
		.map(|contributor| (ModId(contributor.mod_id.clone()), contributor))
		.collect();
	let mut parsed = HashMap::new();
	for mod_id in file_dag.contributors() {
		let contributor = by_mod
			.get(mod_id)
			.ok_or_else(|| format!("missing contributor {} for {file_path}", mod_id.as_str()))?;
		let parsed_file = parsed_from_cache(contributor, script_cache)
			.or_else(|| {
				parse_script_file(
					&contributor.mod_id,
					&contributor.root_path,
					&contributor.absolute_path,
				)
			})
			.ok_or_else(|| {
				format!(
					"failed to parse mod file {} for {}",
					contributor.absolute_path.display(),
					contributor.mod_id,
				)
			})?;
		parsed.insert(mod_id.clone(), parsed_file);
	}
	Ok(parsed)
}

fn parsed_from_cache(
	contributor: &ResolvedFileContributor,
	script_cache: Option<&WorkspaceScriptCache>,
) -> Option<ParsedScriptFile> {
	let relative = contributor
		.absolute_path
		.strip_prefix(&contributor.root_path)
		.ok()?;
	script_cache?.get(&contributor.mod_id, relative).cloned()
}
