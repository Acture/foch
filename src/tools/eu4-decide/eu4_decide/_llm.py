"""A language-model policy over the text rendering of the pocket war.

The environment describes each decision point in plain language - armies,
tech, terrain, the war goal, the payscale - and lists the candidate
provinces as numbered orders. A small instruct model scores each option
number given that description; the highest mean log-probability wins. This
is the zero-shot world-knowledge baseline: nothing is trained, so whatever
it earns over the uniform walk is knowledge the model carried in.
"""

from __future__ import annotations

import json
import subprocess

DEFAULT_MODEL = "Qwen/Qwen2.5-0.5B-Instruct"

SYSTEM = (
	"You are a general in Europa Universalis IV. Choose the order that wins "
	"the war: weigh the war goal, the enemy army, terrain, and attrition."
)


class LlmPolicy:
	def __init__(self, model: str = DEFAULT_MODEL) -> None:
		from transformers import AutoModelForCausalLM, AutoTokenizer

		self.tokenizer = AutoTokenizer.from_pretrained(model)
		self.model = AutoModelForCausalLM.from_pretrained(model)
		self.model.eval()
		self.calls = 0

	def pick(self, description: str, options: int) -> int:
		prompt = description + "\nReply with only the number of the best order."
		messages = [
			{"role": "system", "content": SYSTEM},
			{"role": "user", "content": prompt},
		]
		chat = self.tokenizer.apply_chat_template(
			messages, tokenize=False, add_generation_prompt=True
		)
		import torch

		prefix = self.tokenizer(chat, return_tensors="pt")
		prefix_length = prefix["input_ids"].shape[1]
		scores = []
		for option in range(1, options + 1):
			digit = self.tokenizer(str(option), return_tensors="pt")["input_ids"]
			# tokenizer may prepend a space token; keep only the digit's ids
			digit_ids = digit[0][-(1 if str(option).isdigit() else len(digit[0])) :]
			input_ids = torch.cat(
				[prefix["input_ids"], digit_ids.unsqueeze(0)], dim=1
			)
			with torch.no_grad():
				logits = self.model(input_ids).logits
			# log-probability of the digit token at the last position
			log_probs = (
				torch.log_softmax(logits[0, prefix_length - 1 : prefix_length], dim=-1)
			)
			token_id = digit_ids[-1]
			scores.append(float(log_probs[0, token_id]))
			self.calls += 1
		return max(range(options), key=lambda index: scores[index])


def run_zero_shot(
	env_command: list[str], scenarios: int, rollouts: int, policy: LlmPolicy
) -> float:
	process = subprocess.Popen(
		env_command + ["--scenarios", str(scenarios), "--rollouts", str(rollouts)],
		stdin=subprocess.PIPE,
		stdout=subprocess.PIPE,
		stderr=subprocess.DEVNULL,
		text=True,
		encoding="utf-8",
		bufsize=1,
	)
	rewards: list[float] = []
	assert process.stdout is not None and process.stdin is not None
	for line in process.stdout:
		row = json.loads(line)
		if row.get("done"):
			rewards.append(row["reward"])
			continue
		pick = policy.pick(row["description"], len(row["candidates"]))
		process.stdin.write(json.dumps({"pick": pick}) + "\n")
		process.stdin.flush()
	process.wait()
	mean = sum(rewards) / len(rewards)
	print(
		f"llm zero-shot: mean reward {mean:+.3f} over {len(rewards)} rollouts "
		f"({policy.calls} scored decisions)"
	)
	return mean
