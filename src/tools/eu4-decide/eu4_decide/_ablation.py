"""Fixed-seed ablation of residual categories on overhaul mods.

The generator is synthetic, CPU-only, and finishes in a few seconds. A
decision is one march under one casus belli. Its reward is linear in the
normalized terrain mechanics (march cost, defence dice, combat width,
supply, attrition) and CB mechanics (AE, prestige, province warscore,
warscore cost, ticking warscore), plus a small residual that exists only
for an unmodified vanilla id.

Training pairs are vanilla terrains and vanilla casus belli. Group A is
the least-squares one-hot of those ids: unseen terrain falls back to
``plains``, unseen CB to ``conquest``, and edited numbers are ignored.
Group B is ``f(modifiers)`` with ``E_id`` held at 0. Group C is
``ResidualCategory`` (``f + E_id``) at id-dropout ``p in {0, 0.3, 0.5}``.
Dropout models warm up the embedding first. Training then stops once the
dropout loss is fit, so ``p = 0`` can finish from ``E_id`` alone while
``p > 0`` still has to learn ``f`` on the dropped rows. At test time an
edited or unknown id contributes ``E_id = 0``.
"""

from __future__ import annotations

import json
import random
import struct
from dataclasses import dataclass
from pathlib import Path

import torch
from torch import nn

from eu4_decide._residual import CategoryTable, ResidualCategory

SEED = 20261120
WIDTH = 16
STEPS = 420
WARMUP = 80
STOP_MSE = 1.0e-4
F_LR = 0.03
EMBED_LR = 0.2
F_DECAY = 0.02
GROUPS = 64
GROUP_SIZE = 4
MARGIN = 0.35

TERRAIN_AXES = (
	"movement_cost",
	"defence",
	"combat_width",
	"supply_limit",
	"attrition",
)
CB_AXES = (
	"ae_impact",
	"prestige",
	"province_warscore",
	"warscore_cost",
	"ticking_warscore",
)
TERRAIN_DIV = (2.0, 2.0, 2.0, 8.0, 0.05)
CB_DIV = (1.5, 15.0, 1.5, 1.5, 0.5)
# movement_cost and defence and attrition hurt the attacker; width and supply help.
TERRAIN_W = (-0.50, -0.65, 0.35, 0.60, -0.25)
# AE and warscore cost hurt; prestige, province warscore, and ticking help.
CB_W = (-0.70, 0.40, 0.75, -0.45, 0.35)

VANILLA_TERRAIN: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("plains", (1.00, 0.0, 0.0, 6.0, 0.00)),
	("farmlands", (1.00, -1.0, 0.0, 8.0, 0.00)),
	("forest", (1.40, 1.0, 0.0, 4.0, 0.01)),
	("woods", (1.25, 1.0, 0.0, 5.0, 0.01)),
	("hills", (1.50, 1.0, -1.0, 4.0, 0.01)),
	("highlands", (1.60, 1.0, -1.0, 4.0, 0.02)),
	("mountains", (2.00, 2.0, -2.0, 2.0, 0.02)),
	("marsh", (1.50, 1.0, 0.0, 3.0, 0.05)),
	("jungle", (1.50, 1.0, -1.0, 3.0, 0.04)),
	("desert", (1.10, 0.0, 0.0, 2.0, 0.03)),
	("coastal_desert", (1.10, 0.0, 0.0, 3.0, 0.02)),
	("drylands", (1.15, 0.0, 0.0, 3.0, 0.02)),
	("steppe", (1.00, 0.0, 0.0, 5.0, 0.01)),
	("savannah", (1.10, 0.0, 0.0, 4.0, 0.01)),
	("glacier", (1.80, 1.0, -1.0, 1.0, 0.03)),
)
# Same names, non-uniform rewrite: easy ground becomes lethal and the
# harsh ground is opened up, so a plains/mountains one-hot ranks backwards.
MODIFIED_TERRAIN: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("plains", (2.20, 2.0, -2.0, 1.5, 0.06)),
	("farmlands", (2.00, 2.0, -1.0, 2.0, 0.05)),
	("forest", (1.15, 0.0, 0.0, 7.0, 0.00)),
	("woods", (1.90, 2.0, -1.0, 2.0, 0.04)),
	("hills", (1.10, 0.0, 0.0, 6.5, 0.00)),
	("highlands", (1.05, 0.0, 0.0, 7.5, 0.00)),
	("mountains", (1.00, -1.0, 0.0, 9.0, 0.00)),
	("marsh", (1.05, 0.0, 0.0, 7.0, 0.00)),
	("jungle", (1.10, 0.0, 1.0, 7.5, 0.00)),
	("desert", (1.00, -1.0, 0.0, 8.0, 0.00)),
	("coastal_desert", (2.10, 2.0, -2.0, 1.0, 0.06)),
	("drylands", (1.20, 0.0, 0.0, 6.0, 0.00)),
	("steppe", (1.80, 1.0, -1.0, 2.0, 0.04)),
	("savannah", (1.70, 1.0, -1.0, 2.5, 0.04)),
	("glacier", (1.00, 0.0, 0.0, 8.0, 0.00)),
)
UNSEEN_TERRAIN: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("haunted_bog", (2.30, 2.0, -1.0, 2.0, 0.06)),
	("dwarven_hold", (1.70, 2.0, -2.0, 8.0, 0.00)),
	("coral_atoll", (1.05, -1.0, 1.0, 7.0, 0.00)),
	("ashlands", (1.40, 1.0, -1.0, 1.5, 0.05)),
	("sky_temple", (1.20, 0.0, 0.0, 5.0, 0.00)),
	("glass_desert", (1.80, 0.0, 0.0, 1.0, 0.04)),
)
VANILLA_CB: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("conquest", (1.00, 10.0, 1.00, 1.00, 0.00)),
	("reconquest", (0.25, 5.0, 1.00, 0.80, 0.00)),
	("holy_war", (0.50, 10.0, 1.00, 1.00, 0.10)),
	("imperialism", (0.75, 15.0, 1.25, 0.75, 0.00)),
	("superiority", (0.00, 10.0, 0.00, 1.00, 0.40)),
	("tribal_feud", (0.50, 5.0, 0.50, 1.00, 0.00)),
	("nationalist", (0.50, 5.0, 1.00, 1.00, 0.00)),
	("colonialism", (0.25, 5.0, 1.00, 0.75, 0.00)),
)
# Reverse the vanilla CB ranking: imperialism (best) becomes the expensive
# war, tribal feud (worst) becomes the cheap one, and conquest is rewritten.
MODIFIED_CB: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("conquest", (0.25, 12.0, 1.40, 0.55, 0.25)),
	("reconquest", (1.40, 4.0, 0.45, 1.40, 0.05)),
	("holy_war", (2.00, 2.0, 0.25, 1.70, 0.00)),
	("imperialism", (2.40, 1.0, 0.15, 1.90, 0.00)),
	("superiority", (0.45, 8.0, 1.00, 0.85, 0.15)),
	("tribal_feud", (0.10, 16.0, 1.90, 0.40, 0.45)),
	("nationalist", (0.20, 14.0, 1.60, 0.50, 0.30)),
	("colonialism", (1.70, 3.0, 0.30, 1.55, 0.00)),
)
UNSEEN_CB: tuple[tuple[str, tuple[float, float, float, float, float]], ...] = (
	("eldritch_crusade", (1.80, 20.0, 0.30, 1.50, 0.20)),
	("dwarven_reclamation", (0.20, 8.0, 1.80, 0.50, 0.15)),
	("nomad_razzia", (0.90, 3.0, 0.25, 1.20, 0.55)),
	("merchant_embargo", (0.15, 2.0, 0.20, 0.40, 0.70)),
	("succession_by_steel", (1.20, 12.0, 0.90, 1.10, 0.05)),
)
_TERRAIN_RESIDUAL = {
	"plains": 0.20,
	"farmlands": 0.16,
	"forest": -0.05,
	"woods": -0.04,
	"hills": -0.06,
	"highlands": -0.07,
	"mountains": -0.10,
	"marsh": -0.12,
	"jungle": -0.11,
	"desert": -0.08,
	"coastal_desert": -0.06,
	"drylands": -0.05,
	"steppe": 0.06,
	"savannah": 0.04,
	"glacier": -0.14,
}
_CB_RESIDUAL = {
	"conquest": 0.18,
	"reconquest": 0.06,
	"holy_war": 0.10,
	"imperialism": 0.14,
	"superiority": 0.08,
	"tribal_feud": -0.05,
	"nationalist": 0.04,
	"colonialism": 0.07,
}
DISTRIBUTIONS = (
	"vanilla_id",
	"vanilla_stat_modified",
	"overhaul_unseen_terrain",
	"overhaul_unseen_cb",
)
MODELS = ("A", "B", "C0", "C30", "C50")
EXPORTED_DROPOUT = 0.5


def _center(values: dict[str, float]) -> dict[str, float]:
	mean = sum(values.values()) / len(values)
	return {key: value - mean for key, value in values.items()}


TERRAIN_BIAS = _center(_TERRAIN_RESIDUAL)
CB_BIAS = _center(_CB_RESIDUAL)


def _dot(values: tuple[float, ...] | list[float], weights: tuple[float, ...]) -> float:
	return sum(value * weight for value, weight in zip(values, weights, strict=True))


def _normalize(raw: tuple[float, ...], divisor: tuple[float, ...]) -> tuple[float, ...]:
	return tuple(value / scale for value, scale in zip(raw, divisor, strict=True))


def terrain_term(raw: tuple[float, ...], name: str, *, residual: bool) -> float:
	"""Attacker value of one terrain. Edited and unseen rows skip the residual."""
	value = _dot(_normalize(raw, TERRAIN_DIV), TERRAIN_W)
	if residual:
		value += TERRAIN_BIAS[name]
	return value


def cb_term(raw: tuple[float, ...], name: str, *, residual: bool) -> float:
	"""Attacker value of one casus belli. Edited and unseen rows skip the residual."""
	value = _dot(_normalize(raw, CB_DIV), CB_W)
	if residual:
		value += CB_BIAS[name]
	return value


def decision_reward(
	terrain_raw: tuple[float, ...],
	terrain_name: str,
	cb_raw: tuple[float, ...],
	cb_name: str,
	*,
	terrain_residual: bool,
	cb_residual: bool,
) -> float:
	"""Add a residual only for the side whose id is still the unmodified vanilla row."""
	return terrain_term(terrain_raw, terrain_name, residual=terrain_residual) + cb_term(
		cb_raw, cb_name, residual=cb_residual
	)


@dataclass(frozen=True)
class Pool:
	names: tuple[str, ...]
	raw: tuple[tuple[float, ...], ...]
	modifiers: torch.Tensor

	def index(self, name: str) -> int:
		return self.names.index(name)

	def raw_of(self, name: str) -> tuple[float, ...]:
		return self.raw[self.index(name)]


def _pool(
	rows: tuple[tuple[str, tuple[float, ...]], ...],
	divisor: tuple[float, ...],
) -> Pool:
	names = tuple(name for name, _ in rows)
	raw = tuple(stats for _, stats in rows)
	modifiers = torch.tensor(
		[_normalize(stats, divisor) for stats in raw],
		dtype=torch.float32,
	)
	return Pool(names, raw, modifiers)


def _assert_separated(pool: Pool) -> None:
	for left in range(len(pool.names)):
		for right in range(left + 1, len(pool.names)):
			if torch.allclose(pool.modifiers[left], pool.modifiers[right], atol=1e-5, rtol=0.0):
				raise RuntimeError(f"{pool.names[left]} collides with {pool.names[right]}")


def load_pools() -> dict[str, Pool]:
	terrain = _pool(VANILLA_TERRAIN, TERRAIN_DIV)
	modified_terrain = _pool(MODIFIED_TERRAIN, TERRAIN_DIV)
	unseen_terrain = _pool(UNSEEN_TERRAIN, TERRAIN_DIV)
	cb = _pool(VANILLA_CB, CB_DIV)
	modified_cb = _pool(MODIFIED_CB, CB_DIV)
	unseen_cb = _pool(UNSEEN_CB, CB_DIV)
	if terrain.names != modified_terrain.names or cb.names != modified_cb.names:
		raise RuntimeError("overhaul rewrites must keep the vanilla names")
	for pool in (terrain, modified_terrain, unseen_terrain, cb, modified_cb, unseen_cb):
		_assert_separated(pool)
	for name in terrain.names:
		if torch.allclose(
			terrain.modifiers[terrain.index(name)],
			modified_terrain.modifiers[modified_terrain.index(name)],
			atol=1e-4,
			rtol=0.0,
		):
			raise RuntimeError(f"{name} was not rewritten")
	return {
		"terrain": terrain,
		"modified_terrain": modified_terrain,
		"unseen_terrain": unseen_terrain,
		"cb": cb,
		"modified_cb": modified_cb,
		"unseen_cb": unseen_cb,
	}


def preference_reversal(pools: dict[str, Pool] | None = None) -> dict[str, float]:
	"""Spearman of vanilla rewards against the rewritten rewards. Negative means reversed."""
	pools = load_pools() if pools is None else pools
	terrain_vanilla = [
		terrain_term(raw, name, residual=True)
		for name, raw in zip(pools["terrain"].names, pools["terrain"].raw, strict=True)
	]
	terrain_edited = [
		terrain_term(raw, name, residual=False)
		for name, raw in zip(
			pools["modified_terrain"].names,
			pools["modified_terrain"].raw,
			strict=True,
		)
	]
	cb_vanilla = [
		cb_term(raw, name, residual=True)
		for name, raw in zip(pools["cb"].names, pools["cb"].raw, strict=True)
	]
	cb_edited = [
		cb_term(raw, name, residual=False)
		for name, raw in zip(pools["modified_cb"].names, pools["modified_cb"].raw, strict=True)
	]
	return {
		"terrain": _spearman_lists(terrain_vanilla, terrain_edited),
		"cb": _spearman_lists(cb_vanilla, cb_edited),
	}


@dataclass(frozen=True)
class Rows:
	terrain_names: tuple[str, ...]
	terrain: torch.Tensor
	cb_names: tuple[str, ...]
	cb: torch.Tensor
	reward: torch.Tensor
	group: torch.Tensor


def _collate(
	items: list[tuple[str, torch.Tensor, str, torch.Tensor, float]],
	groups: int | None,
) -> Rows:
	group = (
		torch.zeros(len(items), dtype=torch.long)
		if groups is None
		else torch.arange(len(items), dtype=torch.long) // GROUP_SIZE
	)
	return Rows(
		terrain_names=tuple(item[0] for item in items),
		terrain=torch.stack([item[1] for item in items]),
		cb_names=tuple(item[2] for item in items),
		cb=torch.stack([item[3] for item in items]),
		reward=torch.tensor([item[4] for item in items], dtype=torch.float32),
		group=group,
	)


def _all_pairs(terrain: Pool, cb: Pool) -> Rows:
	items: list[tuple[str, torch.Tensor, str, torch.Tensor, float]] = []
	for t_index, t_name in enumerate(terrain.names):
		for c_index, c_name in enumerate(cb.names):
			reward = decision_reward(
				terrain.raw[t_index],
				t_name,
				cb.raw[c_index],
				c_name,
				terrain_residual=True,
				cb_residual=True,
			)
			items.append(
				(t_name, terrain.modifiers[t_index], c_name, cb.modifiers[c_index], reward)
			)
	return _collate(items, None)


def _pick(rng: random.Random, count: int, vary: bool) -> list[int]:
	if vary:
		return rng.sample(range(count), GROUP_SIZE)
	choice = rng.randrange(count)
	return [choice] * GROUP_SIZE


def _build_groups(
	rng: random.Random,
	terrain: Pool,
	cb: Pool,
	*,
	terrain_residual: bool,
	cb_residual: bool,
	vary_terrain: bool,
	vary_cb: bool,
) -> Rows:
	items: list[tuple[str, torch.Tensor, str, torch.Tensor, float]] = []
	guard = 0
	while len(items) < GROUPS * GROUP_SIZE:
		guard += 1
		if guard > 20000:
			raise RuntimeError("decision groups did not separate")
		terrain_index = _pick(rng, len(terrain.names), vary_terrain)
		cb_index = _pick(rng, len(cb.names), vary_cb)
		draft: list[tuple[int, int, float]] = []
		for t_index, c_index in zip(terrain_index, cb_index, strict=True):
			reward = decision_reward(
				terrain.raw[t_index],
				terrain.names[t_index],
				cb.raw[c_index],
				cb.names[c_index],
				terrain_residual=terrain_residual,
				cb_residual=cb_residual,
			)
			draft.append((t_index, c_index, reward))
		ordered = sorted((reward for _, _, reward in draft), reverse=True)
		if ordered[0] - ordered[1] < MARGIN:
			continue
		for t_index, c_index, reward in draft:
			items.append(
				(
					terrain.names[t_index],
					terrain.modifiers[t_index],
					cb.names[c_index],
					cb.modifiers[c_index],
					reward,
				)
			)
	return _collate(items, GROUPS)


class OneHotDecision:
	"""Least-squares id lookup. Unseen terrain -> plains, unseen CB -> conquest."""

	def __init__(self, terrain: Pool, cb: Pool, terrain_coef: torch.Tensor, cb_coef: torch.Tensor):
		self.terrain_names = terrain.names
		self.cb_names = cb.names
		self.terrain_coef = terrain_coef
		self.cb_coef = cb_coef
		self.plains = terrain.index("plains")
		self.conquest = cb.index("conquest")

	def predict(self, rows: Rows) -> torch.Tensor:
		terrain = [
			self.terrain_names.index(name) if name in self.terrain_names else self.plains
			for name in rows.terrain_names
		]
		cb = [
			self.cb_names.index(name) if name in self.cb_names else self.conquest
			for name in rows.cb_names
		]
		return self.terrain_coef[terrain] + self.cb_coef[cb]


def _fit_one_hot(rows: Rows, terrain: Pool, cb: Pool) -> OneHotDecision:
	design = torch.zeros(rows.reward.shape[0], len(terrain.names) + len(cb.names))
	for index, (t_name, c_name) in enumerate(zip(rows.terrain_names, rows.cb_names, strict=True)):
		design[index, terrain.index(t_name)] = 1.0
		design[index, len(terrain.names) + cb.index(c_name)] = 1.0
	coef = torch.linalg.lstsq(design, rows.reward.unsqueeze(1)).solution.squeeze(1)
	return OneHotDecision(terrain, cb, coef[: len(terrain.names)], coef[len(terrain.names) :])


class ResidualDecision(nn.Module):
	"""``mean(f_t + E_t) + mean(f_c + E_c)``. Mechanics-only passes ``<UNK>``."""

	def __init__(
		self,
		terrain: Pool,
		cb: Pool,
		dropout: float,
		*,
		mechanics_only: bool,
	) -> None:
		super().__init__()
		self.mechanics_only = mechanics_only
		self.terrain = ResidualCategory(
			CategoryTable.from_rows(terrain.names, terrain.modifiers),
			WIDTH,
			dropout,
			mechanics=TERRAIN_AXES,
		)
		self.cb = ResidualCategory(
			CategoryTable.from_rows(cb.names, cb.modifiers),
			WIDTH,
			dropout,
			mechanics=CB_AXES,
		)
		self.terrain_names = terrain.names
		self.cb_names = cb.names
		_open_mechanic_layers(self.terrain)
		_open_mechanic_layers(self.cb)

	def score_batch(
		self,
		terrain: torch.Tensor,
		terrain_ids: torch.Tensor,
		cb: torch.Tensor,
		cb_ids: torch.Tensor,
		generator: torch.Generator | None = None,
	) -> torch.Tensor:
		terrain_h = self.terrain(terrain, terrain_ids, generator=generator)
		cb_h = self.cb(cb, cb_ids, generator=generator)
		return terrain_h.mean(dim=-1) + cb_h.mean(dim=-1)

	def predict(self, rows: Rows) -> torch.Tensor:
		self.eval()
		terrain_ids = _resolved_ids(self.terrain, rows.terrain_names, rows.terrain, self.mechanics_only)
		cb_ids = _resolved_ids(self.cb, rows.cb_names, rows.cb, self.mechanics_only)
		with torch.no_grad():
			return self.score_batch(rows.terrain, terrain_ids, rows.cb, cb_ids)


def _open_mechanic_layers(module: ResidualCategory) -> None:
	"""Start ``f`` at zero so an id lookup can absorb labels before ``f`` moves."""
	first = module.f[0]
	last = module.f[2]
	if not isinstance(first, nn.Linear) or not isinstance(last, nn.Linear):
		raise TypeError("residual f must stay Linear-ReLU-Linear")
	nn.init.xavier_uniform_(first.weight)
	nn.init.constant_(first.bias, 0.05)
	nn.init.zeros_(last.weight)
	nn.init.zeros_(last.bias)


def _resolved_ids(
	module: ResidualCategory,
	names: tuple[str, ...],
	modifiers: torch.Tensor,
	mechanics_only: bool,
) -> torch.Tensor:
	if mechanics_only:
		return torch.zeros(len(names), dtype=torch.long)
	return torch.tensor(
		[module.resolve(name, modifiers[index]) for index, name in enumerate(names)],
		dtype=torch.long,
	)


def _known_ids(names: tuple[str, ...], table: tuple[str, ...]) -> torch.Tensor:
	index = {name: position + 1 for position, name in enumerate(table)}
	return torch.tensor([index[name] for name in names], dtype=torch.long)


def _clamp_unknown(model: ResidualDecision) -> None:
	with torch.no_grad():
		model.terrain.embedding.weight[0].zero_()
		model.cb.embedding.weight[0].zero_()


def _dropout_loss(
	model: ResidualDecision,
	rows: Rows,
	terrain_ids: torch.Tensor,
	cb_ids: torch.Tensor,
	seed: int,
) -> float:
	"""Mean training loss over four fixed dropout masks. ``p = 0`` has no mask noise."""
	total = 0.0
	model.train()
	with torch.no_grad():
		for mask in range(4):
			predicted = model.score_batch(
				rows.terrain,
				terrain_ids,
				rows.cb,
				cb_ids,
				generator=torch.Generator().manual_seed(seed + mask),
			)
			total += float(torch.mean((predicted - rows.reward) ** 2))
	return total / 4


def _train_residual(
	model: ResidualDecision,
	rows: Rows,
	*,
	steps: int,
	warmup: int,
	seed: int,
) -> int:
	"""Train until the dropout loss is fit, or ``steps``. ``f`` stays frozen during warmup.

	``p = 0`` can reach the stop from embeddings alone, so its mechanic map never
	leaves the zero output it started with. ``p > 0`` cannot, and keeps training ``f``.
	"""
	f_params = [param for name, param in model.named_parameters() if "embedding" not in name]
	embed_params = [param for name, param in model.named_parameters() if "embedding" in name]
	optimizer = torch.optim.AdamW(
		[
			{"params": f_params, "lr": F_LR, "weight_decay": F_DECAY},
			{"params": embed_params, "lr": EMBED_LR, "weight_decay": 0.0},
		]
	)
	terrain_ids = _known_ids(rows.terrain_names, model.terrain_names)
	cb_ids = _known_ids(rows.cb_names, model.cb_names)
	if model.mechanics_only:
		terrain_ids = torch.zeros_like(terrain_ids)
		cb_ids = torch.zeros_like(cb_ids)
	generator = torch.Generator().manual_seed(seed)
	ran = steps
	for step in range(steps):
		if step >= warmup and _dropout_loss(model, rows, terrain_ids, cb_ids, seed + 9000) < STOP_MSE:
			ran = step
			break
		model.train()
		learn_mechanics = step >= warmup
		for param in f_params:
			param.requires_grad_(learn_mechanics)
		optimizer.param_groups[0]["lr"] = F_LR if learn_mechanics else 0.0
		predicted = model.score_batch(
			rows.terrain,
			terrain_ids,
			rows.cb,
			cb_ids,
			generator=generator,
		)
		loss = torch.mean((predicted - rows.reward) ** 2)
		optimizer.zero_grad(set_to_none=True)
		loss.backward()
		optimizer.step()
		_clamp_unknown(model)
	model.eval()
	_clamp_unknown(model)
	for param in f_params:
		param.requires_grad_(True)
	return ran


def _spearman_lists(left: list[float], right: list[float]) -> float:
	return _spearman(torch.tensor(left, dtype=torch.float32), torch.tensor(right, dtype=torch.float32))


def _spearman(predicted: torch.Tensor, truth: torch.Tensor) -> float:
	if predicted.numel() < 2:
		return 1.0
	pred_rank = predicted.argsort().argsort().to(dtype=torch.float32)
	true_rank = truth.argsort().argsort().to(dtype=torch.float32)
	pred_rank = pred_rank - pred_rank.mean()
	true_rank = true_rank - true_rank.mean()
	if float(pred_rank.norm()) == 0.0:
		return 0.0
	if float(true_rank.norm()) == 0.0:
		return 1.0
	return float((pred_rank * true_rank).sum() / (pred_rank.norm() * true_rank.norm()))


@dataclass(frozen=True)
class Metric:
	mse: float
	top1: float
	spearman: float


def _metric(predicted: torch.Tensor, rows: Rows) -> Metric:
	mse = float(torch.mean((predicted - rows.reward) ** 2))
	top1_hits = 0
	top1_total = 0
	spearman_sum = 0.0
	for group in rows.group.unique().tolist():
		mask = rows.group == group
		top1_hits += int(int(predicted[mask].argmax()) == int(rows.reward[mask].argmax()))
		top1_total += 1
		spearman_sum += _spearman(predicted[mask], rows.reward[mask])
	return Metric(mse, top1_hits / top1_total, spearman_sum / top1_total)


def _l2(model: ResidualDecision, kind: str) -> float:
	total = 0.0
	for name, param in model.named_parameters():
		selected = "embedding" in name if kind == "embed" else "embedding" not in name
		if selected:
			total += float(param.detach().square().sum())
	return total**0.5


def _f32(value: float) -> float:
	return struct.unpack("f", struct.pack("f", float(value)))[0]


def _naive_score(document: dict[str, object], name: str, modifiers: list[float]) -> list[float]:
	"""Rust ``ResidualCategory::score`` in IEEE-754 f32, multiply then add."""
	names = document["names"]
	trained = document["modifiers"]
	if not isinstance(names, list) or not isinstance(trained, list):
		raise TypeError("exported document is missing category rows")
	width = len(document["f0_bias"])  # type: ignore[arg-type]
	identity = 0
	if name in names:
		row = trained[names.index(name)]
		if not isinstance(row, list):
			raise TypeError("modifier row must be a list")
		matches = True
		for left, right in zip(row, modifiers, strict=True):
			scale = max(abs(float(left)), abs(float(right)))
			if abs(float(left) - float(right)) > 1.0e-5 + 1.0e-5 * scale:
				matches = False
				break
		if matches:
			identity = names.index(name) + 1
	f0_weight = document["f0_weight"]
	f0_bias = document["f0_bias"]
	hidden: list[float] = []
	for row_index in range(width):
		acc = _f32(float(f0_bias[row_index]))  # type: ignore[index]
		weight_row = f0_weight[row_index]  # type: ignore[index]
		for col, modifier in enumerate(modifiers):
			acc = _f32(acc + _f32(_f32(float(weight_row[col])) * _f32(modifier)))
		hidden.append(_f32(max(acc, 0.0)))
	f2_weight = document["f2_weight"]
	f2_bias = document["f2_bias"]
	out: list[float] = []
	for row_index in range(width):
		acc = _f32(float(f2_bias[row_index]))  # type: ignore[index]
		weight_row = f2_weight[row_index]  # type: ignore[index]
		for col, hidden_value in enumerate(hidden):
			acc = _f32(acc + _f32(_f32(float(weight_row[col])) * _f32(hidden_value)))
		out.append(acc)
	if identity != 0:
		embedding = document["embedding"]
		row = embedding[identity]  # type: ignore[index]
		out = [_f32(value + _f32(float(extra))) for value, extra in zip(out, row, strict=True)]
	return out


def _torch_score(module: ResidualCategory, name: str, modifiers: torch.Tensor) -> list[float]:
	module.eval()
	identity = torch.tensor([module.resolve(name, modifiers)], dtype=torch.long)
	with torch.no_grad():
		return module(modifiers.unsqueeze(0), identity).squeeze(0).tolist()


def _support_cases(
	distribution: str,
	head: str,
	pool: Pool,
	module: ResidualCategory,
) -> list[dict[str, object]]:
	cases = []
	for index, name in enumerate(pool.names):
		modifiers = pool.modifiers[index]
		cases.append(
			{
				"distribution": distribution,
				"head": head,
				"name": name,
				"modifiers": modifiers.tolist(),
				"score": _torch_score(module, name, modifiers),
			}
		)
	return cases


def fixture_path() -> Path:
	root = Path(__file__).resolve().parents[4]
	return (
		root
		/ "src"
		/ "packages"
		/ "foch-eu4-decide"
		/ "tests"
		/ "fixtures"
		/ "residual_ablation.json"
	)


@dataclass(frozen=True)
class AblationReport:
	metrics: dict[str, dict[str, Metric]]
	train_mse: dict[str, float]
	f_l2: dict[str, float]
	embed_l2: dict[str, float]
	steps_run: dict[str, int]
	reversal: dict[str, float]
	fixture: dict[str, object]
	reference_error: float

	def table(self) -> str:
		header = f"{'distribution':<28} {'model':<6} {'MSE':>10} {'Top1':>8} {'Spearman':>10}"
		lines = [header, "-" * len(header)]
		for distribution in DISTRIBUTIONS:
			for model in MODELS:
				metric = self.metrics[distribution][model]
				lines.append(
					f"{distribution:<28} {model:<6} {metric.mse:10.4f} "
					f"{metric.top1:8.4f} {metric.spearman:10.4f}"
				)
		return "\n".join(lines)


def _predict(model: OneHotDecision | ResidualDecision, rows: Rows) -> torch.Tensor:
	return model.predict(rows)


def run_ablation(seed: int = SEED) -> AblationReport:
	"""Train A/B/C on vanilla ids and score the four held-out distributions."""
	torch.set_num_threads(1)
	pools = load_pools()
	reversal = preference_reversal(pools)
	train = _all_pairs(pools["terrain"], pools["cb"])
	evals = {
		"vanilla_id": _build_groups(
			random.Random(seed + 1),
			pools["terrain"],
			pools["cb"],
			terrain_residual=True,
			cb_residual=True,
			vary_terrain=True,
			vary_cb=True,
		),
		"vanilla_stat_modified": _build_groups(
			random.Random(seed + 2),
			pools["modified_terrain"],
			pools["modified_cb"],
			terrain_residual=False,
			cb_residual=False,
			vary_terrain=True,
			vary_cb=True,
		),
		"overhaul_unseen_terrain": _build_groups(
			random.Random(seed + 3),
			pools["unseen_terrain"],
			pools["cb"],
			terrain_residual=False,
			cb_residual=True,
			vary_terrain=True,
			vary_cb=False,
		),
		"overhaul_unseen_cb": _build_groups(
			random.Random(seed + 4),
			pools["terrain"],
			pools["unseen_cb"],
			terrain_residual=True,
			cb_residual=False,
			vary_terrain=False,
			vary_cb=True,
		),
	}
	torch.manual_seed(seed)
	baseline = _fit_one_hot(train, pools["terrain"], pools["cb"])
	torch.manual_seed(seed + 10)
	mechanics = ResidualDecision(pools["terrain"], pools["cb"], 0.0, mechanics_only=True)
	steps_run = {
		"B": _train_residual(mechanics, train, steps=STEPS, warmup=0, seed=seed + 11)
	}
	dropout_models: dict[str, ResidualDecision] = {}
	for label, dropout in (("C0", 0.0), ("C30", 0.3), ("C50", 0.5)):
		torch.manual_seed(seed + 20)
		model = ResidualDecision(pools["terrain"], pools["cb"], dropout, mechanics_only=False)
		steps_run[label] = _train_residual(
			model, train, steps=STEPS, warmup=WARMUP, seed=seed + 21
		)
		dropout_models[label] = model
	models: dict[str, OneHotDecision | ResidualDecision] = {
		"A": baseline,
		"B": mechanics,
		**dropout_models,
	}
	metrics = {
		distribution: {label: _metric(_predict(model, rows), rows) for label, model in models.items()}
		for distribution, rows in evals.items()
	}
	train_mse = {
		label: float(torch.mean((_predict(model, train) - train.reward) ** 2))
		for label, model in models.items()
	}
	f_l2 = {label: _l2(model, "f") for label, model in models.items() if label != "A"}
	embed_l2 = {label: _l2(model, "embed") for label, model in models.items() if label != "A"}
	if embed_l2["B"] > 1e-6:
		raise RuntimeError("pure mechanics leaked an id embedding")
	exported = dropout_models["C50"]
	_clamp_unknown(exported)
	terrain_doc = exported.terrain.export_document()
	cb_doc = exported.cb.export_document()
	supports = {
		"vanilla_id": (("terrain", pools["terrain"]), ("cb", pools["cb"])),
		"vanilla_stat_modified": (
			("terrain", pools["modified_terrain"]),
			("cb", pools["modified_cb"]),
		),
		"overhaul_unseen_terrain": (("terrain", pools["unseen_terrain"]), ("cb", pools["cb"])),
		"overhaul_unseen_cb": (("terrain", pools["terrain"]), ("cb", pools["unseen_cb"])),
	}
	heads = {"terrain": exported.terrain, "cb": exported.cb}
	cases: list[dict[str, object]] = []
	for distribution, sides in supports.items():
		for head, pool in sides:
			cases.extend(_support_cases(distribution, head, pool, heads[head]))
	fixture: dict[str, object] = {
		"seed": seed,
		"dropout": EXPORTED_DROPOUT,
		"width": WIDTH,
		"terrain": terrain_doc,
		"cb": cb_doc,
		"cases": cases,
	}
	restored = json.loads(json.dumps(fixture))
	reference_error = 0.0
	for case, restored_case in zip(cases, restored["cases"], strict=True):
		head = str(case["head"])
		document = restored["terrain"] if head == "terrain" else restored["cb"]
		naive = _naive_score(document, str(case["name"]), list(restored_case["modifiers"]))
		for left, right in zip(naive, case["score"], strict=True):  # type: ignore[arg-type]
			reference_error = max(reference_error, abs(left - float(right)))
	return AblationReport(
		metrics=metrics,
		train_mse=train_mse,
		f_l2=f_l2,
		embed_l2=embed_l2,
		steps_run=steps_run,
		reversal=reversal,
		fixture=fixture,
		reference_error=reference_error,
	)


def main() -> None:
	report = run_ablation()
	print(report.table())
	print("reversal", {key: round(value, 4) for key, value in report.reversal.items()})
	print("train_mse", {key: round(value, 6) for key, value in report.train_mse.items()})
	print("f_l2", {key: round(value, 4) for key, value in report.f_l2.items()})
	print("embed_l2", {key: round(value, 4) for key, value in report.embed_l2.items()})
	print("steps", report.steps_run)
	print(f"reference_max_abs {report.reference_error:.3e}")
	path = fixture_path()
	path.parent.mkdir(parents=True, exist_ok=True)
	path.write_text(json.dumps(report.fixture, indent=2) + "\n", encoding="utf-8")
	print(f"wrote {path}")


if __name__ == "__main__":
	main()
