"""Category features as ``f(modifiers) + E_id``.

Index 0 is ``<UNK>`` and its embedding row is the zero vector. Training
replaces the real id with 0 on each example with probability ``p`` in
``[0.30, 0.50]``; the mechanic vector is never dropped. ``p == 0`` is
the ablation control that disables id dropout, not a product setting.
At inference an id the table does not contain, and a known id whose
mechanics no longer match the values it was registered with, both
resolve to 0. A mod can therefore change a terrain's numbers or invent
a category and still score through ``f``.
"""

from __future__ import annotations

from dataclasses import dataclass

import torch
from torch import nn

MECHANICS = (
	"movement_cost",
	"defence",
	"combat_width",
	"supply_limit",
	"attrition",
)
UNK = 0
DROPOUT_MIN = 0.30
DROPOUT_MAX = 0.50


def mechanic_vector(values: dict[str, float]) -> torch.Tensor:
	"""Five mechanic axes in ``MECHANICS`` order. Missing keys are neutral."""
	neutral = {"movement_cost": 1.0, "supply_limit": 1.0}
	return torch.tensor(
		[float(values.get(name, neutral.get(name, 0.0))) for name in MECHANICS],
		dtype=torch.float32,
	)


@dataclass(frozen=True)
class CategoryTable:
	"""Names registered for residual lookup, in id order starting at 1."""

	names: tuple[str, ...]
	modifiers: torch.Tensor

	@staticmethod
	def build(entries: dict[str, dict[str, float]]) -> CategoryTable:
		names = tuple(entries)
		if not names:
			return CategoryTable((), torch.zeros(0, len(MECHANICS)))
		rows = torch.stack([mechanic_vector(entries[name]) for name in names])
		return CategoryTable(names, rows)

	@staticmethod
	def from_rows(names: tuple[str, ...] | list[str], modifiers: torch.Tensor) -> CategoryTable:
		"""Register pre-packed mechanic rows. Callers own the axis order."""
		labels = tuple(names)
		rows = modifiers.detach().to(dtype=torch.float32)
		if tuple(rows.shape) != (len(labels), len(MECHANICS)):
			raise ValueError(f"modifiers must be [{len(labels)}, {len(MECHANICS)}]")
		return CategoryTable(labels, rows.contiguous().clone())

	def id_of(self, name: str, modifiers: torch.Tensor) -> int:
		"""Resolve a category. Unknown names and edited numbers are ``<UNK>``."""
		try:
			index = self.names.index(name)
		except ValueError:
			return UNK
		trained = self.modifiers[index]
		if trained.shape != modifiers.shape or not torch.allclose(trained, modifiers):
			return UNK
		return index + 1


class ResidualCategory(nn.Module):
	"""``f(modifiers) + E_id`` with frozen ``<UNK> = 0``.

	Product training uses ``dropout`` in ``[DROPOUT_MIN, DROPOUT_MAX]``.
	Exactly ``0`` keeps every id so an ablation can show the shortcut
	that appears when ``E_id`` is never dropped.
	"""

	def __init__(
		self,
		table: CategoryTable,
		width: int,
		dropout: float = 0.40,
		mechanics: tuple[str, ...] | None = None,
	) -> None:
		super().__init__()
		if dropout != 0.0 and not DROPOUT_MIN <= dropout <= DROPOUT_MAX:
			raise ValueError(
				f"id dropout must be 0 or in [{DROPOUT_MIN}, {DROPOUT_MAX}], got {dropout}"
			)
		if width < 1:
			raise ValueError("width must be positive")
		chosen = MECHANICS if mechanics is None else tuple(mechanics)
		if len(chosen) != len(MECHANICS):
			raise ValueError(f"expected {len(MECHANICS)} mechanics, got {len(chosen)}")
		self.dropout = dropout
		self.width = width
		self._mechanics = chosen
		mechanic_dim = len(MECHANICS)
		self.f = nn.Sequential(
			nn.Linear(mechanic_dim, width),
			nn.ReLU(),
			nn.Linear(width, width),
		)
		# Row 0 is <UNK> and stays zero. Registered categories occupy 1..N.
		self.embedding = nn.Embedding(len(table.names) + 1, width)
		with torch.no_grad():
			self.embedding.weight.zero_()
		self.embedding.weight.register_hook(self._keep_unk_zero)
		self.register_buffer("trained", table.modifiers.clone(), persistent=True)
		self._names = table.names

	@staticmethod
	def _keep_unk_zero(gradient: torch.Tensor) -> torch.Tensor:
		gradient = gradient.clone()
		gradient[UNK].zero_()
		return gradient

	def resolve(self, name: str, modifiers: torch.Tensor) -> int:
		"""Same rule as :meth:`CategoryTable.id_of` against the registered rows."""
		try:
			index = self._names.index(name)
		except ValueError:
			return UNK
		trained = self.trained[index]
		flat = modifiers.detach().to(dtype=trained.dtype).reshape(-1)
		if flat.shape != trained.shape or not torch.allclose(trained, flat):
			return UNK
		return index + 1

	def forward(
		self,
		modifiers: torch.Tensor,
		ids: torch.Tensor,
		*,
		generator: torch.Generator | None = None,
	) -> torch.Tensor:
		"""Score a batch. Training applies id dropout; inference does not."""
		if modifiers.ndim != 2 or modifiers.shape[1] != len(MECHANICS):
			raise ValueError(f"modifiers must be [batch, {len(MECHANICS)}]")
		if ids.shape != modifiers.shape[:1]:
			raise ValueError("ids must be one index per modifier row")
		used = ids
		if self.training:
			drop = (
				torch.rand(ids.shape, generator=generator, device=ids.device)
				< self.dropout
			)
			used = ids.masked_fill(drop, UNK)
		residual = self.embedding(used)
		residual = residual.clone()
		residual[used == UNK] = 0
		return self.f(modifiers) + residual

	def export_document(self) -> dict[str, object]:
		"""JSON document the Rust `ResidualCategory::from_json` loads."""
		self.eval()
		state = self.state_dict()
		return {
			"kind": "residual-category",
			"mechanics": list(self._mechanics),
			"names": list(self._names),
			"modifiers": self.trained.detach().tolist(),
			"f0_weight": state["f.0.weight"].detach().tolist(),
			"f0_bias": state["f.0.bias"].detach().tolist(),
			"f2_weight": state["f.2.weight"].detach().tolist(),
			"f2_bias": state["f.2.bias"].detach().tolist(),
			"embedding": state["embedding.weight"].detach().tolist(),
		}

	def score(self, name: str, values: dict[str, float]) -> torch.Tensor:
		"""Inference score for one category. Unseen or edited values use ``E_id = 0``."""
		modifiers = mechanic_vector(values).unsqueeze(0)
		ids = torch.tensor([self.resolve(name, modifiers[0])], dtype=torch.long)
		training = self.training
		self.eval()
		try:
			return self.forward(modifiers, ids).squeeze(0)
		finally:
			self.train(training)
