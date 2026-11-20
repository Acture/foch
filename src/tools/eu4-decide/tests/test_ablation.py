"""Offline ablation: residual categories on rewritten and unseen mods."""

import json
import unittest

from eu4_decide._ablation import (
	WARMUP,
	cb_term,
	fixture_path,
	load_pools,
	preference_reversal,
	run_ablation,
	terrain_term,
)

OVERHAUL = (
	"vanilla_stat_modified",
	"overhaul_unseen_terrain",
	"overhaul_unseen_cb",
)
UNSEEN = (
	"overhaul_unseen_terrain",
	"overhaul_unseen_cb",
)
REQUIRED_CASES = (
	("vanilla_id", "plains"),
	("vanilla_id", "conquest"),
	("vanilla_stat_modified", "plains"),
	("vanilla_stat_modified", "conquest"),
	("overhaul_unseen_terrain", "haunted_bog"),
	("overhaul_unseen_terrain", "dwarven_hold"),
	("overhaul_unseen_cb", "eldritch_crusade"),
	("overhaul_unseen_cb", "dwarven_reclamation"),
)


class AblationGeneratorTest(unittest.TestCase):
	def test_overhaul_rewrites_reverse_vanilla_preferences(self) -> None:
		pools = load_pools()
		reversal = preference_reversal(pools)
		self.assertLess(reversal["terrain"], -0.7)
		self.assertLess(reversal["cb"], -0.8)
		plains = terrain_term(pools["terrain"].raw_of("plains"), "plains", residual=True)
		plains_edited = terrain_term(
			pools["modified_terrain"].raw_of("plains"),
			"plains",
			residual=False,
		)
		mountains = terrain_term(pools["terrain"].raw_of("mountains"), "mountains", residual=True)
		mountains_edited = terrain_term(
			pools["modified_terrain"].raw_of("mountains"),
			"mountains",
			residual=False,
		)
		self.assertLess(plains_edited, plains - 0.8)
		self.assertGreater(mountains_edited, mountains + 0.8)
		conquest = cb_term(pools["cb"].raw_of("conquest"), "conquest", residual=True)
		conquest_edited = cb_term(
			pools["modified_cb"].raw_of("conquest"),
			"conquest",
			residual=False,
		)
		self.assertGreater(abs(conquest_edited - conquest), 0.4)
		self.assertNotIn("haunted_bog", pools["terrain"].names)
		self.assertNotIn("dwarven_hold", pools["terrain"].names)
		self.assertNotIn("eldritch_crusade", pools["cb"].names)
		self.assertIn("haunted_bog", pools["unseen_terrain"].names)
		self.assertIn("dwarven_hold", pools["unseen_terrain"].names)

	def test_fixture_covers_the_four_distributions(self) -> None:
		document = json.loads(fixture_path().read_text(encoding="utf-8"))
		self.assertEqual(document["dropout"], 0.5)
		seen = {(case["distribution"], case["name"]) for case in document["cases"]}
		distributions = {distribution for distribution, _ in seen}
		self.assertEqual(
			distributions,
			{
				"vanilla_id",
				"vanilla_stat_modified",
				"overhaul_unseen_terrain",
				"overhaul_unseen_cb",
			},
		)
		for case in REQUIRED_CASES:
			self.assertIn(case, seen)


class AblationTrainTest(unittest.TestCase):
	def test_dropout_beats_one_hot_and_matches_the_rust_fixture(self) -> None:
		report = run_ablation()
		print(report.table())
		for distribution in OVERHAUL:
			metrics = report.metrics[distribution]
			best_mse = min(metrics["C30"].mse, metrics["C50"].mse)
			best_top = max(metrics["C30"].top1, metrics["C50"].top1)
			best_rank = max(metrics["C30"].spearman, metrics["C50"].spearman)
			self.assertLess(best_mse, metrics["A"].mse * 0.5, distribution)
			self.assertGreaterEqual(best_top, metrics["A"].top1 + 0.45, distribution)
			self.assertGreaterEqual(best_rank, metrics["A"].spearman + 0.5, distribution)
			self.assertLess(metrics["B"].mse, metrics["A"].mse, distribution)
			self.assertGreaterEqual(metrics["B"].top1, metrics["A"].top1 + 0.4, distribution)
		for distribution in UNSEEN:
			metrics = report.metrics[distribution]
			best_mse = min(metrics["C30"].mse, metrics["C50"].mse)
			best_top = max(metrics["C30"].top1, metrics["C50"].top1)
			self.assertLess(best_mse, metrics["C0"].mse * 0.5, distribution)
			self.assertGreaterEqual(best_top, metrics["C0"].top1 + 0.4, distribution)
		for model in ("C0", "C30", "C50"):
			vanilla = report.metrics["vanilla_id"][model]
			self.assertGreaterEqual(vanilla.top1, 0.95, model)
			self.assertGreaterEqual(vanilla.spearman, 0.95, model)
			self.assertLess(vanilla.mse, 0.01, model)
		self.assertEqual(report.steps_run["C0"], WARMUP)
		self.assertGreater(report.steps_run["C30"], WARMUP)
		self.assertGreater(report.steps_run["C50"], WARMUP)
		self.assertLess(report.reference_error, 1.0e-5)
		committed = json.loads(fixture_path().read_text(encoding="utf-8"))
		self.assertLess(_score_gap(report.fixture, committed), 1.0e-5)


def _score_gap(fresh: dict[str, object], committed: dict[str, object]) -> float:
	fresh_cases = fresh["cases"]
	committed_cases = committed["cases"]
	if not isinstance(fresh_cases, list) or not isinstance(committed_cases, list):
		raise AssertionError("fixture cases must be a list")
	if len(fresh_cases) != len(committed_cases):
		raise AssertionError(f"{len(fresh_cases)} fresh cases vs {len(committed_cases)} committed")
	worst = 0.0
	for fresh_case, committed_case in zip(fresh_cases, committed_cases, strict=True):
		if fresh_case["distribution"] != committed_case["distribution"]:
			raise AssertionError("distribution order drifted")
		if (
			fresh_case["head"] != committed_case["head"]
			or fresh_case["name"] != committed_case["name"]
		):
			raise AssertionError("case order drifted")
		for left, right in zip(fresh_case["score"], committed_case["score"], strict=True):
			worst = max(worst, abs(float(left) - float(right)))
	return worst


if __name__ == "__main__":
	unittest.main()
