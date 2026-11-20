"""Offline ablation: residual categories on rewritten and unseen mods."""

import json
import unittest

import torch

from eu4_decide._ablation import (
	DISTRIBUTIONS,
	EXPORT_SEED,
	STEPS,
	TRAIN_SEEDS,
	ResidualDecision,
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
		self.assertEqual(document["seed"], EXPORT_SEED)
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
	def test_joint_arms_share_xavier_and_keep_unk_at_zero(self) -> None:
		pools = load_pools()
		states = []
		for dropout, mechanics_only in ((0.0, True), (0.0, False), (0.3, False), (0.5, False)):
			torch.manual_seed(EXPORT_SEED)
			model = ResidualDecision(
				pools["terrain"],
				pools["cb"],
				dropout,
				mechanics_only=mechanics_only,
			)
			states.append(
				{name: param.detach().clone() for name, param in model.named_parameters()}
			)
		shared = states[0]
		for other in states[1:]:
			for name, value in shared.items():
				self.assertTrue(torch.equal(value, other[name]), name)
		self.assertGreater(float(shared["terrain.f.2.weight"].abs().sum()), 0.0)
		self.assertGreater(float(shared["cb.f.2.weight"].abs().sum()), 0.0)
		self.assertEqual(float(shared["terrain.embedding.weight"].abs().sum()), 0.0)
		self.assertEqual(float(shared["cb.embedding.weight"].abs().sum()), 0.0)

	def test_fair_joint_training_and_rust_fixture(self) -> None:
		report = run_ablation()
		print(report.table())
		print(report.norms())
		self.assertEqual(report.seeds, TRAIN_SEEDS)
		self.assertEqual(
			report.steps_run,
			{"B": STEPS, "C0": STEPS, "C30": STEPS, "C50": STEPS},
		)
		for distribution in DISTRIBUTIONS:
			spread = report.metric_std[distribution]["A"]
			self.assertEqual(spread.mse, 0.0)
			self.assertEqual(spread.top1, 0.0)
			self.assertEqual(spread.spearman, 0.0)
		for distribution in OVERHAUL:
			metrics = report.metrics[distribution]
			for model in ("B", "C30", "C50"):
				self.assertLess(metrics[model].mse, metrics["A"].mse * 0.5, f"{distribution} {model}")
				self.assertGreaterEqual(
					metrics[model].top1,
					metrics["A"].top1 + 0.45,
					f"{distribution} {model}",
				)
				self.assertGreaterEqual(
					metrics[model].spearman,
					metrics["A"].spearman + 0.5,
					f"{distribution} {model}",
				)
			# C0 still learns f from a shared init, so it beats the one-hot, but the
			# margin is smaller than the dropout arms on the slices below.
			self.assertLess(metrics["C0"].mse, metrics["A"].mse, distribution)
			self.assertGreater(metrics["C0"].top1, metrics["A"].top1, distribution)
			self.assertGreater(metrics["C0"].spearman, metrics["A"].spearman, distribution)
		# Edited stats and unseen CBs drop E_id. Dropout's gain shows up there.
		for distribution in ("vanilla_stat_modified", "overhaul_unseen_cb"):
			metrics = report.metrics[distribution]
			for model in ("C30", "C50"):
				self.assertLess(
					metrics[model].mse,
					metrics["C0"].mse * 0.5,
					f"{distribution} {model}",
				)
				self.assertGreater(
					metrics[model].spearman,
					metrics["C0"].spearman + 0.05,
					f"{distribution} {model}",
				)
		modified = report.metrics["vanilla_stat_modified"]
		self.assertGreater(modified["C30"].top1, modified["C0"].top1)
		self.assertGreater(modified["C50"].top1, modified["C0"].top1)
		# Unseen-terrain groups hold the vanilla CB fixed, so the rank is f(terrain)
		# after the unknown terrain id resolves to <UNK>. C0 learns that map too.
		terrain = report.metrics["overhaul_unseen_terrain"]
		unseen_cb = report.metrics["overhaul_unseen_cb"]
		for model in ("B", "C0", "C30", "C50"):
			self.assertEqual(terrain[model].top1, 1.0, model)
			self.assertAlmostEqual(terrain[model].spearman, 1.0, places=6, msg=model)
			self.assertLess(terrain[model].mse, 0.05, model)
			self.assertEqual(unseen_cb[model].top1, 1.0, model)
		self.assertLess(abs(terrain["C0"].mse - terrain["C30"].mse), 0.02)
		self.assertLess(abs(terrain["C0"].mse - terrain["C50"].mse), 0.02)
		for model in ("B", "C0", "C30", "C50"):
			vanilla = report.metrics["vanilla_id"][model]
			self.assertGreaterEqual(vanilla.top1, 0.95, model)
			self.assertGreaterEqual(vanilla.spearman, 0.95, model)
			self.assertLess(vanilla.mse, 0.01, model)
		self.assertLess(report.embed_l2["B"], 1.0e-6)
		self.assertLess(report.grad_embed["B"], 1.0e-8)
		self.assertGreater(report.embed_l2["C30"], 0.0)
		self.assertGreater(report.embed_l2["C50"], 0.0)
		self.assertGreater(report.embed_l2["C0"], 4.0 * report.embed_l2["C30"])
		self.assertGreater(report.embed_l2["C0"], 4.0 * report.embed_l2["C50"])
		for model in ("C0", "C30", "C50"):
			self.assertGreater(report.f_l2[model], 0.5 * report.f_l2["B"], model)
		self.assertGreater(report.grad_f["C30"], report.grad_f["C0"])
		self.assertGreater(report.grad_f["C50"], report.grad_f["C0"])
		self.assertGreater(report.grad_embed["C30"], 0.0)
		self.assertLess(report.grad_embed["C0"] / report.grad_embed["C30"], 2.0)
		self.assertLess(report.grad_embed["C0"] / report.grad_embed["C50"], 2.0)
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
