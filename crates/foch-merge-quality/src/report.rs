//! Writers for the harness artifacts: `results.json`, `report.md`, `rules.md`.
//!
//! Verdict output is deterministic (sorted keys, stable ordering) so scoring
//! diffs are meaningful. Timing fields are current-run observations.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::orchestrate::{CaseResult, ResolutionRow};

// ------------------------------------------------------------------ public API

/// Serialise `results` to `{results_dir}/results.json` (pretty-printed, stable).
pub fn write_results_json(results_dir: &Path, results: &[CaseResult]) -> std::io::Result<()> {
	fs::create_dir_all(results_dir)?;
	let json = serde_json::to_string_pretty(results).expect("CaseResult is always serialisable");
	fs::write(results_dir.join("results.json"), json)
}

/// Render a summary `{results_dir}/report.md` from scored results.
pub fn write_report_md(results_dir: &Path, results: &[CaseResult]) -> std::io::Result<()> {
	fs::create_dir_all(results_dir)?;
	let md = render_report(results);
	fs::write(results_dir.join("report.md"), md)
}

/// Write `{results_dir}/rules.md` from pre-classified resolution rows.
pub fn write_rules_md(results_dir: &Path, rows: &[ResolutionRow]) -> std::io::Result<()> {
	fs::create_dir_all(results_dir)?;
	let md = render_rules(rows);
	fs::write(results_dir.join("rules.md"), md)
}

// ------------------------------------------------------------------ internals

const VERDICT_MEANING: &[(&str, &str)] = &[
	(
		"matches_human",
		"foch's merge is AST-equivalent and ≥0.92 text-similar to the hand-written compatch",
	),
	(
		"matches_ast",
		"foch's merge is AST-equivalent under the corpus ordering policy",
	),
	(
		"accepted_equivalent",
		"foch differs from the human AST but is equivalent under an explicit scorer policy",
	),
	(
		"accepted_better",
		"foch differs from the human AST but is manually adjudicated as better",
	),
	(
		"diverges_formatting",
		"AST comparison unavailable; same top-level definitions, different text",
	),
	(
		"diverges_ast",
		"same top-level definitions, but parsed AST differs under the corpus ordering policy",
	),
	(
		"diverges_structure",
		"different set of top-level definitions vs the human",
	),
	(
		"drops_content",
		"foch lost a top-level definition present in at least one source mod",
	),
	(
		"conflict_withheld",
		"foch surfaced a manual conflict; the human resolved it by hand",
	),
	("not_emitted", "foch produced no file for this path"),
];

fn verdict_meaning(verdict: &str) -> &'static str {
	VERDICT_MEANING
		.iter()
		.find(|(k, _)| *k == verdict)
		.map(|(_, v)| *v)
		.unwrap_or("")
}

fn render_report(results: &[CaseResult]) -> String {
	let mut lines = vec!["# foch merge-quality report".to_string(), String::new()];

	let mut all_agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut multi_source_agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut total_ground_truth = 0_usize;
	let mut total_multi_source = 0_usize;
	let mut accepted_ground_truth = 0_usize;
	let mut accepted_multi_source = 0_usize;
	let mut total_setup_ms: u64 = 0;
	let mut total_merge_ms: u64 = 0;
	let mut total_scoring_ms: u64 = 0;
	let mut total_case_ms: u64 = 0;
	for r in results {
		for (verdict, count) in &r.all_ground_truth_verdicts {
			*all_agg.entry(verdict.clone()).or_default() += count;
		}
		for (verdict, count) in &r.multi_source_verdicts {
			*multi_source_agg.entry(verdict.clone()).or_default() += count;
		}
		total_ground_truth += r.ground_truth_files;
		total_multi_source += r.multi_source_files;
		accepted_ground_truth += r.accepted_ground_truth_files;
		accepted_multi_source += r.accepted_multi_source_files;
		total_setup_ms = total_setup_ms.saturating_add(r.timings.setup_ms);
		total_merge_ms = total_merge_ms.saturating_add(r.timings.merge_ms);
		total_scoring_ms = total_scoring_ms.saturating_add(r.timings.scoring_ms);
		total_case_ms = total_case_ms.saturating_add(r.timings.total_ms);
	}

	lines.push(format!(
		"Cases scored: **{}** · all ground-truth accepted: **{}/{}** · multi-source accepted: **{}/{}**",
		results.len(),
		accepted_ground_truth,
		total_ground_truth,
		accepted_multi_source,
		total_multi_source,
	));
	lines.push(format!(
		"Timing: total={} ms · setup={} ms · merge={} ms · scoring={} ms",
		total_case_ms, total_setup_ms, total_merge_ms, total_scoring_ms
	));
	lines.push(String::new());
	lines.push("## Multi-source verdicts".to_string());
	lines.push(String::new());
	lines.push("| verdict | count | meaning |".to_string());
	lines.push("|---|---|---|".to_string());

	// Sort by count desc, then name asc for determinism
	let mut agg_sorted: Vec<(String, usize)> = multi_source_agg.into_iter().collect();
	agg_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
	for (v, n) in &agg_sorted {
		lines.push(format!("| `{}` | {} | {} |", v, n, verdict_meaning(v)));
	}
	lines.push(String::new());
	lines.push("## All ground-truth verdicts".to_string());
	lines.push(String::new());
	lines.push("| verdict | count | meaning |".to_string());
	lines.push("|---|---|---|".to_string());
	let mut all_sorted: Vec<(String, usize)> = all_agg.into_iter().collect();
	all_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
	for (verdict, count) in &all_sorted {
		lines.push(format!(
			"| `{verdict}` | {count} | {} |",
			verdict_meaning(verdict)
		));
	}

	lines.push(String::new());
	lines.push("## Per-case".to_string());
	lines.push(String::new());

	for r in results {
		lines.push(format!(
			"### {} (`{}`) — patches {}",
			r.title,
			r.compatch_id,
			r.patched.join(" + ")
		));
		let val_parse_errors = r
			.validation
			.as_ref()
			.and_then(|v| v.get("parse_errors"))
			.and_then(Value::as_u64)
			.map(|n| n.to_string())
			.unwrap_or_else(|| "?".to_string());
		lines.push(format!(
			"- merge: status={} parse_errors={} · ground-truth={}/{} accepted · multi-source={}/{} accepted",
			r.merge_status.as_deref().unwrap_or("?"),
			val_parse_errors,
			r.accepted_ground_truth_files,
			r.ground_truth_files,
			r.accepted_multi_source_files,
			r.multi_source_files,
		));
		lines.push(format!(
			"- timings: total={} ms · setup={} ms · merge={} ms · scoring={} ms",
			r.timings.total_ms, r.timings.setup_ms, r.timings.merge_ms, r.timings.scoring_ms
		));
		lines.push(format!(
			"- multi-source verdicts: {:?}",
			r.multi_source_verdicts
		));

		for f in &r.files {
			if !f.multi_source {
				continue;
			}
			let mut extra = String::new();
			if let Some(s) = f.similarity {
				extra.push_str(&format!(" sim={s}"));
			}
			if let Some(ast_match) = f.ast_match {
				extra.push_str(&format!(" ast_match={ast_match}"));
			}
			if !f.dropped_keys.is_empty() {
				let shown: Vec<_> = f.dropped_keys.iter().take(4).cloned().collect();
				extra.push_str(&format!(" dropped={shown:?}"));
			}
			if let Some(reason) = &f.acceptance_reason {
				extra.push_str(&format!(" accepted_reason={reason:?}"));
			}
			lines.push(format!("  - `{}` → **{}**{}", f.rel, f.verdict, extra));
		}
		lines.push(String::new());
	}

	lines.join("\n")
}

const RES_MEANING: &[(&str, &str)] = &[
	(
		"union",
		"human kept BOTH contributors' unique content (do-both)",
	),
	(
		"took_base",
		"human kept the base (first) mod, dropped the overlay's unique content",
	),
	(
		"took_overlay",
		"human kept the overlay (later) mod = load-order / last-writer",
	),
	(
		"partial_union",
		"human retained unique content from some, but not all, contributors",
	),
	(
		"hand_edit",
		"human wrote something not derivable from either side",
	),
	(
		"identical",
		"both contributors identical here (no real conflict)",
	),
];

fn res_meaning(verdict: &str) -> &'static str {
	RES_MEANING
		.iter()
		.find(|(k, _)| *k == verdict)
		.map(|(_, v)| *v)
		.unwrap_or("")
}

/// Render `rules.md` — port of Python `cmd_learn` output (four sections).
fn render_rules(rows: &[ResolutionRow]) -> String {
	// Aggregate (all use count-desc then name-asc for determinism).
	let mut agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut agg_conflict: BTreeMap<String, usize> = BTreeMap::new();
	let mut crosstab: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();

	for row in rows {
		let verdict = row.resolution.verdict.as_str().to_string();
		let rel_kind = row.resolution.relationship.as_str().to_string();
		*agg.entry(verdict.clone()).or_default() += 1;
		*crosstab
			.entry(rel_kind)
			.or_default()
			.entry(verdict.clone())
			.or_default() += 1;
		if row.foch_verdict == "conflict_withheld" {
			*agg_conflict.entry(verdict).or_default() += 1;
		}
	}

	let sort_desc = |map: &BTreeMap<String, usize>| -> Vec<(String, usize)> {
		let mut v: Vec<(String, usize)> = map.iter().map(|(k, &n)| (k.clone(), n)).collect();
		v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
		v
	};

	let mut lines = vec![
		"# Human resolution rules (learned from compatches)".to_string(),
		String::new(),
	];

	lines.push(format!("Overlapping files analysed: **{}**", rows.len()));
	lines.push(String::new());

	// --- Section 1: crosstab ---
	lines.push(
		"## Order-independent rule: contributor relationship -> human resolution".to_string(),
	);
	lines.push(String::new());
	lines.push(
		"The honest signal is the relationship between the two contributors (not which".to_string(),
	);
	lines.push(
		"side won, which depends on load order). `disjoint`=additive, `redundant`=heavily"
			.to_string(),
	);
	lines.push(
		"overlapping mechanics (e.g. renamed dupes), `subset`=one contained in the other."
			.to_string(),
	);
	lines.push(String::new());
	lines.push("| contributor relationship | human resolutions |".to_string());
	lines.push("|---|---|".to_string());
	let mut ct_sorted: Vec<(&String, &BTreeMap<String, usize>)> = crosstab.iter().collect();
	ct_sorted.sort_by(|a, b| {
		let sa: usize = a.1.values().sum();
		let sb: usize = b.1.values().sum();
		sb.cmp(&sa).then(a.0.cmp(b.0))
	});
	for (rel_kind, verdict_map) in &ct_sorted {
		let dist = sort_desc(verdict_map)
			.iter()
			.map(|(v, n)| format!("{v}:{n}"))
			.collect::<Vec<_>>()
			.join(", ");
		lines.push(format!("| `{rel_kind}` | {dist} |"));
	}
	lines.push(String::new());

	// --- Section 2: ALL overlaps ---
	lines.push("## How humans resolve overlaps (ALL overlapping files)".to_string());
	lines.push(String::new());
	lines.push("| human resolution | count | meaning |".to_string());
	lines.push("|---|---|---|".to_string());
	for (v, n) in sort_desc(&agg) {
		lines.push(format!("| `{v}` | {n} | {} |", res_meaning(&v)));
	}
	lines.push(String::new());

	// --- Section 3: conflict_withheld subset ---
	lines
		.push("## How humans resolve the conflicts foch WITHHELD (the actionable set)".to_string());
	lines.push(String::new());
	if agg_conflict.is_empty() {
		lines.push("_(no conflict_withheld files in the corpus)_".to_string());
	} else {
		lines.push("| human resolution | count |".to_string());
		lines.push("|---|---|".to_string());
		for (v, n) in sort_desc(&agg_conflict) {
			lines.push(format!("| `{v}` | {n} |"));
		}
	}
	lines.push(String::new());

	// --- Section 4: per-file detail ---
	lines.push("## Per-file detail".to_string());
	lines.push(String::new());
	lines.push(
		"| case | file | foch | relationship | human | source_sim | retention | human_only | base_removed |"
			.to_string(),
	);
	lines.push("|---|---|---|---|---|---|---|---|---|".to_string());
	for row in rows {
		let r = &row.resolution;
		let retention = r
			.contributors
			.iter()
			.map(|contributor| {
				let fraction = contributor
					.fraction_kept
					.map_or_else(|| "n/a".to_string(), |value| value.to_string());
				format!("{}:{fraction}", contributor.source_id)
			})
			.collect::<Vec<_>>()
			.join(", ");
		lines.push(format!(
			"| {} | `{}` | {} | {} | **{}** | {} | {} | {} | {} |",
			row.title,
			row.rel,
			row.foch_verdict,
			r.relationship.as_str(),
			r.verdict.as_str(),
			r.source_jaccard,
			retention,
			r.human_only_atoms,
			r.basegame_atoms_subtracted,
		));
	}
	lines.push(String::new());

	lines.join("\n")
}
