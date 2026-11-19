"""Export a TerritoryGNN checkpoint as flat JSON for the Rust port.

Writes two files:
  <out>.json          the weights (nested arrays, parse_array-friendly)
  <out>.parity.json   random states + the Python policy's scores on them,
                      the acceptance fixture for the Rust forward pass.
"""
import json
import random
import sys

import torch

sys.path.insert(0, "src/tools/eu4-decide")
from eu4_decide._territory_raft import NODE, RULES, TerritoryGNN  # noqa: E402


def main():
    src, out = sys.argv[1], sys.argv[2]
    pol = TerritoryGNN()
    pol.load_state_dict(torch.load(src, map_location="cpu"))
    pol.eval()

    sd = pol.state_dict()
    names = [
        "gat1.lin_l.weight", "gat1.lin_l.bias", "gat1.lin_r.weight", "gat1.lin_r.bias",
        "gat1.att", "gat1.bias",
        "gat2.lin_l.weight", "gat2.lin_l.bias", "gat2.lin_r.weight", "gat2.lin_r.bias",
        "gat2.att", "gat2.bias",
        "query.0.weight", "query.0.bias", "query.2.weight", "query.2.bias",
        "head.0.weight", "head.0.bias", "head.2.weight", "head.2.bias",
    ]
    weights = {name: sd[name].tolist() for name in names}
    with open(out, "w") as f:
        json.dump(weights, f)

    # Parity fixture: random connected-ish graphs scored by the real net.
    rng = random.Random(20261118)
    torch.manual_seed(20261118)
    states = []
    for _ in range(6):
        n = rng.randrange(18, 34)
        nodes = torch.randn(n, NODE)
        nodes[:, 8] = 0.0
        nodes[rng.randrange(n), 8] = 1.0  # exactly one "we stand here"
        src_l, dst_l = [], []
        for a in range(n):
            for b in (a - 1, a + 1, a - 3, a + 4):
                if 0 <= b < n and rng.random() < 0.6:
                    src_l.extend([a, b])
                    dst_l.extend([b, a])
        ei = torch.tensor([src_l, dst_l], dtype=torch.long)
        rules = torch.randn(RULES)
        cands = sorted(rng.sample(range(n), rng.randrange(3, 9)))
        with torch.no_grad():
            scores = pol(nodes, ei, rules, cands)
        # Undirected edges as flat (src, dst) pairs; the graph is symmetric.
        pairs = [int(v) for pair in zip(src_l, dst_l) for v in pair]
        states.append({
            "nodes": [[float(v) for v in row] for row in nodes],
            "edges": pairs,
            "rules": [float(v) for v in rules],
            "cands": cands,
            "scores": [round(float(s), 6) for s in scores],
        })
    with open(out + ".parity.json", "w") as f:
        json.dump({"weights_file": out, "states": states}, f)
    print(f"{out}: {sum(t.numel() for t in sd.values())} params, "
          f"{len(states)} parity states")


if __name__ == "__main__":
    main()
