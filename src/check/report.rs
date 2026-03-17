use crate::check::model::{
	ChannelMode, CheckResult, Finding, MergePlanEntry, MergePlanResult, MergePlanStrategy, Severity,
};

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

	for finding in findings {
		lines.push(render_finding(&finding, color));
	}

	lines.join("\n")
}

pub fn render_merge_plan_text(result: &MergePlanResult) -> String {
	let mut lines = Vec::new();
	lines.push("Foch Merge Plan".to_string());
	lines.push(format!("fatal_errors: {}", result.fatal_errors.len()));
	lines.push(format!("total_paths: {}", result.summary.total_paths));
	lines.push(format!("copy_through: {}", result.summary.copy_through));
	lines.push(format!(
		"last_writer_overlay: {}",
		result.summary.last_writer_overlay
	));
	lines.push(format!(
		"structural_merge: {}",
		result.summary.structural_merge
	));
	lines.push(format!(
		"manual_conflict: {}",
		result.summary.manual_conflict
	));

	for fatal in &result.fatal_errors {
		lines.push(format!("[FATAL] {fatal}"));
	}

	for strategy in [
		MergePlanStrategy::ManualConflict,
		MergePlanStrategy::StructuralMerge,
		MergePlanStrategy::LastWriterOverlay,
	] {
		for entry in result
			.entries
			.iter()
			.filter(|entry| entry.strategy == strategy)
		{
			lines.push(render_merge_plan_entry(entry));
		}
	}

	lines.join("\n")
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
		"[{strategy}] path={} winner={} contributors={}{}",
		entry.path, winner, contributors, notes
	)
}
