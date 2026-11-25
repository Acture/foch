"""A board-CNN pointer policy over the raw pocket-war state.

The environment sends a padded board of raw cells - terrain, development,
fort, both armies, the goal - plus the rule numbers it runs on, and the
candidate provinces as board coordinates. This policy embeds the board with
a small conv net, embeds the rules with a small MLP, and scores each
candidate as the dot product of its cell embedding with the rule query: a
pointer head, with no hand-crafted strategic feature anywhere. Distance,
containment, defensive ground - whatever the policy needs, it learns.
"""

from __future__ import annotations

import json
import math
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

import torch
from torch import nn

CLIP = 0.2
CELL = 13  # x, y, terrain one-hot(5), development, capital, fort, ours, enemy, goal
RULES = 15


@dataclass
class Decision:
	board: torch.Tensor  # (H, W, CELL)
	rules: torch.Tensor  # (RULES,)
	candidates: list[tuple[int, int]]  # (x, y) per candidate
	action: int = 0
	log_probability: float = 0.0


@dataclass
class Trajectory:
	episode: int
	decisions: list[Decision] = field(default_factory=list)
	reward: float = 0.0


class PointerPolicy(nn.Module):
	"""Conv cell embeddings and a rule query; candidates score by dot."""

	def __init__(self, hidden: int = 32) -> None:
		super().__init__()
		self.cells = nn.Sequential(
			nn.Conv2d(CELL, hidden, 3, padding=1),
			nn.ReLU(),
			nn.Conv2d(hidden, hidden, 3, padding=1),
			nn.ReLU(),
		)
		self.query = nn.Sequential(nn.Linear(RULES, hidden), nn.ReLU(), nn.Linear(hidden, hidden))

	def scores(self, board: torch.Tensor, rules: torch.Tensor, candidates) -> torch.Tensor:
		features = self.cells(board.permute(2, 0, 1).unsqueeze(0)).squeeze(0)  # (hidden,H,W)
		query = self.query(rules)  # (hidden,)
		coords = torch.tensor(candidates, dtype=torch.long)
		picked = features[:, coords[:, 1], coords[:, 0]].T  # (n, hidden)
		return picked @ query


def choose(
	policy: nn.Module, decision: Decision, deterministic: bool
) -> tuple[int, float]:
	scores = policy.scores(decision.board, decision.rules, decision.candidates)
	probabilities = torch.softmax(scores, dim=0)
	if deterministic:
		action = int(scores.argmax())
	else:
		action = int(torch.multinomial(probabilities, 1))
	return action, float(probabilities[action].log())


def parse_state(row: dict) -> Decision:
	board = row["board"]
	height = board["h"]
	width = board["w"]
	grid = torch.zeros(height, width, CELL)
	for cell in board["cells"]:
		x, y = int(cell[0]), int(cell[1])
		if 0 <= y < height and 0 <= x < width:
			grid[y, x] = torch.tensor(cell, dtype=torch.float32)
	rules = torch.tensor(row["rules"], dtype=torch.float32)
	candidates = [(int(x), int(y)) for x, y, _ in row["candidates"]]
	return Decision(board=grid, rules=rules, candidates=candidates)


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
		bufsize=1,
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
		action, log_probability = choose(policy, decision, deterministic)
		decision.action = action
		decision.log_probability = log_probability
		current.decisions.append(decision)
		process.stdin.write(json.dumps({"pick": action}) + "\n")
		process.stdin.flush()
	process.wait()
	return batch


def update(policy: nn.Module, batch: list[Trajectory], epochs: int, lr: float) -> float:
	optimizer = torch.optim.Adam(policy.parameters(), lr=lr)
	groups: dict[int, list[Trajectory]] = {}
	for trajectory in batch:
		groups.setdefault(trajectory.episode, []).append(trajectory)
	loss_value = 0.0
	decisions_total = sum(len(t.decisions) for t in batch)
	for _epoch in range(epochs):
		total = torch.tensor(0.0)
		for group in groups.values():
			rewards = torch.tensor([t.reward for t in group])
			advantage = (rewards - rewards.mean()) / rewards.std().clamp(min=1e-6)
			for trajectory, value in zip(group, advantage):
				for decision in trajectory.decisions:
					scores = policy.scores(
						decision.board, decision.rules, decision.candidates
					)
					probabilities = torch.softmax(scores, dim=0)
					old = math.exp(decision.log_probability)
					ratio = probabilities[decision.action] / max(old, 1e-8)
					clipped = ratio.clamp(1.0 - CLIP, 1.0 + CLIP)
					total = total - torch.minimum(ratio * value, clipped * value)
		# Average per decision, not per trajectory: thousands of decision
		# terms at trajectory scale melt the policy.
		total = total / decisions_total
		optimizer.zero_grad()
		total.backward()
		nn.utils.clip_grad_norm_(policy.parameters(), 0.5)
		optimizer.step()
		loss_value = total.item()
	return loss_value


def main() -> None:
	import argparse

	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument(
		"--env",
		nargs="+",
		default=[
			"cargo", "run", "-q", "-p", "foch-eu4-decide", "--example", "env_server", "--",
		],
	)
	parser.add_argument("--scenarios", type=int, default=32)
	parser.add_argument("--rollouts", type=int, default=8)
	parser.add_argument("--rounds", type=int, default=12)
	parser.add_argument("--epochs", type=int, default=2)
	parser.add_argument("--learning-rate", type=float, default=1e-2)
	parser.add_argument("--weights", type=Path, default=Path("target/decide-weights.json"))
	parser.add_argument(
		"--init",
		type=Path,
		help="warm-start state dict, e.g. the behavior-cloned pointer",
	)
	options = parser.parse_args()
	policy = PointerPolicy()
	if options.init is not None:
		policy.load_state_dict(torch.load(options.init, weights_only=True))
		print(f"warm start from {options.init}")
	for round_index in range(options.rounds):
		batch = run_batch(options.env, options.scenarios, options.rollouts, policy, False)
		rewards = [t.reward for t in batch]
		print(
			f"round {round_index:>2}: mean reward {sum(rewards) / len(rewards):+.3f} "
			f"[min {min(rewards):+.2f} max {max(rewards):+.2f}] "
			f"({len(batch)} trajectories, {sum(len(t.decisions) for t in batch)} decisions)"
		)
		update(policy, batch, options.epochs, options.learning_rate)
	eval_batch = run_batch(options.env, options.scenarios // 4 or 8, options.rollouts, policy, True)
	print(f"greedy mean reward {sum(t.reward for t in eval_batch) / len(eval_batch):+.3f}")
	torch.save(policy.state_dict(), options.weights.with_suffix(".pt"))
	print(f"wrote {options.weights.with_suffix('.pt')}")
	export_json(policy, options.weights.with_suffix(".json"))


def export_json(policy: nn.Module, path: Path) -> None:
	# Plain-number weights for the Rust-side forward pass.
	layers = {
		"conv1_weight": policy.cells[0].weight.detach().tolist(),
		"conv1_bias": policy.cells[0].bias.detach().tolist(),
		"conv2_weight": policy.cells[2].weight.detach().tolist(),
		"conv2_bias": policy.cells[2].bias.detach().tolist(),
		"query1_weight": policy.query[0].weight.detach().tolist(),
		"query1_bias": policy.query[0].bias.detach().tolist(),
		"query2_weight": policy.query[2].weight.detach().tolist(),
		"query2_bias": policy.query[2].bias.detach().tolist(),
	}
	path.write_text(json.dumps(layers), encoding="utf-8")
	print(f"wrote {path}")

if __name__ == "__main__":
	main()
