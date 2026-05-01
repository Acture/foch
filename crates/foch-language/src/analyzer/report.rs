use foch_core::model::{
	ChannelMode, CheckResult, Finding, FindingChannel, MergePlanEntry, MergePlanResult,
	MergePlanStrategy, MergeReport, MergeReportStatus, Severity,
};
use std::collections::BTreeMap;

pub fn render_text(result: &CheckResult, color: bool, channel: ChannelMode) -> String {
	let findings = result.filtered_findings(channel);
	let mut lines = Vec::new();
	lines.push("Foch Check Report".to_string());
	lines.push(format!("fatal_errors: {}", result.fatal_errors.len()));
	lines.push(format!("findings: {}", findings.len()));
	lines.push(format!("strict_findings: {}", result.strict_findings.len()));
	lines.push(format!(
		"advisory_findings: {}",
		result.advisory_findings.len()
	));
	lines.push(format!(
		"analysis: text_documents={} parsed_files={} parse_errors={} scopes={} defs={} refs={} aliases={}",
		result.analysis_meta.text_documents,
		result.analysis_meta.parsed_files,
		result.analysis_meta.parse_errors,
		result.analysis_meta.scopes,
		result.analysis_meta.symbol_definitions,
		result.analysis_meta.symbol_references,
		result.analysis_meta.alias_usages
	));
	lines.push(format!(
		"parse_families: clausewitz_mainline={{documents:{} failed:{} issues:{}}} localisation={{documents:{} failed:{} issues:{}}} csv={{documents:{} failed:{} issues:{}}} json={{documents:{} failed:{} issues:{}}}",
		result.analysis_meta.parse_stats.clausewitz_mainline.documents,
		result
			.analysis_meta
			.parse_stats
			.clausewitz_mainline
			.parse_failed_documents,
		result
			.analysis_meta
			.parse_stats
			.clausewitz_mainline
			.parse_issue_count,
		result.analysis_meta.parse_stats.localisation.documents,
		result
			.analysis_meta
			.parse_stats
			.localisation
			.parse_failed_documents,
		result
			.analysis_meta
			.parse_stats
			.localisation
			.parse_issue_count,
		result.analysis_meta.parse_stats.csv.documents,
		result.analysis_meta.parse_stats.csv.parse_failed_documents,
		result.analysis_meta.parse_stats.csv.parse_issue_count,
		result.analysis_meta.parse_stats.json.documents,
		result.analysis_meta.parse_stats.json.parse_failed_documents,
		result.analysis_meta.parse_stats.json.parse_issue_count
	));

	for fatal in &result.fatal_errors {
		lines.push(format!("[FATAL] {fatal}"));
	}

	for finding in &findings {
		lines.push(render_finding(finding, color));
	}

	append_findings_by_rule_summary(&mut lines, &findings, color);

	lines.join("\n")
}

fn append_findings_by_rule_summary(lines: &mut Vec<String>, findings: &[Finding], color: bool) {
	lines.push(String::new());

	if findings.is_empty() {
		lines.push("Findings by rule: (no findings)".to_string());
		return;
	}

	let mut counts: BTreeMap<(String, u8, u8), usize> = BTreeMap::new();
	for finding in findings {
		*counts
			.entry((
				finding.rule_id.clone(),
				severity_order(finding.severity),
				channel_order(finding.channel),
			))
			.or_insert(0) += 1;
	}

	let mut sorted = counts.into_iter().collect::<Vec<_>>();
	sorted.sort_by(
		|((rule_a, severity_a, channel_a), count_a), ((rule_b, severity_b, channel_b), count_b)| {
			count_b
				.cmp(count_a)
				.then_with(|| rule_a.cmp(rule_b))
				.then_with(|| severity_a.cmp(severity_b))
				.then_with(|| channel_a.cmp(channel_b))
		},
	);

	let count_width = sorted
		.iter()
		.map(|(_, count)| count.to_string().len())
		.max()
		.unwrap_or("count".len())
		.max("count".len());

	lines.push("Findings by rule:".to_string());
	lines.push(format!(
		"  {:>count_width$}  {:<7}  {:<9}  {}",
		"count", "severity", "channel", "rule_id"
	));
	lines.push(format!(
		"  {:>count_width$}  {:<7}  {:<9}  {}",
		"-".repeat(count_width),
		"--------",
		"---------",
		"-------"
	));

	for ((rule_id, severity, channel), count) in sorted {
		lines.push(format!(
			"  {:>count_width$}  {}  {:<9}  {}",
			count,
			render_summary_severity(severity_from_order(severity), color),
			channel_label(channel_from_order(channel)),
			rule_id
		));
	}
	lines.push(format!("  (total: {})", findings.len()));
}

fn severity_order(severity: Severity) -> u8 {
	match severity {
		Severity::Error => 0,
		Severity::Warning => 1,
		Severity::Info => 2,
	}
}

fn severity_from_order(order: u8) -> Severity {
	match order {
		0 => Severity::Error,
		1 => Severity::Warning,
		_ => Severity::Info,
	}
}

fn channel_order(channel: FindingChannel) -> u8 {
	match channel {
		FindingChannel::Strict => 0,
		FindingChannel::Advisory => 1,
	}
}

fn channel_from_order(order: u8) -> FindingChannel {
	match order {
		0 => FindingChannel::Strict,
		_ => FindingChannel::Advisory,
	}
}

fn severity_label(severity: Severity) -> &'static str {
	match severity {
		Severity::Error => "Error",
		Severity::Warning => "Warning",
		Severity::Info => "Info",
	}
}

fn channel_label(channel: FindingChannel) -> &'static str {
	match channel {
		FindingChannel::Strict => "Strict",
		FindingChannel::Advisory => "Advisory",
	}
}

fn render_summary_severity(severity: Severity, color: bool) -> String {
	let padded = format!("{:<7}", severity_label(severity));
	if color {
		match severity {
			Severity::Error => console::style(padded).red().bold().to_string(),
			Severity::Warning => console::style(padded).yellow().bold().to_string(),
			Severity::Info => console::style(padded).cyan().bold().to_string(),
		}
	} else {
		padded
	}
}

pub fn render_merge_plan_text(result: &MergePlanResult) -> String {
	let mut lines = Vec::new();
	lines.push("Foch Merge Plan".to_string());
	lines.push(format!("game: {}", result.game));
	lines.push(format!("playset_name: {}", result.playset_name));
	lines.push(format!("generated_at: {}", result.generated_at));
	lines.push(format!("include_game_base: {}", result.include_game_base));
	lines.push(format!("fatal_errors: {}", result.fatal_errors.len()));
	lines.push(format!("total_paths: {}", result.strategies.total_paths));
	lines.push(format!("copy_through: {}", result.strategies.copy_through));
	lines.push(format!(
		"last_writer_overlay: {}",
		result.strategies.last_writer_overlay
	));
	lines.push(format!(
		"structural_merge: {}",
		result.strategies.structural_merge
	));
	lines.push(format!(
		"localisation_merge: {}",
		result.strategies.localisation_merge
	));
	lines.push(format!(
		"manual_conflict: {}",
		result.strategies.manual_conflict
	));

	for fatal in &result.fatal_errors {
		lines.push(format!("[FATAL] {fatal}"));
	}

	for strategy in [
		MergePlanStrategy::ManualConflict,
		MergePlanStrategy::StructuralMerge,
		MergePlanStrategy::LocalisationMerge,
		MergePlanStrategy::LastWriterOverlay,
	] {
		for entry in result
			.paths
			.iter()
			.filter(|entry| entry.strategy == strategy)
		{
			lines.push(render_merge_plan_entry(entry));
		}
	}

	lines.join("\n")
}

pub fn merge_plan_exit_code(result: &MergePlanResult) -> i32 {
	if result.has_fatal_errors() {
		1
	} else if result.has_manual_conflicts() {
		2
	} else {
		0
	}
}

pub fn render_merge_report_text(report: &MergeReport) -> String {
	let mut lines = Vec::new();
	lines.push("Foch Merge Report".to_string());
	lines.push(format!(
		"status: {}",
		render_merge_report_status(report.status)
	));
	lines.push(format!(
		"manual_conflict_count: {}",
		report.manual_conflict_count
	));
	lines.push(format!(
		"fallback_resolved_count: {}",
		report.fallback_resolved_count
	));
	lines.push(format!(
		"generated_file_count: {}",
		report.generated_file_count
	));
	lines.push(format!("copied_file_count: {}", report.copied_file_count));
	lines.push(format!("overlay_file_count: {}", report.overlay_file_count));
	lines.push(format!(
		"noop_skipped_file_count: {}",
		report.noop_skipped_file_count
	));
	lines.push(format!(
		"validation: fatal_errors={} strict_findings={} advisory_findings={} parse_errors={} unresolved_references={} missing_localisation={}",
		report.validation.fatal_errors,
		report.validation.strict_findings,
		report.validation.advisory_findings,
		report.validation.parse_errors,
		report.validation.unresolved_references,
		report.validation.missing_localisation
	));
	append_dep_misuse_section(&mut lines, report);
	append_version_mismatch_section(&mut lines, report);
	lines.join("\n")
}

fn append_version_mismatch_section(lines: &mut Vec<String>, report: &MergeReport) {
	if report.version_mismatch.is_empty() {
		return;
	}

	lines.push(String::new());
	lines.push(format!(
		"⚠ Mod supported_version mismatch detected ({} findings):",
		report.version_mismatch.len()
	));
	for finding in &report.version_mismatch {
		let marker = match finding.severity {
			Severity::Info => "ℹ",
			Severity::Warning => "⚠",
			Severity::Error => "✖",
		};
		lines.push(String::new());
		lines.push(format!(
			"  {marker} mod {} ({}) declares supported_version = {}",
			finding.mod_id,
			quote(&finding.mod_display_name),
			quote(&finding.supported_version)
		));
		lines.push(format!(
			"  but vanilla game version is {}.",
			quote(&finding.game_version)
		));
		lines.push(format!("  {}", finding.message));
	}
}

fn append_dep_misuse_section(lines: &mut Vec<String>, report: &MergeReport) {
	if report.dep_misuse.is_empty() {
		return;
	}

	lines.push(String::new());
	lines.push(format!(
		"⚠ Dependency misuse detected ({} findings):",
		report.dep_misuse.len()
	));
	for finding in &report.dep_misuse {
		lines.push(String::new());
		lines.push(format!(
			"  mod {} ({}) declares",
			finding.mod_id,
			quote(&finding.mod_display_name)
		));
		lines.push(format!(
			"    dependencies = {{ {} }}",
			quote(&finding.suspicious_dep_display_name)
		));
		lines.push("  in its descriptor.mod, but its source does not reference any".to_string());
		lines.push("  symbols defined by that mod.".to_string());
		if finding.evidence.false_remove_count > 0 {
			lines.push(format!(
				"  This caused {} false-positive deletion patches.",
				finding.evidence.false_remove_count
			));
		} else {
			lines.push(
                "  No deletion patches were observed in this merge run, but the dependency still looks non-semantic."
                    .to_string(),
            );
		}
		lines.push(String::new());
		lines.push(
			"  Recommendation: contact the mod author and ask them to remove this".to_string(),
		);
		lines.push(
			"  entry from dependencies={}, or override locally with foch.toml when supported:"
				.to_string(),
		);
		lines.push("    [[overrides]]".to_string());
		lines.push(format!("    mod = {}", quote(&finding.mod_id)));
		lines.push(format!("    dep = {}", quote(&finding.suspicious_dep_id)));
	}
}

fn quote(value: &str) -> String {
	format!("\"{}\"", value.replace('"', "\\\""))
}

pub fn merge_report_exit_code(report: &MergeReport) -> i32 {
	match report.status {
		MergeReportStatus::Ready => 0,
		MergeReportStatus::PartialSuccess => 0,
		MergeReportStatus::Blocked => 2,
		MergeReportStatus::Fatal => 1,
	}
}

fn render_finding(finding: &Finding, color: bool) -> String {
	let level = match finding.severity {
		Severity::Error => "ERROR",
		Severity::Warning => "WARN",
		Severity::Info => "INFO",
	};
	let level = if color {
		match finding.severity {
			Severity::Error => console::style(level).red().bold().to_string(),
			Severity::Warning => console::style(level).yellow().bold().to_string(),
			Severity::Info => console::style(level).cyan().bold().to_string(),
		}
	} else {
		level.to_string()
	};

	let path = finding
		.path
		.as_ref()
		.map(|value| value.display().to_string())
		.unwrap_or_else(|| "<none>".to_string());
	let mod_id = finding
		.mod_id
		.clone()
		.unwrap_or_else(|| "<none>".to_string());
	let evidence = finding.evidence.clone().unwrap_or_default();
	let line = finding
		.line
		.map(|value| value.to_string())
		.unwrap_or_else(|| "-".to_string());
	let column = finding
		.column
		.map(|value| value.to_string())
		.unwrap_or_else(|| "-".to_string());

	format!(
		"[{level}] {} {} channel={:?} mod={} path={} line={} col={} {}",
		finding.rule_id, finding.message, finding.channel, mod_id, path, line, column, evidence
	)
}

fn render_merge_plan_entry(entry: &MergePlanEntry) -> String {
	let strategy = match entry.strategy {
		MergePlanStrategy::CopyThrough => "COPY_THROUGH",
		MergePlanStrategy::LastWriterOverlay => "LAST_WRITER_OVERLAY",
		MergePlanStrategy::StructuralMerge => "STRUCTURAL_MERGE",
		MergePlanStrategy::LocalisationMerge => "LOCALISATION_MERGE",
		MergePlanStrategy::ManualConflict => "MANUAL_CONFLICT",
	};
	let contributors = entry
		.contributors
		.iter()
		.map(|item| item.mod_id.clone())
		.collect::<Vec<_>>()
		.join(" -> ");
	let winner = entry
		.winner
		.as_ref()
		.map(|item| item.mod_id.clone())
		.unwrap_or_else(|| "<none>".to_string());
	let notes = if entry.notes.is_empty() {
		String::new()
	} else {
		format!(" notes={}", entry.notes.join(" | "))
	};

	format!(
		"[{strategy}] path={} winner={} generated={} contributors={}{}",
		entry.path, winner, entry.generated, contributors, notes
	)
}

fn render_merge_report_status(status: MergeReportStatus) -> &'static str {
	match status {
		MergeReportStatus::Ready => "READY",
		MergeReportStatus::PartialSuccess => "PARTIAL_SUCCESS",
		MergeReportStatus::Blocked => "BLOCKED",
		MergeReportStatus::Fatal => "FATAL",
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn finding(rule_id: &str, severity: Severity, channel: FindingChannel) -> Finding {
		Finding {
			rule_id: rule_id.to_string(),
			severity,
			channel,
			message: "synthetic finding".to_string(),
			mod_id: None,
			path: None,
			evidence: None,
			line: None,
			column: None,
			confidence: None,
		}
	}

	#[test]
	fn render_text_appends_findings_by_rule_summary() {
		let mut result = CheckResult {
			findings: vec![
				finding("alpha-rule", Severity::Warning, FindingChannel::Advisory),
				finding("beta-rule", Severity::Error, FindingChannel::Strict),
				finding("alpha-rule", Severity::Warning, FindingChannel::Advisory),
			],
			..Default::default()
		};
		result.recompute_channels();

		let output = render_text(&result, false, ChannelMode::All);
		let summary = output
			.split("Findings by rule:")
			.nth(1)
			.expect("summary section should be present");

		assert!(summary.contains("alpha-rule"));
		assert!(summary.contains("beta-rule"));
		assert!(summary.contains("Warning"));
		assert!(summary.contains("Advisory"));
		assert!(summary.contains("Error"));
		assert!(summary.contains("Strict"));
		assert!(summary.contains("(total: 3)"));
		assert!(summary.lines().any(|line| {
			line.contains('2')
				&& line.contains("Warning")
				&& line.contains("Advisory")
				&& line.contains("alpha-rule")
		}));
		assert!(summary.lines().any(|line| {
			line.contains('1')
				&& line.contains("Error")
				&& line.contains("Strict")
				&& line.contains("beta-rule")
		}));
		assert!(summary.find("alpha-rule") < summary.find("beta-rule"));
	}

	#[test]
	fn render_text_appends_empty_findings_summary() {
		let output = render_text(&CheckResult::default(), false, ChannelMode::All);

		assert!(output.contains("\nFindings by rule: (no findings)"));
		assert!(!output.contains("  count  severity"));
	}
}
