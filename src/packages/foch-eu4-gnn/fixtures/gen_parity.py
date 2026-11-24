"""Deterministic parity fixtures for the Rust GeneralGnn port.

Self-contained on purpose: defines the reference model inline (torch +
torch_geometric only) so fixture generation never imports the training
tooling. Two modes:

  gen_parity.py fixture OUT.json       # synthetic small model + graphs
  gen_parity.py export CKPT.pt OUT.json  # trained checkpoint -> flat JSON

The synthetic model is tiny (2 heads, 4 channels) but exercises the
same code paths as the real one: two GATv2 layers, the rules MLP, and
the pointer head. Graphs deliberately include an input self-loop so the
Rust side's remove-then-add self-loop semantics are covered.
"""
import json
import sys

import torch
import torch.nn.functional as F
from torch import nn
from torch_geometric.nn import GATv2Conv

NODE, RULES, HEADS, C = 29, 26, 2, 4


class ReferenceGnn(nn.Module):
	def __init__(self):
		super().__init__()
		self.gat1 = GATv2Conv(NODE, C, heads=HEADS, concat=False)
		self.gat2 = GATv2Conv(C, C, heads=HEADS, concat=False)
		self.query = nn.Sequential(nn.Linear(RULES, C), nn.ReLU(), nn.Linear(C, C))
		self.head = nn.Sequential(nn.Linear(C * 3, C), nn.ReLU(), nn.Linear(C, 1))

	def forward(self, nodes, ei, rules, cands):
		h = F.relu(self.gat1(nodes, ei))
		h = F.relu(self.gat2(h, ei))
		q = self.query(rules)
		c = h[torch.tensor(cands)]
		here = h[int(nodes[:, 8].argmax())].unsqueeze(0).expand(c.size(0), -1)
		qs = q.unsqueeze(0).expand(c.size(0), -1)
		return self.head(torch.cat([c, here, qs], dim=-1)).squeeze(-1)


def flat(state_dict):
	return {name: tensor.tolist() for name, tensor in state_dict.items()}


def fixture(out_path):
	torch.manual_seed(20261119)
	model = ReferenceGnn().eval()
	weights = flat(model.state_dict())

	graphs = [
		# (nodes, directed_edges, rules, cands) — one graph carries an
		# explicit self-loop on node 1 to pin remove+add semantics.
		(5, [(0, 1), (1, 0), (1, 2), (2, 1), (2, 3), (3, 2), (3, 4), (4, 3), (1, 1)], [0, 2, 4]),
		(6, [(0, 1), (1, 0), (1, 3), (3, 1), (2, 5), (5, 2), (3, 4), (4, 3)], [1, 3, 5, 0]),
		(7, [(i, i + 1) for i in range(6)] + [(i + 1, i) for i in range(6)], [2, 4, 6]),
	]
	states = []
	with torch.no_grad():
		for index, (n, edges, cands) in enumerate(graphs):
			nodes = torch.zeros(n, NODE)
			nodes[:, :5] = torch.arange(n).unsqueeze(1).float() / (n + 1.0)
			nodes[0, 8] = 1.0  # we stand on node 0 (col 8 argmax)
			nodes[:, 9] = (torch.arange(n).float() * 0.25) % 1.0
			ei = torch.tensor(edges, dtype=torch.long).t().contiguous()
			rules = torch.linspace(-1.0, 1.0, RULES)
			scores = model(nodes, ei, rules, cands)
			states.append({
				"nodes": [[float(v) for v in row] for row in nodes],
				"edges": [v for edge in edges for v in edge],
				"rules": [float(v) for v in rules],
				"cands": cands,
				"scores": [round(float(s), 9) for s in scores],
			})
			print(f"graph {index}: {n} nodes, {len(edges)} directed edges "
				  f"(incl. self-loop: {any(a == b for a, b in edges)})")
	with open(out_path, "w") as f:
		json.dump({"weights": weights, "states": states}, f)
	print(f"{out_path}: {len(states)} graphs, "
		  f"{sum(len(t) for t in weights.values())} parameters")


def export(ckpt_path, out_path):
	state = torch.load(ckpt_path, map_location="cpu")
	with open(out_path, "w") as f:
		json.dump(flat(state), f)
	print(f"{out_path}: {sum(len(t) for t in flat(state).values())} parameters")


if __name__ == "__main__":
	if len(sys.argv) == 4 and sys.argv[1] == "export":
		export(sys.argv[2], sys.argv[3])
	elif len(sys.argv) == 3 and sys.argv[1] == "fixture":
		fixture(sys.argv[2])
	else:
		raise SystemExit(__doc__)
