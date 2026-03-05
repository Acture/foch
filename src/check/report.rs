use crate::check::model::{ChannelMode, CheckResult, Finding, Severity};

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
		"analysis: parsed_files={} parse_errors={} scopes={} defs={} refs={} aliases={}",
		result.analysis_meta.parsed_files,
		result.analysis_meta.parse_errors,
		result.analysis_meta.scopes,
		result.analysis_meta.symbol_definitions,
		result.analysis_meta.symbol_references,
		result.analysis_meta.alias_usages
	));

	for fatal in &result.fatal_errors {
		lines.push(format!("[FATAL] {fatal}"));
	}

	for finding in findings {
		lines.push(render_finding(&finding, color));
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
