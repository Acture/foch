"""A graph neural network policy over the province adjacency graph.

The environment reports each province as a node (terrain, development,
fort, armies, goal flag) and the adjacencies as edges. This policy runs
two rounds of graph attention - each node aggregates messages from its
neighbors, learning "what is nearby" the same way a general reads a map -
then scores each candidate as the dot product of its embedding with a
global query from the rules. The pointer head is unchanged; only the
spatial encoder changes, from a flattened CNN to a GNN that preserves
the graph's topology and scales to maps of any size.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

import torch
from torch import nn
import torch.nn.functional as F
from torch_geometric.nn import GATv2Conv

CLIP = 0.2
NODE = 13  # same channels as the board cells
RULES = 15
HIDDEN = 32


@dataclass
class GraphState:
	nodes: torch.Tensor  # (num_nodes, NODE)
	edge_index: torch.Tensor  # (2, num_edges) directed
	rules: torch.Tensor  # (RULES,)
	candidates: list[int]  # node indices


@dataclass
class Decision:
	state: GraphState
	action: int = 0
	log_probability: float = 0.0


@dataclass
class Trajectory:
	episode: int
	decisions: list[Decision] = field(default_factory=list)
	reward: float = 0.0


class ProvinceGNN(nn.Module):
	"""Two rounds of graph attention plus a pointer head."""

	def __init__(self) -> None:
		super().__init__()
		self.gat1 = GATv2Conv(NODE, HIDDEN, heads=2, concat=False)
		self.gat2 = GATv2Conv(HIDDEN, HIDDEN, heads=2, concat=False)
		self.query = nn.Sequential(
			nn.Linear(RULES, HIDDEN),
			nn.ReLU(),
			nn.Linear(HIDDEN, HIDDEN),
		)

	def forward(self, state: GraphState) -> torch.Tensor:
		h = F.relu(self.gat1(state.nodes, state.edge_index))
		h = F.relu(self.gat2(h, state.edge_index))
		q = self.query(state.rules)
		picked = h[state.candidates]  # (num_candidates, HIDDEN)
		return picked @ q  # (num_candidates,)


def parse_state(row: dict) -> Decision:
	"""Build the graph from the protocol's board + adjacency."""
	board = row["board"]
	num_nodes = len(board["cells"])
	nodes = torch.zeros(num_nodes, NODE, dtype=torch.float32)
	for i, cell in enumerate(board["cells"]):
		nodes[i] = torch.tensor(cell[:NODE], dtype=torch.float32)
	# Build directed edge index (each undirected edge -> two directed)
	edges = board.get("adj", [])
	src, dst = [], []
	for a, b in edges:
		src.extend([a, b])
		dst.extend([b, a])
	edge_index = torch.tensor([src, dst], dtype=torch.long)
	rules = torch.tensor(row["rules"][:RULES], dtype=torch.float32)
	candidates = [int(c[2]) for c in row["candidates"]]
	return Decision(state=GraphState(nodes, edge_index, rules, candidates))


def choose(policy: nn.Module, decision: Decision, deterministic: bool):
	scores = policy(decision.state)
	probabilities = F.softmax(scores, dim=0)
	if deterministic:
		action = int(scores.argmax())
	else:
		action = int(torch.multinomial(probabilities, 1))
	return action, float(probabilities[action].log())


def run_batch(
	command: list[str],
	scenarios: int,
	rollouts: int,
	policy: nn.Module,
	deterministic: bool,
) -> list[Trajectory]:
	process = subprocess.Popen(
		command + ["--scenarios", str(scenarios), "--rollouts", str(rollouts)],
		stdin=subprocess.PIPE,
		stdout=subprocess.PIPE,
		stderr=subprocess.DEVNULL,
		text=True,
		encoding="utf-8",
	)
	current = Trajectory(episode=-1)
	batch: list[Trajectory] = []
	assert process.stdout is not None and process.stdin is not None
	for line in process.stdout:
		row = json.loads(line)
		if row.get("done"):
			current.reward = row["reward"]
			batch.append(current)
			current = Trajectory(episode=-1)
			continue
		if current.episode != row["episode"]:
			current = Trajectory(episode=row["episode"])
		decision = parse_state(row)
		action, log_prob = choose(policy, decision, deterministic)
		decision.action = action
		decision.log_probability = log_prob
		current.decisions.append(decision)
		process.stdin.write(json.dumps({"pick": action}) + "\n")
		process.stdin.flush()
	process.wait()
	return batch


def grpo_update(
	policy: nn.Module, batch: list[Trajectory], epochs: int, lr: float
) -> float:
	optimizer = torch.optim.Adam(policy.parameters(), lr=lr)
	groups: dict[int, list[Trajectory]] = {}
	for t in batch:
		groups.setdefault(t.episode, []).append(t)
	decisions_total = sum(len(t.decisions) for t in batch)
	loss_value = 0.0
	for _epoch in range(epochs):
		total = torch.tensor(0.0)
		for group in groups.values():
			rewards = torch.tensor([t.reward for t in group])
			advantage = (rewards - rewards.mean()) / rewards.std().clamp(min=1e-6)
			for trajectory, value in zip(group, advantage):
				for decision in trajectory.decisions:
					scores = policy(decision.state)
					probabilities = F.softmax(scores, dim=0)
					old = math.exp(decision.log_probability)
					ratio = probabilities[decision.action] / max(old, 1e-8)
					clipped = ratio.clamp(1.0 - CLIP, 1.0 + CLIP)
					total = total - torch.minimum(
						ratio * value, clipped * value
					)
		total = total / decisions_total
		optimizer.zero_grad()
		total.backward()
		nn.utils.clip_grad_norm_(policy.parameters(), 0.5)
		optimizer.step()
		loss_value = total.item()
	return loss_value


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument(
		"--env", nargs="+", required=True, help="environment server command"
	)
	parser.add_argument("--scenarios", type=int, default=24)
	parser.add_argument("--rollouts", type=int, default=8)
	parser.add_argument("--rounds", type=int, default=20)
	parser.add_argument("--epochs", type=int, default=1)
	parser.add_argument("--learning-rate", type=float, default=3e-3)
	parser.add_argument("--weights", type=Path, default=Path("target/gnn.pt"))
	options = parser.parse_args()

	policy = ProvinceGNN()
	for round_index in range(options.rounds):
		batch = run_batch(options.env, options.scenarios, options.rollouts, policy, False)
		rewards = [t.reward for t in batch]
		print(
			f"round {round_index:>2}: mean {sum(rewards)/len(rewards):+.3f} "
			f"[{min(rewards):+.2f}..{max(rewards):+.2f}] "
			f"({len(batch)} tr, {sum(len(t.decisions) for t in batch)} dec)"
		)
		grpo_update(policy, batch, options.epochs, options.learning_rate)

	eval_batch = run_batch(options.env, max(8, options.scenarios // 3), options.rollouts, policy, True)
	print(f"greedy mean {sum(t.reward for t in eval_batch)/len(eval_batch):+.3f}")
	torch.save(policy.state_dict(), options.weights)
	print(f"wrote {options.weights}")


if __name__ == "__main__":
	main()
