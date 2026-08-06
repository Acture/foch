//! Integration tests for scoring an already generated product output tree.

use std::path::Path;

use foch_core::model::MergeReport;
use foch_merge_quality::corpus::Case;
use foch_merge_quality::orchestrate::{
	ScoreExistingOutputRequest, score_existing_output_with_cache,
};
use foch_merge_quality::score::ScoreCache;

fn write_file(root: &Path, relative: &str, content: &str) {
	let path = root.join(relative);
	std::fs::create_dir_all(path.parent().expect("fixture file parent"))
		.expect("create fixture file parent");
	std::fs::write(path, content).expect("write fixture file");
}

#[test]
fn scores_existing_output_without_executing_a_merge() {
	let root = tempfile::tempdir().expect("fixture root");
	let mod_a = root.path().join("mod-a");
	let mod_b = root.path().join("mod-b");
	let compatch = root.path().join("compatch");
	let out = root.path().join("out");
	let rel = "events/example.txt";
	let merged = concat!(
		"namespace = example\n",
		"country_event = { id = example.1 title = one }\n",
		"country_event = { id = example.2 title = two }\n",
	);
	write_file(
		&mod_a,
		rel,
		"namespace = example\ncountry_event = { id = example.1 title = one }\n",
	);
	write_file(
		&mod_b,
		rel,
		"namespace = example\ncountry_event = { id = example.2 title = two }\n",
	);
	write_file(&compatch, rel, merged);
	write_file(&out, rel, merged);
	let case = Case {
		compatch_id: "human".to_string(),
		title: "existing output".to_string(),
		referenced_mods: vec!["mod-a".to_string(), "mod-b".to_string()],
		..Default::default()
	};
	let report = MergeReport::default();
	let output_before = std::fs::read(out.join(rel)).expect("read output before scoring");
	let mut cache = ScoreCache::new();

	let source_dirs = [mod_a, mod_b];
	let result = score_existing_output_with_cache(
		&ScoreExistingOutputRequest {
			case: &case,
			compatch_dir: &compatch,
			source_dirs: &source_dirs,
			output_dir: &out,
			report: &report,
			basegame_root: None,
			merge_ms: 42,
		},
		&mut cache,
	)
	.expect("score existing output")
	.result;

	assert_eq!(std::fs::read(out.join(rel)).unwrap(), output_before);
	assert_eq!(result.ground_truth_files, 1);
	assert_eq!(result.multi_source_files, 1);
	assert_eq!(result.merge_status.as_deref(), Some("ready"));
	assert_eq!(result.timings.merge_ms, 42);
	assert!(result.timings.total_ms >= 42);
}

#[test]
fn definition_module_paths_collapse_into_one_existing_output_unit() {
	let root = tempfile::tempdir().expect("fixture root");
	let mod_a = root.path().join("mod-a");
	let mod_b = root.path().join("mod-b");
	let compatch = root.path().join("compatch");
	let out = root.path().join("out");
	write_file(
		&mod_a,
		"common/governments/a.txt",
		"monarchy = { rank = 1 }\n",
	);
	write_file(
		&mod_b,
		"common/governments/b.txt",
		"republic = { rank = 1 }\n",
	);
	write_file(
		&compatch,
		"common/governments/00_human.txt",
		"monarchy = { rank = 1 }\n",
	);
	write_file(
		&compatch,
		"common/governments/10_human.txt",
		"republic = { rank = 1 }\n",
	);
	let module_rel = "common/governments/zzz_foch_governments.txt";
	write_file(
		&out,
		module_rel,
		"monarchy = { rank = 1 }\nrepublic = { rank = 1 }\n",
	);
	let case = Case {
		compatch_id: "human".to_string(),
		title: "two-file governments compatch".to_string(),
		referenced_mods: vec!["mod-a".to_string(), "mod-b".to_string()],
		..Default::default()
	};
	let report = MergeReport::default();
	let mut cache = ScoreCache::new();

	let source_dirs = [mod_a, mod_b];
	let result = score_existing_output_with_cache(
		&ScoreExistingOutputRequest {
			case: &case,
			compatch_dir: &compatch,
			source_dirs: &source_dirs,
			output_dir: &out,
			report: &report,
			basegame_root: None,
			merge_ms: 0,
		},
		&mut cache,
	)
	.expect("score existing definition-module output")
	.result;

	assert_eq!(result.files.len(), 1);
	assert_eq!(result.ground_truth_files, 1);
	assert_eq!(result.multi_source_files, 1);
	assert_eq!(result.all_ground_truth_verdicts.values().sum::<usize>(), 1);
	assert_eq!(result.files[0].rel, module_rel);
}
