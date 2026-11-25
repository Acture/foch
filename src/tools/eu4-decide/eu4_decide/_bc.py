"""Behavior cloning of the lookahead oracle for the pointer policy.

Reads the oracle dataset - raw board, rules, candidates, and the slot the
lookahead rollouts paid most - and fits the pointer policy by
cross-entropy, then plays greedy through the environment server. This is
the warm start for expert iteration: if cloning cannot even fit one-step
lookahead, the bottleneck is the representation, not the optimizer.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import torch
from torch import nn

from ._board_policy import PointerPolicy, choose, parse_state, run_batch


def load_dataset(path: Path) -> list[dict]:
	rows = []
	for line in path.read_text(encoding="utf-8").splitlines():
		row = json.loads(line)
		rows.append(row)
	return rows


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--dataset", type=Path, default=Path("target/oracle.jsonl"))
	parser.add_argument("--epochs", type=int, default=60)
	parser.add_argument("--learning-rate", type=float, default=3e-3)
	parser.add_argument("--holdout", type=float, default=0.2)
	parser.add_argument(
		"--env",
		nargs="+",
		default=[
			"cargo", "run", "-q", "-p", "foch-eu4-decide", "--example", "env_server", "--",
		],
	)
	parser.add_argument("--scenarios", type=int, default=16)
	parser.add_argument("--rollouts", type=int, default=4)
	parser.add_argument("--weights", type=Path, default=Path("target/decide-bc.pt"))
	parser.add_argument(
		"--dagger",
		type=Path,
		help="state dict whose policy walks while the oracle labels (DAgger)",
	)
	options = parser.parse_args()

	policy = PointerPolicy()
	rows = load_dataset(options.dataset)
	if options.dagger is not None:
		# DAgger: the current policy walks, the oracle labels every state it
		# visits, and those on-distribution decisions join the dataset.
		import json as _json
		import subprocess as _subprocess

		policy.load_state_dict(torch.load(options.dagger, weights_only=True))
		command = options.env + ["--oracle-labels"]
		process = _subprocess.Popen(
			command,
			stdin=_subprocess.PIPE,
			stdout=_subprocess.PIPE,
			stderr=_subprocess.DEVNULL,
			text=True,
			encoding="utf-8",
		)
		walked = 0
		for line in process.stdout:
			row = _json.loads(line)
			if row.get("done"):
				continue
			decision = parse_state(row)
			action, _ = choose(policy, decision, False)
			walked += 1
			row["best"] = row["oracle"]
			rows.append(row)
			process.stdin.write(_json.dumps({"pick": action}) + "\n")
			process.stdin.flush()
		process.wait()
		print(f"dagger: labeled {walked} states from the policy's own walks")
	split = int(len(rows) * (1.0 - options.holdout))
	train, test = rows[:split], rows[split:]
	print(f"dataset: {len(train)} train, {len(test)} holdout decisions")

	optimizer = torch.optim.Adam(policy.parameters(), lr=options.learning_rate)
	states = [(parse_state(row), row["best"]) for row in train]
	test_states = [(parse_state(row), row["best"]) for row in test]
	for epoch in range(options.epochs):
		optimizer.zero_grad()
		loss = torch.tensor(0.0)
		correct = 0
		for decision, best in states:
			scores = policy.scores(decision.board, decision.rules, decision.candidates)
			loss = loss + nn.functional.cross_entropy(
				scores.unsqueeze(0), torch.tensor([best])
			)
			correct += int(scores.argmax() == best)
		loss.backward()
		optimizer.step()
		if epoch % 10 == 9 or epoch == options.epochs - 1:
			with torch.no_grad():
				test_correct = sum(
					int(policy.scores(d.board, d.rules, d.candidates).argmax() == best)
					for d, best in test_states
				)
			print(
				f"epoch {epoch:>3}: train {correct}/{len(states)} "
				f"({correct / len(states):.0%}), holdout {test_correct}/{len(test_states)} "
				f"({test_correct / max(len(test_states), 1):.0%})"
			)
	torch.save(policy.state_dict(), options.weights)
	print(f"wrote {options.weights}")
	from ._board_policy import export_json

	export_json(policy, options.weights.with_suffix(".json"))
	from ._board_policy import export_json

	export_json(policy, options.weights.with_suffix(".json"))

	eval_batch = run_batch(options.env, options.scenarios, options.rollouts, policy, True)
	rewards = [t.reward for t in eval_batch]
	print(
		f"greedy mean reward {sum(rewards) / len(rewards):+.3f} "
		f"over {len(rewards)} rollouts"
	)


if __name__ == "__main__":
	main()
