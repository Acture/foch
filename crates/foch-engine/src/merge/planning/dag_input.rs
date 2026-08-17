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

pub(crate) fn merge_ancestor_statements(vanilla: Option<&ParsedScriptFile>) -> Vec<AstStatement> {
	// `replace_path` controls loader visibility and is already reflected in the
	// active DAG and each contributor's complete source view. Observed vanilla
	// remains the epistemic ancestor used to compare independent replacements;
	// applying a replacement source against this root still deletes every base
	// statement that the source omits.
	vanilla
		.map(|base| base.ast.statements.clone())
		.unwrap_or_default()
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
	parse_contributor(base, script_cache)
		.map(Some)
		.map_err(|error| format!("failed to parse vanilla file for {file_path}: {error}"))
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
		let parsed_file = parse_contributor(contributor, script_cache).map_err(|error| {
			format!(
				"failed to parse mod file for {} in {file_path}: {error}",
				contributor.mod_id
			)
		})?;
		parsed.insert(mod_id.clone(), parsed_file);
	}
	Ok(parsed)
}

fn parse_contributor(
	contributor: &ResolvedFileContributor,
	script_cache: Option<&WorkspaceScriptCache>,
) -> Result<ParsedScriptFile, String> {
	if let Some(script_cache) = script_cache {
		return script_cache
			.load(contributor)
			.map(|parsed| (*parsed).clone());
	}
	parse_script_file(
		&contributor.mod_id,
		&contributor.root_path,
		&contributor.absolute_path,
	)
	.ok_or_else(|| format!("failed to parse {}", contributor.absolute_path.display()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn verified_cache_error_never_falls_back_to_direct_parsing() {
		let temp = TempDir::new().expect("temp dir");
		let relative = "events/test.txt";
		fs::create_dir_all(temp.path().join("events")).expect("create events dir");
		fs::write(
			temp.path().join(relative),
			"country_event = { id = test.1 }\n",
		)
		.expect("write script");
		let contributor = ResolvedFileContributor {
			mod_id: "mod-a".to_string(),
			root_path: temp.path().to_path_buf(),
			absolute_path: temp.path().join(relative),
			precedence: 1,
			is_base_game: false,
			is_synthetic_base: false,
			parse_ok_hint: Some(true),
			mod_hash: Some("hash-a".to_string()),
		};
		let cache = WorkspaceScriptCache::default();

		let error = parse_contributor(&contributor, Some(&cache))
			.expect_err("missing verified entry must fail closed");
		assert!(error.contains("no semantic-snapshot input"));
		assert!(
			parse_contributor(&contributor, None).is_ok(),
			"direct parsing remains available only when no cache was supplied"
		);
	}
}
