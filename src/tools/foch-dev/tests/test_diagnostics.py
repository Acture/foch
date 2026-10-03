from __future__ import annotations

import io
import json
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from foch_dev.compare import (
	CompareOptions,
	build_gate_checks,
	build_path_deltas,
	build_rule_deltas,
	top_path_share,
)
from foch_dev.compare import (
	run as compare,
)
from foch_dev.models import CheckOutput, Summary, load_summary, read_check_output
from foch_dev.smoke import SmokeOptions, enabled_mod_ids, filter_playset, run
from foch_dev.summary import build_failure_summary, summarize_findings


def check_output() -> CheckOutput:
	return {
		"findings": [
			{"mod_id": mod, "rule_id": rule, "path": path, "line": 1, "message": "test"}
			for mod, rule, path in (
				("two", "S004", "common/a.txt"),
				("two", "S004", "common/a.txt"),
				("two", "S004", "common/b.txt"),
				("one", "S002", "events/a.txt"),
				(None, "A001", None),
			)
		],
		"fatal_errors": [],
		"strict_findings": [],
		"advisory_findings": [],
	}


def summary() -> Summary:
	result: Summary = summarize_findings(check_output(), {"two"})
	result["check_exit_code"] = 0
	return result


class DiagnosticTests(unittest.TestCase):
	def test_filter_preserves_playset_order_and_metadata(self) -> None:
		mods: list[dict[str, object]] = [
			{"steamId": name, "enabled": enabled}
			for name, enabled in (("two", True), ("one", True), ("three", False))
		]
		playset: dict[str, object] = {"name": "test", "mods": mods}
		filtered: dict[str, object] = filter_playset(playset, {"one", "two"})
		self.assertEqual(filtered, {"name": "test", "mods": mods[:2]})
		self.assertEqual(enabled_mod_ids(playset), {"one", "two"})
		self.assertEqual(playset["mods"], mods)

	def test_check_json_and_summary_roundtrip(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			path: Path = Path(directory) / "check.json"
			path.write_text(json.dumps(check_output()), encoding="utf-8")
			parsed: CheckOutput = read_check_output(path)
			self.assertEqual(parsed["findings"][-1]["mod_id"], None)
			result: Summary = summarize_findings(parsed, {"two"})
			result["check_exit_code"] = 0
			path.write_text(json.dumps(result), encoding="utf-8")
			loaded: Summary = load_summary(path)
			self.assertEqual(loaded["target_counts"]["S004"], 3)
			self.assertEqual(loaded["target_counts"]["S002"], 0)
			self.assertEqual(len(loaded["examples"]["S004"]), 3)
			self.assertEqual(top_path_share(loaded, "S004", 1), 2 / 3)

	def test_malformed_summary_cannot_become_zero_findings(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			path: Path = Path(directory) / "summary.json"
			for invalid in ("0", True, -1, None):
				value: dict[str, object] = dict(summary())
				value["target_counts"] = {"S004": invalid}
				path.write_text(json.dumps(value), encoding="utf-8")
				with self.subTest(invalid=invalid), self.assertRaises(ValueError):
					load_summary(path)
			path.write_text("{}", encoding="utf-8")
			with self.assertRaises(KeyError):
				load_summary(path)

	def test_rule_and_path_deltas_and_thresholds(self) -> None:
		baseline: Summary = summary()
		candidate: Summary = summary()
		candidate["target_counts"]["S004"] = 1
		candidate["focus_by_path"] = [{"path": "common/b.txt", "counts": {"S004": 1}}]
		options: CompareOptions = CompareOptions(
			Path("baseline"), Path("candidate"), gate_rule="S004", min_absolute_drop=2
		)
		self.assertTrue(
			all(item.passed for item in build_gate_checks(options, baseline, candidate))
		)
		self.assertEqual(build_rule_deltas(["S004"], baseline, candidate)[0].delta, -2)
		self.assertEqual(
			build_path_deltas("S004", baseline, candidate, 1)[0].path, "common/a.txt"
		)
		self.assertIsNone(top_path_share(candidate, "S002", 5))

	def test_failed_output_never_passes_even_with_exit_override(self) -> None:
		failed: Summary = build_failure_summary(
			{"two"}, Path("missing"), "missing output"
		)
		failed["check_exit_code"] = 0
		options: CompareOptions = CompareOptions(
			Path("base"), Path("candidate"), allow_nonzero_exit=True
		)
		for baseline, candidate in ((summary(), failed), (failed, summary())):
			self.assertFalse(
				all(
					item.passed
					for item in build_gate_checks(options, baseline, candidate)
				)
			)

	def test_comparison_exit_code_and_json_gate(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			base: Path = Path(directory) / "base.json"
			output: Path = Path(directory) / "report.json"
			base.write_text(json.dumps(summary()), encoding="utf-8")
			with redirect_stdout(io.StringIO()):
				code: int = compare(
					CompareOptions(base, base, gate_rule="S004", output=output)
				)
			self.assertEqual(code, 2)
			self.assertFalse(json.loads(output.read_text())["gate"]["passed"])

	def test_smoke_rejects_missing_or_invalid_output_and_ignores_old_files(
		self,
	) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "src/packages/foch").mkdir(parents=True)
			(root / "Cargo.toml").touch()
			(root / "src/packages/foch/Cargo.toml").touch()
			playset: Path = root / "playset.json"
			playset.write_text(
				json.dumps({"mods": [{"steamId": "two", "enabled": True}]})
			)
			out: Path = root / "output"
			out.mkdir()
			(out / "playset-all-check.json").write_text(json.dumps(check_output()))
			for content in (None, "{", "{}", json.dumps(check_output())):

				def fake_check(
					playset_path: Path,
					output_path: Path,
					repository: Path,
					content: str | None = content,
				) -> subprocess.CompletedProcess[str]:
					self.assertTrue(playset_path.is_absolute())
					self.assertEqual(repository, root.resolve())
					if content is not None:
						output_path.write_text(content)
					return subprocess.CompletedProcess(["foch"], 0, "", "")

				with (
					patch("foch_dev.smoke.run_check", side_effect=fake_check),
					redirect_stdout(io.StringIO()),
				):
					code: int = run(SmokeOptions(playset, out_dir=out, repo=root))
				self.assertEqual(
					code, 0 if content == json.dumps(check_output()) else 1
				)
			self.assertEqual(len(list(out.glob("*-summary.json"))), 4)
			self.assertEqual(len(list(out.glob("*-summary.txt"))), 4)
			self.assertFalse(list(out.glob("playset-all-????????.json")))


if __name__ == "__main__":
	unittest.main()
