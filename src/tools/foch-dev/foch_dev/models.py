"""Validated boundaries for check output and diagnostic summaries."""

from __future__ import annotations

import json
from pathlib import Path
from typing import NotRequired, TypedDict, cast


class Finding(TypedDict):
	mod_id: str | None
	rule_id: str
	path: str | None
	line: int | None
	message: str | None


class CheckOutput(TypedDict):
	findings: list[Finding]
	fatal_errors: list[object]
	strict_findings: list[object]
	advisory_findings: list[object]


class PathCounts(TypedDict):
	path: str
	counts: dict[str, int]


class Example(TypedDict):
	path: str
	line: int | None
	message: str | None


class Summary(TypedDict):
	target_mod_ids: list[str]
	focus_rules: list[str]
	secondary_rules: list[str]
	global_counts: dict[str, int]
	target_counts: dict[str, int]
	focus_by_path: list[PathCounts]
	examples: dict[str, list[Example]]
	fatal_errors: NotRequired[list[object]]
	check_exit_code: NotRequired[int]
	error: NotRequired[str]
	raw_output_path: NotRequired[str]
	playset_path: NotRequired[str]
	selected_mod_filters: NotRequired[list[str]]
	stdout: NotRequired[str]
	stderr: NotRequired[str]


def object_value(value: object) -> dict[str, object]:
	if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
		raise ValueError("expected a JSON object")
	return cast(dict[str, object], value)


def array_value(value: object) -> list[object]:
	if not isinstance(value, list):
		raise ValueError("expected a JSON array")
	return cast(list[object], value)


def string_value(value: object) -> str:
	if not isinstance(value, str):
		raise ValueError("expected a string")
	return value


def integer_value(value: object) -> int:
	if not isinstance(value, int) or isinstance(value, bool):
		raise ValueError("expected an integer")
	return value


def counts_value(value: object) -> dict[str, int]:
	counts: dict[str, int] = {
		key: integer_value(count) for key, count in object_value(value).items()
	}
	if any(count < 0 for count in counts.values()):
		raise ValueError("finding counts cannot be negative")
	return counts


def strings_value(value: object) -> list[str]:
	return [string_value(item) for item in array_value(value)]


def example_value(value: object) -> Example:
	item: dict[str, object] = object_value(value)
	return {
		"path": string_value(item.get("path") or ""),
		"line": integer_value(item["line"]) if item.get("line") is not None else None,
		"message": string_value(item["message"])
		if item.get("message") is not None
		else None,
	}


def read_check_output(path: Path) -> CheckOutput:
	data: dict[str, object] = object_value(json.loads(path.read_text(encoding="utf-8")))
	findings: list[Finding] = []
	for value in array_value(data["findings"]):
		item: dict[str, object] = object_value(value)
		example: Example = example_value(item)
		findings.append(
			{
				"mod_id": string_value(item["mod_id"])
				if item.get("mod_id") is not None
				else None,
				"rule_id": string_value(item["rule_id"]),
				"path": example["path"],
				"line": example["line"],
				"message": example["message"],
			}
		)
	return {
		"findings": findings,
		"fatal_errors": array_value(data["fatal_errors"]),
		"strict_findings": array_value(data["strict_findings"]),
		"advisory_findings": array_value(data["advisory_findings"]),
	}


def load_summary(path: Path) -> Summary:
	data: dict[str, object] = object_value(json.loads(path.read_text(encoding="utf-8")))
	paths: list[PathCounts] = []
	for value in array_value(data["focus_by_path"]):
		item: dict[str, object] = object_value(value)
		paths.append(
			{"path": string_value(item["path"]), "counts": counts_value(item["counts"])}
		)
	counts: dict[str, int] = counts_value(data["global_counts"])
	for key in ("fatal_errors", "strict_findings", "advisory_findings"):
		if key not in counts:
			raise ValueError(f"missing global count: {key}")
	summary: Summary = {
		"target_mod_ids": strings_value(data["target_mod_ids"]),
		"focus_rules": strings_value(data["focus_rules"]),
		"secondary_rules": strings_value(data["secondary_rules"]),
		"global_counts": counts,
		"target_counts": counts_value(data["target_counts"]),
		"focus_by_path": paths,
		"examples": {
			rule: [example_value(item) for item in array_value(values)]
			for rule, values in object_value(data["examples"]).items()
		},
		"check_exit_code": integer_value(data["check_exit_code"]),
	}
	if "error" in data:
		summary["error"] = string_value(data["error"])
	return summary
