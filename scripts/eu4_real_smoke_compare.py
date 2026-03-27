#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable

Summary = dict[str, Any]


@dataclass(frozen=True)
class RunStatus:
	label: str
	path: str
	check_exit_code: int
	fatal_errors: int
	strict_findings: int
	advisory_findings: int


@dataclass(frozen=True)
class RuleDelta:
	rule: str
	baseline: int
	candidate: int
	delta: int
	relative_delta: float | None


@dataclass(frozen=True)
class PathDelta:
	path: str
	baseline: int
	candidate: int
	delta: int


@dataclass(frozen=True)
class GateCheck:
	name: str
	passed: bool
	details: str


def parse_args() -> argparse.Namespace:
	parser: argparse.ArgumentParser = argparse.ArgumentParser(
		description=(
			"Compare two eu4_real_smoke summaries and optionally enforce an issue "
			"exit gate."
		),
	)
	parser.add_argument("baseline", type=Path, help="Baseline *-summary.json path")
	parser.add_argument("candidate", type=Path, help="Candidate *-summary.json path")
	parser.add_argument(
		"--rule",
		dest="rules",
		action="append",
		default=[],
		help="Rule to compare. Repeat to show multiple rules. Defaults to all rules found.",
	)
	parser.add_argument(
		"--gate-rule",
		type=str,
		default=None,
		help="Primary rule used for pass/fail gating, for example S004 or S002.",
	)
	parser.add_argument(
		"--min-absolute-drop",
		type=int,
		default=None,
		help="Require the gate rule count to drop by at least this many findings.",
	)
	parser.add_argument(
		"--min-relative-drop",
		type=float,
		default=None,
		help=(
			"Require the gate rule count to drop by at least this fraction of the "
			"baseline, for example 0.08 for 8%%."
		),
	)
	parser.add_argument(
		"--max-top-path-share",
		type=float,
		default=None,
		help=(
			"Require the gate rule's top-N hotspot share in the candidate summary to "
			"stay below this fraction."
		),
	)
	parser.add_argument(
		"--top-path-limit",
		type=int,
		default=5,
		help="Number of hotspot paths to show per rule and to use for hotspot-share gating.",
	)
	parser.add_argument(
		"--allow-nonzero-exit",
		action="store_true",
		help="Do not fail the gate when the candidate smoke run returned a non-zero exit code.",
	)
	parser.add_argument(
		"--allow-fatal-errors",
		action="store_true",
		help="Do not fail the gate when the candidate summary contains fatal errors.",
	)
	parser.add_argument(
		"--output",
		type=Path,
		default=None,
		help="Optional JSON output path for the comparison report.",
	)
	return parser.parse_args()


def load_summary(path: Path) -> Summary:
	try:
		return json.loads(path.read_text(encoding="utf-8"))
	except FileNotFoundError as err:
		raise SystemExit(f"summary file not found: {path}") from err
	except json.JSONDecodeError as err:
		raise SystemExit(f"summary file is not valid JSON: {path}: {err}") from err


def coerce_int(value: Any) -> int:
	if isinstance(value, bool):
		return int(value)
	if isinstance(value, int):
		return value
	if isinstance(value, float):
		return int(value)
	if isinstance(value, str):
		try:
			return int(value)
		except ValueError:
			return 0
	return 0


def ordered_rules(baseline: Summary, candidate: Summary) -> list[str]:
	ordered: list[str] = []
	seen: set[str] = set()
	for summary in (baseline, candidate):
		for section in ("focus_rules", "secondary_rules"):
			raw_values: Any = summary.get(section, [])
			if not isinstance(raw_values, list):
				continue
			for value in raw_values:
				rule: str = str(value).strip()
				if rule and rule not in seen:
					seen.add(rule)
					ordered.append(rule)
		raw_counts: Any = summary.get("target_counts", {})
		if isinstance(raw_counts, dict):
			for key in raw_counts:
				rule = str(key).strip()
				if rule and rule not in seen:
					seen.add(rule)
					ordered.append(rule)
	return ordered


def selected_rules(
	requested: Iterable[str], baseline: Summary, candidate: Summary
) -> list[str]:
	requested_rules: list[str] = [rule.strip() for rule in requested if rule.strip()]
	if requested_rules:
		return requested_rules
	return ordered_rules(baseline, candidate)


def run_status(label: str, path: Path, summary: Summary) -> RunStatus:
	global_counts: Any = summary.get("global_counts", {})
	if not isinstance(global_counts, dict):
		global_counts = {}
	return RunStatus(
		label=label,
		path=str(path),
		check_exit_code=coerce_int(summary.get("check_exit_code", 0)),
		fatal_errors=coerce_int(global_counts.get("fatal_errors", 0)),
		strict_findings=coerce_int(global_counts.get("strict_findings", 0)),
		advisory_findings=coerce_int(global_counts.get("advisory_findings", 0)),
	)


def rule_count(summary: Summary, rule: str) -> int:
	raw_counts: Any = summary.get("target_counts", {})
	if not isinstance(raw_counts, dict):
		return 0
	return coerce_int(raw_counts.get(rule, 0))


def build_rule_deltas(
	rules: Iterable[str], baseline: Summary, candidate: Summary
) -> list[RuleDelta]:
	deltas: list[RuleDelta] = []
	for rule in rules:
		baseline_count: int = rule_count(baseline, rule)
		candidate_count: int = rule_count(candidate, rule)
		delta: int = candidate_count - baseline_count
		relative_delta: float | None = None
		if baseline_count > 0:
			relative_delta = delta / baseline_count
		deltas.append(
			RuleDelta(
				rule=rule,
				baseline=baseline_count,
				candidate=candidate_count,
				delta=delta,
				relative_delta=relative_delta,
			)
		)
	return deltas


def path_counts(summary: Summary, rule: str) -> Counter[str]:
	counts: Counter[str] = Counter()
	raw_focus_by_path: Any = summary.get("focus_by_path", [])
	if not isinstance(raw_focus_by_path, list):
		return counts
	for item in raw_focus_by_path:
		if not isinstance(item, dict):
			continue
		path: str = str(item.get("path", "")).strip()
		raw_rule_counts: Any = item.get("counts", {})
		if not path or not isinstance(raw_rule_counts, dict):
			continue
		rule_count_value: int = coerce_int(raw_rule_counts.get(rule, 0))
		if rule_count_value > 0:
			counts[path] = rule_count_value
	return counts


def build_path_deltas(
	rule: str, baseline: Summary, candidate: Summary, limit: int
) -> list[PathDelta]:
	baseline_counts: Counter[str] = path_counts(baseline, rule)
	candidate_counts: Counter[str] = path_counts(candidate, rule)
	paths: set[str] = set(baseline_counts) | set(candidate_counts)
	deltas: list[PathDelta] = []
	for path in paths:
		baseline_value: int = baseline_counts.get(path, 0)
		candidate_value: int = candidate_counts.get(path, 0)
		deltas.append(
			PathDelta(
				path=path,
				baseline=baseline_value,
				candidate=candidate_value,
				delta=candidate_value - baseline_value,
			)
		)
	deltas.sort(
		key=lambda item: (
			-max(item.baseline, item.candidate),
			-abs(item.delta),
			item.path,
		)
	)
	return deltas[:limit]


def top_path_share(summary: Summary, rule: str, top_n: int) -> float | None:
	total: int = rule_count(summary, rule)
	if total <= 0:
		return None
	counts: Counter[str] = path_counts(summary, rule)
	top_total: int = sum(count for _, count in counts.most_common(max(top_n, 1)))
	return top_total / total


def format_change(delta: int, relative_delta: float | None) -> str:
	sign: str = "+" if delta > 0 else ""
	if relative_delta is None:
		return f"{sign}{delta}"
	return f"{sign}{delta} ({relative_delta:+.1%})"


def format_share(value: float | None) -> str:
	if value is None:
		return "n/a"
	return f"{value:.1%}"


def build_gate_checks(
	args: argparse.Namespace,
	baseline_summary: Summary,
	candidate_summary: Summary,
) -> list[GateCheck]:
	checks: list[GateCheck] = []
	candidate_status: RunStatus = run_status(
		"candidate", args.candidate, candidate_summary
	)
	if not args.allow_nonzero_exit:
		checks.append(
			GateCheck(
				name="candidate exit code",
				passed=candidate_status.check_exit_code == 0,
				details=f"expected 0, got {candidate_status.check_exit_code}",
			)
		)
	if not args.allow_fatal_errors:
		checks.append(
			GateCheck(
				name="candidate fatal errors",
				passed=candidate_status.fatal_errors == 0,
				details=f"expected 0, got {candidate_status.fatal_errors}",
			)
		)
	if not args.gate_rule:
		return checks

	baseline_count: int = rule_count(baseline_summary, args.gate_rule)
	candidate_count: int = rule_count(candidate_summary, args.gate_rule)
	drop: int = baseline_count - candidate_count
	relative_drop: float | None = None
	if baseline_count > 0:
		relative_drop = drop / baseline_count

	if (
		args.min_absolute_drop is None
		and args.min_relative_drop is None
		and args.max_top_path_share is None
	):
		passed_direction: bool = (
			candidate_count == 0
			if baseline_count == 0
			else candidate_count < baseline_count
		)
		checks.append(
			GateCheck(
				name=f"{args.gate_rule} count direction",
				passed=passed_direction,
				details=f"baseline {baseline_count}, candidate {candidate_count}",
			)
		)

	if args.min_absolute_drop is not None:
		checks.append(
			GateCheck(
				name=f"{args.gate_rule} absolute drop",
				passed=drop >= args.min_absolute_drop,
				details=f"required >= {args.min_absolute_drop}, got {drop}",
			)
		)

	if args.min_relative_drop is not None:
		relative_drop_passed: bool = (
			candidate_count == 0
			if baseline_count == 0
			else relative_drop is not None and relative_drop >= args.min_relative_drop
		)
		checks.append(
			GateCheck(
				name=f"{args.gate_rule} relative drop",
				passed=relative_drop_passed,
				details=(
					f"required >= {args.min_relative_drop:.1%}, "
					f"got {format_share(relative_drop)}"
				),
			)
		)

	if args.max_top_path_share is not None:
		share: float | None = top_path_share(
			candidate_summary, args.gate_rule, args.top_path_limit
		)
		checks.append(
			GateCheck(
				name=f"{args.gate_rule} top-{args.top_path_limit} hotspot share",
				passed=share is None or share <= args.max_top_path_share,
				details=(
					f"required <= {args.max_top_path_share:.1%}, "
					f"got {format_share(share)}"
				),
			)
		)

	return checks


def render_report(
	baseline_status: RunStatus,
	candidate_status: RunStatus,
	rule_deltas: list[RuleDelta],
	path_deltas_by_rule: dict[str, list[PathDelta]],
	path_share_by_rule: dict[str, dict[str, float | None]],
	gate_checks: list[GateCheck],
) -> str:
	lines: list[str] = []
	lines.append("== baseline ==")
	lines.append(f"path: {baseline_status.path}")
	lines.append(f"check_exit_code: {baseline_status.check_exit_code}")
	lines.append(f"fatal_errors: {baseline_status.fatal_errors}")
	lines.append(f"strict_findings: {baseline_status.strict_findings}")
	lines.append(f"advisory_findings: {baseline_status.advisory_findings}")
	lines.append("")
	lines.append("== candidate ==")
	lines.append(f"path: {candidate_status.path}")
	lines.append(f"check_exit_code: {candidate_status.check_exit_code}")
	lines.append(f"fatal_errors: {candidate_status.fatal_errors}")
	lines.append(f"strict_findings: {candidate_status.strict_findings}")
	lines.append(f"advisory_findings: {candidate_status.advisory_findings}")
	lines.append("")
	lines.append("== rule deltas ==")
	for delta in rule_deltas:
		lines.append(
			f"{delta.rule}: {delta.baseline} -> {delta.candidate} "
			f"({format_change(delta.delta, delta.relative_delta)})"
		)
	lines.append("")

	for delta in rule_deltas:
		lines.append(f"== hotspot deltas: {delta.rule} ==")
		shares: dict[str, float | None] = path_share_by_rule[delta.rule]
		lines.append(
			f"top_path_share({len(path_deltas_by_rule[delta.rule])} shown): "
			f"{format_share(shares['baseline'])} -> {format_share(shares['candidate'])}"
		)
		for path_delta in path_deltas_by_rule[delta.rule]:
			lines.append(
				f"{path_delta.path}: {path_delta.baseline} -> {path_delta.candidate} "
				f"({format_change(path_delta.delta, None)})"
			)
		if not path_deltas_by_rule[delta.rule]:
			lines.append("(none)")
		lines.append("")

	if gate_checks:
		passed: bool = all(check.passed for check in gate_checks)
		lines.append("== gate ==")
		lines.append(f"result: {'PASS' if passed else 'FAIL'}")
		for check in gate_checks:
			status: str = "ok" if check.passed else "fail"
			lines.append(f"[{status}] {check.name}: {check.details}")
		lines.append("")

	return "\n".join(lines).rstrip() + "\n"


def build_report_json(
	baseline_status: RunStatus,
	candidate_status: RunStatus,
	rule_deltas: list[RuleDelta],
	path_deltas_by_rule: dict[str, list[PathDelta]],
	path_share_by_rule: dict[str, dict[str, float | None]],
	gate_checks: list[GateCheck],
) -> dict[str, Any]:
	return {
		"baseline": asdict(baseline_status),
		"candidate": asdict(candidate_status),
		"rule_deltas": [asdict(item) for item in rule_deltas],
		"hotspot_deltas": {
			rule: [asdict(item) for item in values]
			for rule, values in path_deltas_by_rule.items()
		},
		"hotspot_shares": path_share_by_rule,
		"gate": {
			"passed": all(check.passed for check in gate_checks)
			if gate_checks
			else None,
			"checks": [asdict(check) for check in gate_checks],
		},
	}


def main() -> int:
	args: argparse.Namespace = parse_args()
	if args.top_path_limit <= 0:
		raise SystemExit("--top-path-limit must be greater than 0")
	if args.gate_rule is None and (
		args.min_absolute_drop is not None
		or args.min_relative_drop is not None
		or args.max_top_path_share is not None
	):
		raise SystemExit(
			"--gate-rule is required when using --min-absolute-drop, "
			"--min-relative-drop, or --max-top-path-share"
		)

	baseline_summary: Summary = load_summary(args.baseline)
	candidate_summary: Summary = load_summary(args.candidate)
	rules: list[str] = selected_rules(args.rules, baseline_summary, candidate_summary)
	if not rules:
		raise SystemExit("no comparable rules found in the provided summaries")
	if args.gate_rule and args.gate_rule not in rules:
		rules.append(args.gate_rule)

	baseline_status: RunStatus = run_status("baseline", args.baseline, baseline_summary)
	candidate_status: RunStatus = run_status(
		"candidate", args.candidate, candidate_summary
	)
	rule_deltas: list[RuleDelta] = build_rule_deltas(
		rules, baseline_summary, candidate_summary
	)
	path_deltas_by_rule: dict[str, list[PathDelta]] = {
		rule: build_path_deltas(
			rule, baseline_summary, candidate_summary, args.top_path_limit
		)
		for rule in rules
	}
	path_share_by_rule: dict[str, dict[str, float | None]] = {
		rule: {
			"baseline": top_path_share(baseline_summary, rule, args.top_path_limit),
			"candidate": top_path_share(candidate_summary, rule, args.top_path_limit),
		}
		for rule in rules
	}
	gate_checks: list[GateCheck] = build_gate_checks(
		args, baseline_summary, candidate_summary
	)

	report_text: str = render_report(
		baseline_status,
		candidate_status,
		rule_deltas,
		path_deltas_by_rule,
		path_share_by_rule,
		gate_checks,
	)
	sys.stdout.write(report_text)

	if args.output is not None:
		report: dict[str, Any] = build_report_json(
			baseline_status,
			candidate_status,
			rule_deltas,
			path_deltas_by_rule,
			path_share_by_rule,
			gate_checks,
		)
		args.output.parent.mkdir(parents=True, exist_ok=True)
		args.output.write_text(
			json.dumps(report, ensure_ascii=False, indent=2) + "\n",
			encoding="utf-8",
		)

	if gate_checks and not all(check.passed for check in gate_checks):
		return 2
	return 0


if __name__ == "__main__":
	raise SystemExit(main())
