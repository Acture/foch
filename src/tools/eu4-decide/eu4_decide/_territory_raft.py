"""RAFT on the territorial war with EU4 scoring and the 5-year timer.

With --oracle, collection asks the env for oracle labels (--oracle-labels 8)
and training is plain behavior cloning on (state -> oracle argmax) for every
labeled decision; outcome filtering is skipped because the oracle IS the
filter. Without --oracle this is outcome-filtered RAFT as before.
"""
import json, subprocess as sp, random, torch, torch.nn.functional as F
from torch import nn
from torch_geometric.nn import GATv2Conv

NODE, RULES, HIDDEN = 29, 26, 64

class TerritoryGNN(nn.Module):
	def __init__(self):
		super().__init__()
		self.gat1 = GATv2Conv(NODE, HIDDEN, heads=2, concat=False)
		self.gat2 = GATv2Conv(HIDDEN, HIDDEN, heads=2, concat=False)
		self.query = nn.Sequential(nn.Linear(RULES, HIDDEN), nn.ReLU(), nn.Linear(HIDDEN, HIDDEN))
		self.head = nn.Sequential(
			nn.Linear(HIDDEN * 3, HIDDEN), nn.ReLU(), nn.Linear(HIDDEN, 1))
	def forward(self, nodes, ei, rules, cands):
		h = F.relu(self.gat1(nodes, ei))
		h = F.relu(self.gat2(h, ei))
		q = self.query(rules)
		# Score each candidate against where we stand and the rules —
		# a choice is relative, not absolute.
		c = h[torch.tensor(cands, device=h.device)]
		here = h[int(nodes[:, 8].argmax())].unsqueeze(0).expand(c.size(0), -1)
		qs = q.unsqueeze(0).expand(c.size(0), -1)
		return self.head(torch.cat([c, here, qs], dim=-1)).squeeze(-1)

	def forward_batch(self, states, device):
		"""Score every candidate of every example in one pass.

		`collate` packs the examples into a single disjoint-union graph
		(offset edge_index): GATv2 only mixes along edges, so the union
		visit computes exactly what per-example `forward` calls would.
		Returns one score tensor per example, in input order.
		"""
		nodes_cat, ei, rules_b, cand_idx, here_idx, group, counts = collate(states, device)
		h = F.relu(self.gat1(nodes_cat, ei))
		h = F.relu(self.gat2(h, ei))
		q = self.query(rules_b)
		c = h[cand_idx]
		here = h[here_idx][group]
		qs = q[group]
		scores = self.head(torch.cat([c, here, qs], dim=-1)).squeeze(-1)
		return list(scores.split(counts))

def collate(states, device):
	"""Pack parsed states into one disjoint-union minibatch on `device`.

	Each state is a parsed (nodes, ei, rules, cands) tuple. Node features
	concatenate to [sum(N), NODE], every graph's edge_index is offset by
	its node offset so components stay disconnected, rules stack to
	[B, RULES], and candidate/"here" indices flatten into the union
	graph. `group[i]` is the example owning flat candidate i.
	"""
	offsets, total = [], 0
	for n, _, _, _ in states:
		offsets.append(total)
		total += n.size(0)
	nodes_cat = torch.cat([n for n, _, _, _ in states], dim=0).to(device)
	ei = torch.cat([e + off for (_, e, _, _), off in zip(states, offsets)], dim=1).to(device)
	rules_b = torch.stack([ru for _, _, ru, _ in states]).to(device)
	cand_idx = torch.cat([torch.tensor(ca, dtype=torch.long) + off
		for (_, _, _, ca), off in zip(states, offsets)]).to(device)
	here_idx = torch.tensor([off + int(n[:, 8].argmax())
		for (n, _, _, _), off in zip(states, offsets)], dtype=torch.long, device=device)
	counts = [len(ca) for _, _, _, ca in states]
	group = torch.repeat_interleave(
		torch.arange(len(states), device=device), torch.tensor(counts, device=device))
	return nodes_cat, ei, rules_b, cand_idx, here_idx, group, counts

def parse(row):
	b = row["board"]
	n = len(b["cells"])
	nodes = torch.zeros(n, NODE, dtype=torch.float32)
	for i, c in enumerate(b["cells"]):
		nodes[i] = torch.tensor(c[:NODE], dtype=torch.float32)
	src, dst = [], []
	for a, x in b.get("adj", []):
		src.extend([a, x])
		dst.extend([x, a])
	ei = torch.tensor([src, dst], dtype=torch.long) if src else torch.zeros(2, 0, dtype=torch.long)
	rules = torch.tensor(row["rules"][:RULES], dtype=torch.float32)
	return nodes, ei, rules, [int(c) for c in row["candidates"]]

def run(pol, exe, ep, ro, seed, temp=1.0, maps=None, oracle=0, decisive_gap=8.0, soft_labels=False):
	"""Play `ep` x `ro` episodes; return (trajectories, oracle-labeled decisions).

	With oracle > 0 the env gets --oracle-labels oracle and every state line
	carrying "label" becomes one (state, label) training example.
	"""
	rng = random.Random(seed)
	command = [exe, "--scenarios", str(ep), "--rollouts", str(ro)]
	if maps:
		command += ["--maps", maps]
	if oracle:
		command += ["--oracle-labels", str(oracle)]
	p = sp.Popen(command,
		stdin=sp.PIPE, stdout=sp.PIPE, stderr=sp.DEVNULL, text=True, encoding="utf-8")
	tr, ss, aa, lab = [], [], [], []
	dev = next(pol.parameters()).device if pol is not None else None
	for line in p.stdout:
		r = json.loads(line)
		if r.get("done"):
			tr.append((r.get("episode", 0), ss, aa, r["reward"]))
			ss, aa = [], []
			continue
		n, e, ru, ca = parse(r)
		if "label" in r:
			values = r.get("oracle")
			if soft_labels and values and len(values) > 1:
				# Soft targets keep EVERY decision: the full value
				# vector becomes a softmax target, so near-ties teach
				# "either is fine" instead of being discarded.
				lab.append(((n, e, ru, ca), int(r["label"]), values))
			else:
				# Only decisive decisions teach: where the best candidate
				# clearly beats the runner-up. Near ties are coin flips.
				decisive = False
				if values and len(values) > 1:
					ordered = sorted(values, reverse=True)
					decisive = ordered[0] - ordered[1] >= decisive_gap
				if decisive:
					lab.append(((n, e, ru, ca), int(r["label"]), None))
		if pol is None:
			pick = rng.randrange(len(ca))
		else:
			# Collection always parses on CPU; hop the state onto the
			# model's device (--device cuda) just for this forward.
			with torch.no_grad():
				scores = pol(n.to(dev), e.to(dev), ru.to(dev), ca)
				if temp <= 0.01:
					# Pure argmax: deterministic greedy rollouts that walk
					# straight into the policy's own failure states.
					pick = int(scores.argmax())
				else:
					pr = F.softmax(scores / temp, 0)
					pick = int(torch.multinomial(pr, 1))
		pick = min(pick, len(ca) - 1)
		ss.append((n, e, ru, ca))
		aa.append(pick)
		p.stdin.write(json.dumps({"pick": pick}) + "\n")
		p.stdin.flush()
	p.wait()
	return tr, lab

def evaluate(pol, exe, ep=16, ro=4, maps=None, temp=0.0, seed=4242):
	# temp 0 = argmax (the old metric). temp > 0 samples picks from
	# softmax(scores/temp) with a FIXED-seed RNG — the same sampled
	# deployment the policy will actually run under, and common random
	# numbers across rounds so keep-best scores stay comparable.
	command = [exe, "--scenarios", str(ep), "--rollouts", str(ro)]
	if maps:
		command += ["--maps", maps]
	rng = random.Random(seed)
	p = sp.Popen(command,
		stdin=sp.PIPE, stdout=sp.PIPE, stderr=sp.DEVNULL, text=True, encoding="utf-8")
	rewards = []
	dev = next(pol.parameters()).device
	for line in p.stdout:
		r = json.loads(line)
		if r.get("done"):
			rewards.append(r["reward"])
			continue
		n, e, ru, ca = parse(r)
		with torch.no_grad():
			scores = pol(n.to(dev), e.to(dev), ru.to(dev), ca)
			if temp <= 0.01:
				pick = int(scores.argmax())
			else:
				pick = int(torch.multinomial(F.softmax(scores / temp, 0), 1))
		p.stdin.write(json.dumps({"pick": min(pick, len(ca) - 1)}) + "\n")
		p.stdin.flush()
	p.wait()
	return sum(rewards) / len(rewards) if rewards else 0.0

def prepare_examples(examples, soft_tau, device):
	"""One pass over the pool -> (state, action, weight, soft target).

	Hard clones get weight 1 and no soft target, advantage clones carry
	their group advantage as the weight, oracle examples with a value
	spread get a softmax(v / soft_tau) soft target — the same three
	branches the per-example loss switch used to make inline.
	"""
	prepared = []
	for ex in examples:
		if len(ex) == 3 and isinstance(ex[2], float):
			prepared.append((ex[0], int(ex[1]), float(ex[2]), None))
		elif len(ex) == 3 and ex[2]:
			v = torch.tensor(ex[2], dtype=torch.float32, device=device)
			prepared.append((ex[0], int(ex[1]), 1.0, F.softmax(v / soft_tau, 0)))
		else:
			prepared.append((ex[0], int(ex[1]), 1.0, None))
	return prepared

def batch_loss(policy, chunk, device, entropy=0.0):
	"""Loss for one minibatch: ONE batched forward, per-example targets.

	Hard and advantage-weighted clones vectorize into a gathered
	cross-entropy (weight 1 vs advantage); soft oracle targets become a
	padded dot product with the per-example log-probs. Ragged candidate
	counts are padded with -1e9 — exp() underflows to exactly 0, so
	padded entries contribute nothing to any branch. entropy > 0 adds a
	negative-entropy term summed over the batch: minimizing it maximizes
	entropy, keeping the alternatives alive instead of collapsing onto
	one mode.
	"""
	scores = policy.forward_batch([ex[0] for ex in chunk], device)
	width = max(s.numel() for s in scores)
	logits = scores[0].new_full((len(chunk), width), -1e9)
	for row, s in enumerate(scores):
		logits[row, : s.numel()] = s
	logp = F.log_softmax(logits, dim=-1)
	loss = logits.new_zeros(())
	if entropy > 0.0:
		loss = loss + entropy * (logp.exp() * logp).sum()
	hard = [i for i, ex in enumerate(chunk) if ex[3] is None]
	if hard:
		rows = torch.tensor(hard, device=device)
		actions = torch.tensor([chunk[i][1] for i in hard], device=device)
		weights = torch.tensor([chunk[i][2] for i in hard], device=device)
		loss = loss + (weights * -logp[rows, actions]).sum()
	soft = [i for i, ex in enumerate(chunk) if ex[3] is not None]
	if soft:
		rows = torch.tensor(soft, device=device)
		target = logits.new_zeros((len(soft), width))
		for row, i in enumerate(soft):
			target[row, : chunk[i][3].numel()] = chunk[i][3]
		loss = loss - (target * logp[rows]).sum()
	return loss

def train(policy, prepared, epochs, seed, device, batch=32, sequential=False, lr=5e-3,
		entropy=0.0):
	"""Adam(lr) minibatch training; one optimizer step per batch.

	Default path batches each minibatch through `forward_batch` (one
	graph pass, one head pass, one backward). `sequential` keeps the old
	per-example forward loop for benchmarking; both accumulate the same
	per-example losses over the batch before the single backward, so
	they take identical steps up to float noise.
	"""
	optimizer = torch.optim.Adam(policy.parameters(), lr=lr)
	shuffler = random.Random(seed)
	for epoch in range(epochs):
		order = shuffler.sample(range(len(prepared)), len(prepared))
		total = 0.0
		for start in range(0, len(order), batch):
			chunk = [prepared[i] for i in order[start:start + batch]]
			optimizer.zero_grad()
			if sequential:
				loss = torch.tensor(0.0, device=device)
				for state, action, weight, soft in chunk:
					n, e, ru, ca = state
					scores = policy(n.to(device), e.to(device), ru.to(device), ca)
					if soft is None:
						ce = F.cross_entropy(scores.unsqueeze(0),
							torch.tensor([action], device=device))
						loss = loss + weight * ce
					else:
						loss = loss - (soft * F.log_softmax(scores, 0)).sum()
					if entropy > 0.0:
						lp = F.log_softmax(scores, 0)
						loss = loss + entropy * (lp.exp() * lp).sum()
			else:
				loss = batch_loss(policy, chunk, device, entropy)
			loss.backward()
			optimizer.step()
			total += loss.item()
		if epoch % 25 == 24:
			print(f"  epoch {epoch}: loss {total / len(prepared):.3f}/example")

def main():
	import argparse
	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("--env", required=True)
	parser.add_argument("--episodes", type=int, default=48)
	parser.add_argument("--rollouts", type=int, default=8)
	parser.add_argument("--filter", type=float, default=0.10)
	parser.add_argument("--epochs", type=int, default=100)
	parser.add_argument("--rounds", type=int, default=3)
	parser.add_argument("--temp", type=float, default=2.0)
	parser.add_argument("--maps", type=str, default="mixed")
	parser.add_argument("--oracle", action="store_true",
		help="behavior cloning on env oracle labels instead of outcome-filtered RAFT")
	parser.add_argument("--oracle-labels", type=int, default=8,
		help="labels requested per episode when --oracle is set")
	parser.add_argument("--advantage-clone", action="store_true",
		help="GRPO-style: clone above-average rollouts weighted by group advantage")
	parser.add_argument("--best-of-k", type=int, default=0,
		help="per-episode best-of-K trajectory cloning by final reward")
	parser.add_argument("--decisive-gap", type=float, default=8.0,
		help="oracle label kept only when best beats runner-up by this much")
	parser.add_argument("--soft-tau", type=float, default=3.0,
		help="softmax temperature over oracle values for soft targets")
	parser.add_argument("--weights", type=str, default="target/territory-gnn.pt")
	parser.add_argument("--device", choices=["cpu", "cuda"], default="cpu",
		help="device for the model and training batches; data collection always parses on CPU")
	parser.add_argument("--sequential", action="store_true",
		help="train with the old per-example forward loop (benchmarking aid)")
	parser.add_argument("--sources", type=str, default="random,policy@t,policy@1,greedy",
		help="comma list of collection sources for rounds 2+: random, policy@t (temp sampling), policy@1, greedy")
	parser.add_argument("--accumulate", action="store_true",
		help="grow the training pool across rounds instead of replacing it each round")
	parser.add_argument("--warm-start", action="store_true",
		help="continue training the same network across rounds instead of retraining from scratch")
	parser.add_argument("--lr", type=float, default=5e-3,
		help="Adam learning rate for train()")
	parser.add_argument("--soft-labels", action="store_true",
		help="oracle mode: keep every decision with a softmax(values/soft-tau) target instead of decisive hard labels only")
	parser.add_argument("--entropy", type=float, default=0.0,
		help="entropy regularization coefficient: penalize concentrated candidate distributions so sampled deployment keeps its alternatives alive")
	parser.add_argument("--eval-temp", type=float, default=0.0,
		help="selection/eval temperature: 0 = argmax (old metric), >0 samples picks like deployment does")
	parser.add_argument("--init", type=str, default="",
		help="initialize the round-1 policy from this checkpoint instead of random init")
	options = parser.parse_args()

	device = torch.device(options.device)
	policy = None
	best_score, best_state = -1e9, None
	pool_all = []
	for round_index in range(1, options.rounds + 1):
		sources = ["random"]
		oracle_k = options.oracle_labels if options.oracle else 0
		decisive_gap = options.decisive_gap
		tr, lab = run(None, options.env, options.episodes, options.rollouts,
			seed=700 + round_index * 17, maps=options.maps, oracle=oracle_k, decisive_gap=decisive_gap,
			soft_labels=options.soft_labels)
		active = set(options.sources.split(","))
		if policy is not None and "policy@t" in active:
			sources.append(f"policy@{options.temp:g}")
			ptr, plab = run(policy, options.env, max(8, options.episodes // 2), options.rollouts,
				seed=800 + round_index * 31, temp=options.temp, maps=options.maps, oracle=oracle_k, decisive_gap=decisive_gap,
				soft_labels=options.soft_labels)
			tr += ptr
			lab += plab
		# DAgger: label the policy's near-greedy distribution too —
		# the states it actually drifts into under argmax play.
		if policy is not None and "policy@1" in active:
			sources.append("policy@1")
			dtr, dlab = run(policy, options.env, max(8, options.episodes // 2), options.rollouts,
				seed=900 + round_index * 37, temp=1.0, maps=options.maps, oracle=oracle_k, decisive_gap=decisive_gap,
				soft_labels=options.soft_labels)
			tr += dtr
			lab += dlab
		# And pure greedy: it walks into its own loops and gets told.
		if policy is not None and "greedy" in active:
			sources.append("greedy")
			gtr, glab = run(policy, options.env, max(8, options.episodes // 3), options.rollouts,
				seed=1100 + round_index * 41, temp=0.0, maps=options.maps, oracle=oracle_k, decisive_gap=decisive_gap,
				soft_labels=options.soft_labels)
			tr += gtr
			lab += glab
		rw = sorted(t[3] for t in tr)

		if options.advantage_clone:
			# GRPO-style soft cloning: within each scenario's group of
			# real rollouts, weight each trajectory by its group-relative
			# advantage (r - mean) / std; above-average trajectories are
			# cloned proportionally, below-average ignored. The group is
			# same-scenario rollouts — common random numbers built in.
			by_episode = {}
			for ep, ss, aa, r in tr:
				by_episode.setdefault(ep, []).append((r, ss, aa))
			pool = []
			kept = weighted = 0
			for ep, rolls in sorted(by_episode.items()):
				rewards = [roll[0] for roll in rolls]
				mean = sum(rewards) / len(rewards)
				spread = (sum((x - mean) ** 2 for x in rewards) / len(rewards)) ** 0.5
			for r, ss, aa in rolls:
				# Above-average play is cloned proportionally to its
				# group advantage; below-average play is ignored. (The
				# signed "push away from disasters" variant was tried
				# and rejected: unlearning noisy below-average play
				# also unlearns its good moves.)
				advantage = (r - mean) / (spread + 1e-3)
				if advantage <= 0.05:
					continue
				kept += 1
				weighted += advantage
				for sa, a in zip(ss, aa):
					pool.append((sa, a, advantage))
			print(f"round {round_index} [{'+'.join(sources)}]: {len(tr)} tr over "
				f"{len(by_episode)} scenarios, cloned {kept} above-average "
				f"(weight sum {weighted:.0f}) -> {len(pool)} weighted dec")
		elif options.best_of_k:
			# Per-episode best-of-K: same scenario, K real trajectories,
			# clone the best by final reward — real play, real reward,
			# no heuristic value estimates.
			by_episode = {}
			for ep, ss, aa, r in tr:
				by_episode.setdefault(ep, []).append((r, ss, aa))
			pool = []
			bests = []
			for ep, rolls in sorted(by_episode.items()):
				rolls.sort(key=lambda roll: -roll[0])
				best = rolls[0][0]
				bests.append(best)
				for _, ss, aa in rolls[:options.best_of_k]:
					pool.extend(zip(ss, aa))
			bests.sort()
			print(f"round {round_index} [{'+'.join(sources)}]: {len(tr)} tr over "
				f"{len(by_episode)} scenarios, best-of-{options.best_of_k} med "
				f"{bests[len(bests)//2]:+.1f} max {bests[-1]:+.1f} -> {len(pool)} dec")
		elif options.oracle:
			# Plain behavior cloning: every oracle-labeled decision trains,
			# no outcome filtering (the oracle IS the filter).
			print(f"round {round_index} [{'+'.join(sources)}]: {len(tr)} tr, "
				f"min {rw[0]:+.1f} med {rw[len(rw)//2]:+.1f} max {rw[-1]:+.1f}, "
				f"{len(lab)} oracle-labeled dec")
			pool = lab
		else:
			thr = rw[int(len(rw) * (1 - options.filter))]
			good = [(s, a) for s, a, r in tr if r >= thr]
			dec = sum(len(s) for s, _ in good)
			full = sum(1 for r in rw if r >= 99.0)
			print(f"round {round_index} [{'+'.join(sources)}]: {len(tr)} tr, "
				f"min {rw[0]:+.1f} med {rw[len(rw)//2]:+.1f} max {rw[-1]:+.1f}, "
				f"full occ {full}/{len(rw)}")
			print(f"  top {options.filter:.0%} thr {thr:+.1f} -> {len(good)} tr, {dec} dec")
			pool = good

		# Retrain on the pool each round — from scratch, or warm-started on
		# an accumulated pool when --warm-start/--accumulate are set (the
		# from-scratch-on-fresh-pool variant oscillates round to round).
		if options.accumulate:
			pool_all = pool_all + pool
		else:
			pool_all = pool
		if policy is None or not options.warm_start:
			policy = TerritoryGNN().to(device)
			if options.init:
				policy.load_state_dict({k: v.to(device) for k, v in
					torch.load(options.init, map_location="cpu").items()})
				print(f"warm-init from {options.init}")
		examples = pool_all if (options.oracle or options.best_of_k or options.advantage_clone) else [
			(sa, a) for sa, a in pool_all]
		# Pre-collate every example once into (state, action, weight,
		# soft target); each minibatch then needs one batched forward.
		prepared = prepare_examples(examples, options.soft_tau, device)
		train(policy, prepared, options.epochs, round_index, device,
			sequential=options.sequential, lr=options.lr, entropy=options.entropy)

		# Selection metric = how the policy deploys: argmax when
		# --eval-temp is 0, temperature-sampled otherwise. When sampling,
		# also print the argmax number — the two diverge by 20+ points
		# and only the sampled one is the real deployment score.
		score = evaluate(policy, options.env, maps=options.maps, temp=options.eval_temp)
		metric = "argmax" if options.eval_temp <= 0.01 else f"sampled@{options.eval_temp:g}"
		if options.eval_temp > 0.01:
			argmax_score = evaluate(policy, options.env, maps=options.maps)
			print(f"  {metric} {score:+.2f} | argmax {argmax_score:+.2f} after round {round_index}")
		else:
			print(f"  {metric} {score:+.2f} after round {round_index}")
		# Keep the best round's weights, not the last — and checkpoint
		# immediately: a killed run (session death, Ctrl-C) still leaves
		# the best policy on disk.
		if score > best_score:
			best_score = score
			best_state = {k: v.detach().cpu().clone() for k, v in policy.state_dict().items()}
			torch.save(best_state, options.weights)

	if best_state is not None:
		policy.load_state_dict(best_state)
	# Save CPU tensors so a cuda-trained checkpoint loads anywhere.
	torch.save({k: v.cpu() for k, v in policy.state_dict().items()}, options.weights)
	print(f"saved {options.weights} (best {metric} {best_score:+.2f})")

	uni_tr, _ = run(None, options.env, 16, 4, seed=999, maps=options.maps)
	uni = sum(t[3] for t in uni_tr) / len(uni_tr)
	print(f"\nmixed {metric}: {best_score:+.3f} (best round) | mixed uniform: {uni:+.3f} | improvement: {best_score - uni:+.3f}")
	for kind in ("real", "synth"):
		greedy_kind = evaluate(policy, options.env, maps=kind, temp=options.eval_temp)
		uni_kind_tr, _ = run(None, options.env, 16, 4, seed=999, maps=kind)
		uni_kind = sum(t[3] for t in uni_kind_tr) / len(uni_kind_tr)
		print(f"{kind:5} policy: {greedy_kind:+.3f} | uniform: {uni_kind:+.3f} | improvement: {greedy_kind - uni_kind:+.3f}")

if __name__ == "__main__":
	main()
