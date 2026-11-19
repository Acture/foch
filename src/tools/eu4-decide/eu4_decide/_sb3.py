"""The pocket war as a Gymnasium environment, trained with MaskablePPO.

This replaces the hand-rolled clipped gradient with an off-the-shelf
algorithm: SB3's MaskablePPO carries the value baseline, GAE, entropy
bonus, and schedules our own update lacked. One gym step is one decision
point; the reward of a step is the change in the war's running score since
the previous decision - the game's own staged events, not a shaped proxy.
Observations are the raw board tensor and the rules vector; the action
space is fixed at the candidate cap, masked down to the live candidates.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path

import gymnasium as gym
import numpy as np
from gymnasium import spaces
from sb3_contrib import MaskablePPO

BOARD = 8
CELL = 13
RULES = 15
CANDIDATES = 8


class PocketWarEnv(gym.Env):
	metadata = {"render_modes": []}

	def __init__(self, env_command: list[str]) -> None:
		super().__init__()
		self.env_command = env_command
		self.process: subprocess.Popen | None = None
		self.reward_so_far = 0.0
		self.action_space = spaces.Discrete(CANDIDATES)
		self.observation_space = spaces.Dict(
			{
				"board": spaces.Box(0.0, 8.0, shape=(BOARD, BOARD, CELL), dtype=np.float32),
				"rules": spaces.Box(-2.0, 3.0, shape=(RULES,), dtype=np.float32),
			}
		)

	def _start(self) -> None:
		self.process = subprocess.Popen(
			self.env_command,
			stdin=subprocess.PIPE,
			stdout=subprocess.PIPE,
			stderr=subprocess.DEVNULL,
			text=True,
			encoding="utf-8",
		)
		self.reward_so_far = 0.0

	def _observe(self, row: dict) -> tuple[dict, np.ndarray]:
		board = np.zeros((BOARD, BOARD, CELL), dtype=np.float32)
		for cell in row["board"]["cells"]:
			x, y = int(cell[0]), int(cell[1])
			if x < BOARD and y < BOARD:
				board[y, x] = np.asarray(cell, dtype=np.float32)
		rules = np.asarray(row["rules"], dtype=np.float32)
		mask = np.zeros(CANDIDATES, dtype=bool)
		mask[: len(row["candidates"])] = True
		return {"board": board, "rules": rules}, mask

	def reset(self, *, seed=None, options=None):
		super().reset(seed=seed)
		if self.process is None or self.process.poll() is not None:
			self._start()
		assert self.process is not None and self.process.stdout is not None
		row = json.loads(self.process.stdout.readline())
		self.reward_so_far = row.get("reward", 0.0)
		observation, mask = self._observe(row)
		self._mask = mask
		self._candidates = len(row["candidates"])
		return observation, {"action_mask": mask}

	def action_masks(self) -> np.ndarray:
		return self._mask

	def step(self, action: int):
		assert self.process is not None and self.process.stdin is not None
		pick = min(int(action), self._candidates - 1)
		self.process.stdin.write(json.dumps({"pick": pick}) + "\n")
		self.process.stdin.flush()
		row = json.loads(self.process.stdout.readline())
		if row.get("done"):
			reward = row["reward"] - self.reward_so_far
			# The server runs further episodes and waits on stdin; waiting for
			# it to exit would deadlock. Kill it instead.
			self.process.kill()
			self.process.wait()
			self.process = None
			return (
				{"board": np.zeros((BOARD, BOARD, CELL), np.float32), "rules": np.zeros(RULES, np.float32)},
				reward,
				True,
				False,
				{},
			)
		observation, mask = self._observe(row)
		reward = row.get("reward", self.reward_so_far) - self.reward_so_far
		self.reward_so_far = row.get("reward", self.reward_so_far)
		self._mask = mask
		self._candidates = len(row["candidates"])
		return observation, reward, False, False, {"action_mask": mask}

	def close(self) -> None:
		if self.process is not None:
			self.process.kill()
			self.process = None


def evaluate(model: MaskablePPO, env_command: list[str], episodes: int) -> float:
	env = PocketWarEnv(env_command)
	rewards = []
	for _ in range(episodes):
		observation, _ = env.reset()
		done = False
		while not done:
			action, _ = model.predict(observation, action_masks=env.action_masks(), deterministic=True)
			observation, _, done, _, _ = env.step(int(action))
		rewards.append(env.reward_so_far)
	env.close()
	return sum(rewards) / len(rewards)


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument(
		"--env",
		nargs="+",
		default=[
			"cargo", "run", "-q", "-p", "foch-eu4-decide", "--example", "env_server", "--",
		],
	)
	parser.add_argument("--timesteps", type=int, default=120_000)
	parser.add_argument("--n-envs", type=int, default=4)
	parser.add_argument("--learning-rate", type=float, default=3e-4)
	parser.add_argument("--weights", type=Path, default=Path("target/decide-ppo"))
	options, _unknown = parser.parse_known_args()
	venv = PocketWarEnv(options.env + ["--scenarios", "1000000", "--rollouts", "1"])
	model = MaskablePPO(
		"MultiInputPolicy",
		venv,
		learning_rate=options.learning_rate,
		n_steps=256,
		batch_size=256,
		ent_coef=0.01,
		verbose=1,
	)
	model.learn(total_timesteps=options.timesteps, progress_bar=False)
	model.save(options.weights)
	print(f"saved {options.weights}")
	mean = evaluate(model, options.env + ["--scenarios", "16", "--rollouts", "4"], 64)
	print(f"greedy mean reward {mean:+.3f} over 64 rollouts")


if __name__ == "__main__":
	main()
