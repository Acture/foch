"""`python -m eu4_decide --episodes DIR` trains; `--llm` runs the zero-shot model."""

import argparse
import sys

from ._grpo import main as train_main
from ._llm import DEFAULT_MODEL, LlmPolicy, run_zero_shot


def main() -> None:
	parser = argparse.ArgumentParser(prog="eu4_decide")
	parser.add_argument("--llm", action="store_true", help="run the zero-shot LLM policy")
	parser.add_argument("--model", default=DEFAULT_MODEL)
	parser.add_argument("--env", nargs="+", help="the environment server command")
	parser.add_argument("--scenarios", type=int, default=24)
	parser.add_argument("--rollouts", type=int, default=8)
	options, rest = parser.parse_known_args()
	if options.llm:
		if options.env is None:
			print("--llm needs --env", file=sys.stderr)
			return
		env = options.env
		if isinstance(env, str):
			env = env.split()
		policy = LlmPolicy(options.model)
		run_zero_shot(env, options.scenarios, options.rollouts, policy)
		return
	sys.argv = [sys.argv[0], *rest] if rest else sys.argv
	train_main()


main()
