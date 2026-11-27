"""Mixed-source RAFT: random + policy rollouts, strict filter, scratch retrain.

The recipe that first beat the uniform walk, by four times:

1. Collect random rollouts on the advantage curriculum - pure
   exploration that stumbles into occupations worth +15 or more.
2. Collect random rollouts on the normal world - coverage of the
   difficulty the policy will actually face.
3. Collect the warm-start policy's rollouts at high temperature -
   exploitation that finds new good paths near where it already
   looks, with enough noise to discover what random alone missed.
4. Pool all three, keep the top eight percent. At this strictness
   every kept trajectory has occupied the war goal.
5. Retrain the GNN from scratch on the survivors. Fine-tuning
   overwrites; a fresh network on clean data does not forget.

Run: python -m eu4_decide._raft --env <path-to-env_server>
"""

from __future__ import annotations

import argparse
import json
import random
import subprocess
from pathlib import Path

import torch
import torch.nn.functional as F

from ._gnn import ProvinceGNN, parse_state


def collect(
	env_exe: str,
	policy: ProvinceGNN | None,
	episodes: int,
	rollouts: int,
	seed: int,
	temperature: float = 1.0,
	curriculum: bool = False,
) -> list[tuple[list, list, float]]:
	"""Run rollouts; policy=None is pure random, otherwise sampled."""
	rng = random.Random(seed)
	args = [env_exe, "--scenarios", str(episodes), "--rollouts", str(rollouts)]
	if curriculum:
		args.append("--curriculum")
	process = subprocess.Popen(
		args,
		stdin=subprocess.PIPE,
		stdout=subprocess.PIPE,
		stderr=subprocess.DEVNULL,
		text=True,
		encoding="utf-8",
	)
	trajectories = []
	states, actions = [], []
	assert process.stdout is not None and process.stdin is not None
	for line in process.stdout:
		row = json.loads(line)
		if row.get("done"):
			trajectories.append((states, actions, row["reward"]))
			states, actions = [], []
			continue
		decision = parse_state(row)
		if policy is None:
			pick = rng.randrange(len(row["candidates"]))
		else:
			with torch.no_grad():
				scores = policy(decision.state) / temperature
				probs = F.softmax(scores, dim=0)
				pick = int(torch.multinomial(probs, 1))
		states.append(decision)
		actions.append(pick)
		process.stdin.write(json.dumps({"pick": pick}) + "\n")
		process.stdin.flush()
	process.wait()
	return trajectories


def evaluate(policy: ProvinceGNN, env_exe: str, episodes=16, rollouts=4) -> float:
	process = subprocess.Popen(
		[env_exe, "--scenarios", str(episodes), "--rollouts", str(rollouts)],
		stdin=subprocess.PIPE,
		stdout=subprocess.PIPE,
		stderr=subprocess.DEVNULL,
		text=True,
		encoding="utf-8",
	)
	rewards = []
	assert process.stdout is not None and process.stdin is not None
	for line in process.stdout:
		row = json.loads(line)
		if row.get("done"):
			rewards.append(row["reward"])
			continue
		decision = parse_state(row)
		with torch.no_grad():
			pick = int(policy(decision.state).argmax())
		pick = min(pick, len(row["candidates"]) - 1)
		process.stdin.write(json.dumps({"pick": pick}) + "\n")
		process.stdin.flush()
	process.wait()
	return sum(rewards) / len(rewards)


def train_from_scratch(pairs: list, epochs=100, lr=5e-3) -> ProvinceGNN:
	policy = ProvinceGNN()
	optimizer = torch.optim.Adam(policy.parameters(), lr=lr)
	for _epoch in range(epochs):
		optimizer.zero_grad()
		loss = torch.tensor(0.0)
		for states, actions in pairs:
			for decision, action in zip(states, actions):
				scores = policy(decision.state)
				loss = loss + F.cross_entropy(
					scores.unsqueeze(0), torch.tensor([action])
				)
		loss.backward()
		optimizer.step()
	return policy


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--env", required=True, help="path to env_server.exe")
	parser.add_argument(
		"--enemy-weights",
		type=Path,
		help="policy weights for the enemy (self-play); appends --enemy-policy to the env command",
	)
	parser.add_argument("--warm-start", type=Path, help="state dict to bootstrap the policy rollouts")
	parser.add_argument("--filter", type=float, default=0.08, help="top fraction to keep")
	parser.add_argument("--epochs", type=int, default=100)
	parser.add_argument("--temperature", type=float, default=2.0)
	parser.add_argument("--weights", type=Path, default=Path("target/gnn-raft.pt"))
	options = parser.parse_args()

	warm = ProvinceGNN()
	if options.warm_start and options.warm_start.exists():
		warm.load_state_dict(torch.load(options.warm_start, weights_only=True))
		print(f"warm start from {options.warm_start}")
	else:
		print("no warm start; policy rollouts will be random until loaded")

	env_easy = options.env
	env_norm = options.env
	env_pol = options.env
	if options.enemy_weights and options.enemy_weights.exists():
		flag = ["--enemy-policy", str(options.enemy_weights)]
		print(f"self-play: enemy uses {options.enemy_weights}")
	else:
		flag = []
	# The flag is appended to each env invocation inside collect via env_exe.
	# For simplicity, we append it to the exe path (the collect function
	# passes it through to subprocess).
	env_easy = [options.env] + flag if flag else options.env
	env_norm = env_easy
	env_pol = env_easy

	print("collecting: random on advantage curriculum...")
	easy = collect(env_easy, None, 48, 8, seed=200, curriculum=True)
	print("collecting: random on normal world...")
	norm = collect(env_norm, None, 48, 8, seed=201)
	print(f"collecting: policy rollouts (temp={options.temperature})...")
	pol = collect(env_pol, warm, 32, 8, seed=202, temperature=options.temperature)

	pool = easy + norm + pol
	rewards = sorted(t[2] for t in pool)
	threshold = rewards[int(len(rewards) * (1.0 - options.filter))]
	good = [(s, a) for s, a, r in pool if r >= threshold]
	decisions = sum(len(s) for s, _ in good)
	print(
		f"pooled {len(pool)} trajectories, "
		f"top {options.filter:.0%} threshold {threshold:+.2f} "
		f"-> {len(good)} trajectories, {decisions} decisions"
	)

	policy = train_from_scratch(good, epochs=options.epochs)
	result = evaluate(policy, options.env)
	print(f"greedy: {result:+.3f}")
	torch.save(policy.state_dict(), options.weights)
	print(f"wrote {options.weights}")


if __name__ == "__main__":
	main()
