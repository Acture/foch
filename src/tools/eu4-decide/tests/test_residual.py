"""Residual category features: unseen and edited terrain still score."""

import unittest

import torch

from eu4_decide._residual import (
	DROPOUT_MAX,
	DROPOUT_MIN,
	UNK,
	CategoryTable,
	ResidualCategory,
)

VANILLA = {
	"plains": {"movement_cost": 1.0, "supply_limit": 4.0},
	"marsh": {"movement_cost": 3.0, "defence": 1.0, "attrition": 2.0},
}


class ResidualCategoryTest(unittest.TestCase):
	def test_dropout_must_stay_inside_the_training_band(self) -> None:
		table = CategoryTable.build(VANILLA)
		ResidualCategory(table, width=4, dropout=DROPOUT_MIN)
		ResidualCategory(table, width=4, dropout=DROPOUT_MAX)
		with self.assertRaises(ValueError):
			ResidualCategory(table, width=4, dropout=0.2)
		with self.assertRaises(ValueError):
			ResidualCategory(table, width=4, dropout=0.6)

	def test_unknown_and_edited_categories_use_the_zero_embedding(self) -> None:
		module = ResidualCategory(CategoryTable.build(VANILLA), width=4, dropout=0.4)
		with torch.no_grad():
			module.embedding.weight[1:].fill_(1.0)
			module.f[0].weight.copy_(torch.eye(4, len(module.f[0].weight[0])))
			module.f[0].bias.zero_()
			module.f[2].weight.copy_(torch.eye(4))
			module.f[2].bias.zero_()
		unseen = {"movement_cost": 2.5, "defence": 4.0, "attrition": 3.0}
		edited = {"movement_cost": 9.0, "supply_limit": 4.0}
		self.assertEqual(module.resolve("haunted_bog", _vec(unseen)), UNK)
		self.assertEqual(module.resolve("plains", _vec(edited)), UNK)
		self.assertEqual(module.resolve("plains", _vec(VANILLA["plains"])), 1)
		bog = module.score("haunted_bog", unseen)
		other = module.score(
			"glass_desert", {"movement_cost": 1.2, "combat_width": -2.0}
		)
		self.assertTrue(torch.isfinite(bog).all())
		self.assertFalse(torch.allclose(bog, other))
		# The residual of an unseen id is zero, so the score is exactly f.
		modifiers = _vec(unseen).unsqueeze(0)
		with torch.no_grad():
			only_f = module.f(modifiers).squeeze(0)
		self.assertTrue(torch.allclose(bog, only_f))

	def test_training_drops_ids_and_keeps_modifiers(self) -> None:
		module = ResidualCategory(CategoryTable.build(VANILLA), width=4, dropout=0.40)
		with torch.no_grad():
			for layer in module.f:
				if isinstance(layer, torch.nn.Linear):
					layer.weight.zero_()
					layer.bias.zero_()
			module.embedding.weight[1].fill_(1.0)
		modifiers = _vec(VANILLA["plains"]).unsqueeze(0).expand(2000, -1).contiguous()
		ids = torch.ones(2000, dtype=torch.long)
		generator = torch.Generator().manual_seed(0)
		module.train()
		scored = module(modifiers, ids, generator=generator)
		dropped = scored.abs().sum(dim=-1) == 0
		rate = float(dropped.float().mean())
		self.assertGreater(rate, 0.30)
		self.assertLess(rate, 0.50)
		kept = scored[~dropped]
		self.assertTrue(torch.allclose(kept, torch.ones_like(kept)))


def _vec(values: dict[str, float]) -> torch.Tensor:
	from eu4_decide._residual import mechanic_vector

	return mechanic_vector(values)


if __name__ == "__main__":
	unittest.main()
