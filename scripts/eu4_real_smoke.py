#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

FOCUS_RULES: tuple[str, ...] = ("A001", "S002", "S003", "S004", "A004")
SECONDARY_RULES: tuple[str, ...] = ("A005",)


def parse_args() -> argparse.Namespace:
	parser: argparse.ArgumentParser = argparse.ArgumentParser(
		description="Run local EU4 real-corpus smoke checks and summarize targeted rules.",
	)
	parser.add_argument("--playset", required=True, type=Path)
	parser.add_argument("--mods", type=str, default="")
	parser.add_argument("--out-dir", type=Path, default=None)
	return parser.parse_args()


def repo_root() -> Path:
	return Path(__file__).resolve().parent.parent


def default_out_dir() -> Path:
	return repo_root() / "target" / "eu4-real-smoke"


def slugify(value: str) -> str:
	parts: list[str] = []
	for ch in value:
		if ch.isalnum():
			parts.append(ch.lower())
		else:
			parts.append("-")
	slug: str = "".join(parts).strip("-")
	while "--" in slug:
		slug = slug.replace("--", "-")
	return slug or "smoke"


def load_playset(path: Path) -> dict[str, Any]:
	return json.loads(path.read_text(encoding="utf-8"))


def normalize_mod_filter(raw: str) -> set[str]:
	return {part.strip() for part in raw.split(",") if part.strip()}


def filter_playset(playset: dict[str, Any], selected: set[str]) -> dict[str, Any]:
	if not selected:
		return playset
	filtered_mods: list[dict[str, Any]] = []
	for mod in playset.get("mods", []):
		if not isinstance(mod, dict):
			continue
		steam_id: str = str(mod.get("steamId", "")).strip()
		display_name: str = str(mod.get("displayName", "")).strip()
		if steam_id in selected or display_name in selected:
			filtered_mods.append(mod)
	filtered: dict[str, Any] = dict(playset)
	filtered["mods"] = filtered_mods
	return filtered


def enabled_mod_ids(playset: dict[str, Any]) -> set[str]:
	mod_ids: set[str] = set()
	for mod in playset.get("mods", []):
		if not isinstance(mod, dict) or not mod.get("enabled", False):
			continue
		steam_id: str = str(mod.get("steamId", "")).strip()
		display_name: str = str(mod.get("displayName", "")).strip()
		if steam_id:
			mod_ids.add(steam_id)
		elif display_name:
			mod_ids.add(display_name)
	return mod_ids


def run_check(playset_path: Path, output_path: Path) -> subprocess.CompletedProcess[str]:
	command: list[str] = [
		"cargo",
		"run",
		"--offline",
		"--",
		"check",
		str(playset_path),
		"--format",
		"json",
		"--output",
		str(output_path),
	]
	return subprocess.run(
		command,
		cwd=repo_root(),
		text=True,
		capture_output=True,
		check=False,
	)


def summarize_findings(
	data: dict[str, Any],
	target_mod_ids: set[str],
) -> dict[str, Any]:
	all_findings: list[dict[str, Any]] = list(data.get("findings", []))
	target_findings: list[dict[str, Any]] = [
		finding for finding in all_findings if finding.get("mod_id") in target_mod_ids
	]

	by_rule: Counter[str] = Counter(finding["rule_id"] for finding in target_findings)
	by_path: dict[str, Counter[str]] = defaultdict(Counter)
	examples: dict[str, list[dict[str, Any]]] = {rule: [] for rule in (*FOCUS_RULES, *SECONDARY_RULES)}

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

	focus_by_path: list[dict[str, Any]] = []
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
		"global_counts": {
			"fatal_errors": len(data.get("fatal_errors", [])),
			"strict_findings": len(data.get("strict_findings", [])),
			"advisory_findings": len(data.get("advisory_findings", [])),
		},
		"target_counts": {rule: by_rule.get(rule, 0) for rule in (*FOCUS_RULES, *SECONDARY_RULES)},
		"focus_by_path": focus_by_path,
		"examples": examples,
	}


def render_text_summary(summary: dict[str, Any]) -> str:
	lines: list[str] = []
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


def main() -> int:
	args: argparse.Namespace = parse_args()
	out_dir: Path = args.out_dir or default_out_dir()
	out_dir.mkdir(parents=True, exist_ok=True)

	playset: dict[str, Any] = load_playset(args.playset)
	selected: set[str] = normalize_mod_filter(args.mods)
	filtered: dict[str, Any] = filter_playset(playset, selected)
	target_mod_ids: set[str] = enabled_mod_ids(filtered)
	playset_slug: str = slugify(
		f"{args.playset.stem}-{'-'.join(sorted(selected)) if selected else 'all'}"
	)

	with tempfile.NamedTemporaryFile(
		"w",
		delete=False,
		encoding="utf-8",
		dir=out_dir,
		suffix=".json",
		prefix=f"{playset_slug}-",
	) as handle:
		json.dump(filtered, handle, ensure_ascii=False)
		temp_playset_path: Path = Path(handle.name)

	raw_output_path: Path = out_dir / f"{playset_slug}-check.json"
	summary_json_path: Path = out_dir / f"{playset_slug}-summary.json"
	summary_text_path: Path = out_dir / f"{playset_slug}-summary.txt"

	try:
		result: subprocess.CompletedProcess[str] = run_check(temp_playset_path, raw_output_path)
		data: dict[str, Any] = json.loads(raw_output_path.read_text(encoding="utf-8"))
		summary: dict[str, Any] = summarize_findings(data, target_mod_ids)
		summary["playset_path"] = str(args.playset)
		summary["selected_mod_filters"] = sorted(selected)
		summary["check_exit_code"] = result.returncode
		summary["stdout"] = result.stdout
		summary["stderr"] = result.stderr

		summary_json_path.write_text(
			json.dumps(summary, ensure_ascii=False, indent=2) + "\n",
			encoding="utf-8",
		)
		summary_text_path.write_text(render_text_summary(summary), encoding="utf-8")

		print(summary_text_path)
		return result.returncode
	finally:
		temp_playset_path.unlink(missing_ok=True)


if __name__ == "__main__":
	raise SystemExit(main())
