"""Trajectory GRPO over the env_server pocket war.

The Rust example `env_server` owns the world: random terrain maps, daily
time, command-cycle decision points, and a capped local candidate set. This
trainer owns the policy: one linear scorer shared by every candidate -
province features concatenated with the broadcast global features - so the
same weights score any map of any size. Each episode's rollouts share the
scenario seed, which makes them one GRPO group; the update is the clipped
policy gradient over every decision point of every trajectory.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

import torch
from torch import nn

CLIP = 0.2
PROVINCE_FEATURES = 12
GLOBAL_FEATURES = 16
SCORER_INPUT = PROVINCE_FEATURES + GLOBAL_FEATURES


@dataclass
class Decision:
	global_features: list[float]
	candidates: list[list[float]]
	action: int
	log_probability: float


@dataclass
class Trajectory:
	episode: int
	decisions: list[Decision] = field(default_factory=list)
	reward: float = 0.0


class CandidateScorer(nn.Module):
	"""One weight vector scoring `province features ++ global features`."""

	def __init__(self) -> None:
		super().__init__()
		self.weights = nn.Linear(SCORER_INPUT, 1, bias=False)
		nn.init.zeros_(self.weights.weight)

	def scores(
		self, global_features: torch.Tensor, candidates: torch.Tensor, lengths: torch.Tensor
	) -> list[torch.Tensor]:
		out: list[torch.Tensor] = []
		for index in range(candidates.shape[0]):
			count = int(lengths[index])
			feats = torch.cat(
				[
					candidates[index, :count],
					global_features[index].expand(count, GLOBAL_FEATURES),
				],
				dim=1,
			)
			out.append(self.weights(feats).squeeze(-1))
		return out


def rollout_policy(
	policy: nn.Module,
	decision: Decision,
	deterministic: bool,
) -> tuple[int, float]:
	features = torch.tensor(
		[
			list(candidate) + decision.global_features
			for candidate in decision.candidates
		],
		dtype=torch.float32,
	)
	scores = policy.weights(features).squeeze(-1)
	probabilities = torch.softmax(scores, dim=0)
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
		bufsize=1,
	)
	current = Trajectory(episode=-1)
	batch: list[Trajectory] = []
	assert process.stdout is not None
	for line in process.stdout:
		row = json.loads(line)
		if row.get("done"):
			current.reward = row["reward"]
			batch.append(current)
			current = Trajectory(episode=-1)
			continue
		decision = Decision(
			global_features=row["global"],
			candidates=row["candidates"],
			action=0,
			log_probability=0.0,
		)
		action, log_probability = rollout_policy(policy, decision, deterministic)
		decision.action = action
		decision.log_probability = log_probability
		if current.episode != row["episode"]:
			current = Trajectory(episode=row["episode"])
		current.decisions.append(decision)
		process.stdin.write(json.dumps({"pick": action}) + "\n")
		process.stdin.flush()
	process.wait()
	return batch


def update(policy: nn.Module, batch: list[Trajectory], epochs: int) -> float:
	optimizer = torch.optim.Adam(policy.parameters(), lr=3e-3)
	groups: dict[int, list[Trajectory]] = {}
	for trajectory in batch:
		groups.setdefault(trajectory.episode, []).append(trajectory)
	loss_value = 0.0
	for _epoch in range(epochs):
		total = torch.tensor(0.0)
		for group in groups.values():
			rewards = torch.tensor([t.reward for t in group])
			advantage = (rewards - rewards.mean()) / rewards.std().clamp(min=1e-6)
			for trajectory, value in zip(group, advantage):
				for decision in trajectory.decisions:
					features = torch.tensor(
						[
							list(candidate) + decision.global_features
							for candidate in decision.candidates
						],
						dtype=torch.float32,
					)
					scores = policy.weights(features).squeeze(-1)
					probabilities = torch.softmax(scores, dim=0)
					old_probability = math.exp(decision.log_probability)
					ratio = probabilities[decision.action] / max(old_probability, 1e-8)
					clipped = ratio.clamp(1.0 - CLIP, 1.0 + CLIP)
					total = total - torch.minimum(
						ratio * value, clipped * value
					) / len(batch)
		optimizer.zero_grad()
		total.backward()
		optimizer.step()
		loss_value = total.item()
	return loss_value


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument(
		"--env",
		nargs="+",
		default=["cargo", "run", "-q", "-p", "foch-eu4-decide", "--example", "env_server", "--"],
		help="the environment server command",
	)
	parser.add_argument("--scenarios", type=int, default=32)
	parser.add_argument("--rollouts", type=int, default=8)
	parser.add_argument("--rounds", type=int, default=12)
	parser.add_argument("--epochs", type=int, default=2)
	parser.add_argument("--weights", type=Path, default=Path("target/decide-weights.json"))
	options = parser.parse_args()
	policy = CandidateScorer()
	baseline = None
	for round_index in range(options.rounds):
		batch = run_batch(options.env, options.scenarios, options.rollouts, policy, False)
		rewards = [t.reward for t in batch]
		mean = sum(rewards) / len(rewards)
		baseline = mean if baseline is None else baseline
		histogram: dict[int, int] = {}
		for trajectory in batch:
			for decision in trajectory.decisions:
				histogram[decision.action] = histogram.get(decision.action, 0) + 1
		spread = f"min {min(rewards):+.2f} max {max(rewards):+.2f}"
		print(
			f"round {round_index:>2}: mean reward {mean:+.3f} [{spread}] "
			f"actions {dict(sorted(histogram.items()))}"
		)
		update(policy, batch, options.epochs)
	eval_batch = run_batch(options.env, max(8, options.scenarios // 4), options.rollouts, policy, True)
	print(f"greedy mean reward {sum(t.reward for t in eval_batch) / len(eval_batch):+.3f}")
	weights = policy.weights.weight.detach().squeeze(0).tolist()
	options.weights.parent.mkdir(parents=True, exist_ok=True)
	options.weights.write_text(json.dumps({"scorer": weights}), encoding="utf-8")
	print(f"wrote {options.weights}", file=sys.stderr)


if __name__ == "__main__":
	main()
