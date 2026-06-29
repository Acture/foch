//! Writers for the harness artifacts: `results.json`, `report.md`, `rules.md`.
//!
//! All output is deterministic (sorted keys, stable ordering) so diffs are
//! meaningful and CI can gate on the content.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::orchestrate::CaseResult;

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

/// Read `{results_dir}/results.json`, classify verdict distribution, write
/// `{results_dir}/rules.md`.
pub fn write_rules_md(results_dir: &Path, results: &[CaseResult]) -> std::io::Result<()> {
	fs::create_dir_all(results_dir)?;
	let md = render_rules(results);
	fs::write(results_dir.join("rules.md"), md)
}

// ------------------------------------------------------------------ internals

const VERDICT_MEANING: &[(&str, &str)] = &[
	(
		"matches_human",
		"foch's merge ≈ the hand-written compatch (same defs, ≥0.92 similar)",
	),
	(
		"diverges_formatting",
		"same definitions, different text/formatting",
	),
	(
		"diverges_structure",
		"different set of top-level definitions vs the human",
	),
	(
		"drops_content",
		"foch lost a top-level def present in mod A or B (load-order failure mode)",
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

	// Aggregate across all cases (overlap files only, via the verdicts map)
	let mut agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut total_overlap: usize = 0;
	for r in results {
		for (v, n) in &r.verdicts {
			*agg.entry(v.clone()).or_default() += n;
			total_overlap += n;
		}
	}

	lines.push(format!(
		"Cases scored: **{}**  ·  overlapping ground-truth files: **{}**",
		results.len(),
		total_overlap
	));
	lines.push(String::new());
	lines.push("## Corpus verdicts (overlapping files)".to_string());
	lines.push(String::new());
	lines.push("| verdict | count | meaning |".to_string());
	lines.push("|---|---|---|".to_string());

	// Sort by count desc, then name asc for determinism
	let mut agg_sorted: Vec<(String, usize)> = agg.into_iter().collect();
	agg_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
	for (v, n) in &agg_sorted {
		lines.push(format!("| `{}` | {} | {} |", v, n, verdict_meaning(v)));
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
			"- merge: status={} parse_errors={} · ground-truth files={} overlap={}",
			r.merge_status.as_deref().unwrap_or("?"),
			val_parse_errors,
			r.ground_truth_files,
			r.overlap_files
		));
		lines.push(format!("- verdicts: {:?}", r.verdicts));

		for f in &r.files {
			if !f.overlap {
				continue;
			}
			let mut extra = String::new();
			if let Some(s) = f.similarity {
				extra.push_str(&format!(" sim={s}"));
			}
			if !f.dropped_keys.is_empty() {
				let shown: Vec<_> = f.dropped_keys.iter().take(4).cloned().collect();
				extra.push_str(&format!(" dropped={shown:?}"));
			}
			lines.push(format!("  - `{}` → **{}**{}", f.rel, f.verdict, extra));
		}
		lines.push(String::new());
	}

	lines.join("\n")
}

fn render_rules(results: &[CaseResult]) -> String {
	// Aggregate foch verdicts across all overlapping files in all cases.
	let mut agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut conflict_agg: BTreeMap<String, usize> = BTreeMap::new();
	let mut total_overlap: usize = 0;

	for r in results {
		for (v, n) in &r.verdicts {
			*agg.entry(v.clone()).or_default() += n;
			total_overlap += n;
			if v == "conflict_withheld" {
				// conflict_withheld is its own category — track separately
				// for the "actionable set" section
				*conflict_agg.entry(v.clone()).or_default() += n;
			}
		}
	}

	let mut lines = vec![
		"# foch merge-quality: verdict summary (learned from corpus)".to_string(),
		String::new(),
	];

	lines.push(format!(
		"Corpus: **{}** case(s), **{}** overlapping ground-truth files.",
		results.len(),
		total_overlap
	));
	lines.push(String::new());

	lines.push("## Verdict distribution (foch's output vs. human compatch)".to_string());
	lines.push(String::new());
	lines.push(
		"Each file that both patched mods touch (overlap) is classified by how foch's merge \
		 compares to the human-authored compatch."
			.to_string(),
	);
	lines.push(String::new());
	lines.push("| verdict | count | meaning |".to_string());
	lines.push("|---|---|---|".to_string());

	let mut agg_sorted: Vec<(String, usize)> = agg.iter().map(|(k, v)| (k.clone(), *v)).collect();
	agg_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
	for (v, n) in &agg_sorted {
		lines.push(format!("| `{}` | {} | {} |", v, n, verdict_meaning(v)));
	}

	lines.push(String::new());
	lines.push("## Actionable set: `conflict_withheld`".to_string());
	lines.push(String::new());
	lines.push(
		"Files foch declined to auto-merge (surfaced as manual conflicts). The human compatch \
		 resolved each by hand — these are the cases where foch's merge strategy most clearly \
		 diverges from the human author's intent and are the primary improvement target."
			.to_string(),
	);
	lines.push(String::new());

	let withheld = agg.get("conflict_withheld").copied().unwrap_or(0);
	lines.push(format!(
		"**{withheld}** of **{total_overlap}** overlapping files were withheld as conflicts \
		 ({:.0}%).",
		if total_overlap > 0 {
			withheld as f64 / total_overlap as f64 * 100.0
		} else {
			0.0
		}
	));
	lines.push(String::new());

	lines.push("## Per-case summary".to_string());
	lines.push(String::new());
	lines.push("| case | compatch | overlap | verdicts |".to_string());
	lines.push("|---|---|---|---|".to_string());
	for r in results {
		let v_summary = r
			.verdicts
			.iter()
			.map(|(k, v)| format!("{k}:{v}"))
			.collect::<Vec<_>>()
			.join(", ");
		lines.push(format!(
			"| {} | `{}` | {} | {} |",
			r.title, r.compatch_id, r.overlap_files, v_summary
		));
	}
	lines.push(String::new());

	lines.join("\n")
}
