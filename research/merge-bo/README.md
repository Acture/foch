# merge-bo

Research harness for **Bayesian optimization of EU4 mod-merge validity**. Drives the
`foch` CLI as a black box and (later) an EU4 observer-mode error oracle.

Decision variable = the merge **priority ordering** (which mod wins each conflict);
objective = whether the merged config **errors** in-game. See the design plan for the
two-track structure (method core + prevalence gate ladder) and the gate-driven decision
nodes (is-this-BO? / parameterization).

## Layout

- `merge_bo/foch_oracle.py` — typed adapter: priority ordering → `foch.toml` →
  `merge-plan` JSON conflict graph + materialized merge tree. Hashes mod content,
  ignoring foch's `.foch/` report artifacts.
- `merge_bo/gate0.py` — **Gate 0**: sample K random orderings, materialize each, measure
  how much the merged tree varies. GO if >1 distinct tree; NO-GO ⇒ optimization target is
  flat ⇒ stop before EU4. Reports the order-sensitive file set (Gate 1's target).
- *(later)* `kernel.py` / `surrogate.py` / `acquisition.py` — Track A method core; pulls
  `botorch`+`gpytorch` via the `method` optional-dependency group. Built only if Gate 0/1
  pass.
- *(later)* `oracle/eu4_observer.py` — Gate 1: EU4 observer mode → `error.log` → classified
  error set.

## Running Gate 0

Stdlib-only — no install needed beyond a built `foch` binary:

```sh
PYTHONPATH=. python3 -m merge_bo.gate0 \
  --foch-bin /path/to/foch \
  --playset  /path/to/dlc_load.json \
  --k 24 --seed 0 --out gate0-report.json
# add --include-game-base to merge against installed EU4 base data
```

For the method track later: `uv sync --extra method`.
