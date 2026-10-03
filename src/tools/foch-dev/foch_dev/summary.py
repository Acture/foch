"""Pure diagnostic aggregation and human-readable rendering."""

from collections import Counter, defaultdict
from pathlib import Path

from .models import CheckOutput, Example, Finding, PathCounts, Summary

FOCUS_RULES: tuple[str, ...] = ("A001", "S002", "S003", "S004", "A004")
SECONDARY_RULES: tuple[str, ...] = ("A005",)


def summarize_findings(
	data: CheckOutput,
	target_mod_ids: set[str],
) -> Summary:
	all_findings: list[Finding] = list(data.get("findings", []))
	target_findings: list[Finding] = [
		finding for finding in all_findings if finding.get("mod_id") in target_mod_ids
	]

	by_rule: Counter[str] = Counter(finding["rule_id"] for finding in target_findings)
	by_path: dict[str, Counter[str]] = defaultdict(Counter)
	examples: dict[str, list[Example]] = {
		rule: [] for rule in (*FOCUS_RULES, *SECONDARY_RULES)
	}

	for finding in target_findings:
		rule_id: str = str(finding["rule_id"])
		path: str = str(finding.get("path") or "")
		by_path[path][rule_id] += 1
		if rule_id in examples and len(examples[rule_id]) < 10:
			examples[rule_id].append(
				{
					"path": path,
					"line": finding.get("line"),
					"message": finding.get("message"),
				}
			)

	focus_by_path: list[PathCounts] = []
	for path, counts in sorted(by_path.items()):
		focus_counts: dict[str, int] = {
			rule: counts[rule]
			for rule in (*FOCUS_RULES, *SECONDARY_RULES)
			if counts[rule] > 0
		}
		if focus_counts:
			focus_by_path.append({"path": path, "counts": focus_counts})

	return {
		"target_mod_ids": sorted(target_mod_ids),
		"focus_rules": list(FOCUS_RULES),
		"secondary_rules": list(SECONDARY_RULES),
		"fatal_errors": list(data.get("fatal_errors", [])),
		"global_counts": {
			"fatal_errors": len(data.get("fatal_errors", [])),
			"strict_findings": len(data.get("strict_findings", [])),
			"advisory_findings": len(data.get("advisory_findings", [])),
		},
		"target_counts": {
			rule: by_rule.get(rule, 0) for rule in (*FOCUS_RULES, *SECONDARY_RULES)
		},
		"focus_by_path": focus_by_path,
		"examples": examples,
	}


def build_failure_summary(
	target_mod_ids: set[str],
	raw_output_path: Path,
	error: str,
) -> Summary:
	return {
		"target_mod_ids": sorted(target_mod_ids),
		"focus_rules": list(FOCUS_RULES),
		"secondary_rules": list(SECONDARY_RULES),
		"global_counts": {
			"fatal_errors": 0,
			"strict_findings": 0,
			"advisory_findings": 0,
		},
		"target_counts": {rule: 0 for rule in (*FOCUS_RULES, *SECONDARY_RULES)},
		"focus_by_path": [],
		"examples": {rule: [] for rule in (*FOCUS_RULES, *SECONDARY_RULES)},
		"error": error,
		"raw_output_path": str(raw_output_path),
	}


def render_text_summary(summary: Summary) -> str:
	lines: list[str] = []
	if summary.get("error"):
		lines.append("== check failed ==")
		lines.append(f"exit_code: {summary.get('check_exit_code', '<unknown>')}")
		lines.append(f"error: {summary['error']}")
		lines.append(f"raw_output_path: {summary.get('raw_output_path', '<unknown>')}")
		if summary.get("stdout"):
			lines.append("")
			lines.append("== stdout ==")
			lines.append(str(summary["stdout"]).rstrip())
		if summary.get("stderr"):
			lines.append("")
			lines.append("== stderr ==")
			lines.append(str(summary["stderr"]).rstrip())
		return "\n".join(lines).rstrip() + "\n"

	if (
		summary.get("check_exit_code", 0) != 0
		or summary["global_counts"].get("fatal_errors", 0) > 0
	):
		lines.append("== check status ==")
		lines.append(f"exit_code: {summary.get('check_exit_code', 0)}")
		for key in ("fatal_errors", "strict_findings", "advisory_findings"):
			lines.append(f"{key}: {summary['global_counts'].get(key, 0)}")
		if summary.get("fatal_errors"):
			lines.append("")
			lines.append("== fatal errors ==")
			for error in summary["fatal_errors"]:
				lines.append(str(error))
			lines.append("")

	lines.append("== target counts ==")
	for rule in (*FOCUS_RULES, *SECONDARY_RULES):
		lines.append(f"{rule}: {summary['target_counts'].get(rule, 0)}")
	lines.append("")
	lines.append("== focus by path ==")
	for item in summary["focus_by_path"]:
		counts: str = ", ".join(
			f"{rule}={count}" for rule, count in sorted(item["counts"].items())
		)
		lines.append(f"{item['path']}: {counts}")
	lines.append("")
	lines.append("== examples ==")
	for rule in (*FOCUS_RULES, *SECONDARY_RULES):
		lines.append(f"[{rule}]")
		for example in summary["examples"].get(rule, []):
			lines.append(
				f"{example['path']}:{example.get('line')}: {example.get('message')}"
			)
		if not summary["examples"].get(rule):
			lines.append("(none)")
		lines.append("")
	return "\n".join(lines).rstrip() + "\n"
