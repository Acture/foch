"""Gate 0 — prevalence of order-sensitivity in a real modpack (cheap, no game).

Founding assumption of the whole program: that *which mod wins each conflict*
actually changes the merged output on real modpacks, not just on constructed
fixtures. Gate 0 tests it directly: sample K random priority orderings, materialize
each via foch, and measure how much the merged tree varies.

GO criterion: the merged tree takes >1 distinct value across random orderings
(equivalently, ≥1 file is order-sensitive). NO-GO: outputs are identical regardless
of ordering -> the optimization target is flat -> stop before EU4 (Gate 1).

The set of order-sensitive files is reported because it is exactly what Gate 1
should target when launching the EU4 observer-mode error oracle.
"""

from __future__ import annotations

import argparse
import json
import logging
import random
import shutil
import tempfile
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path

from merge_bo.foch_oracle import FochOracle, ordering_to_boosts, file_hashes

log = logging.getLogger("gate0")


@dataclass
class Gate0Report:
    n_conflict_mods: int
    n_conflict_paths: int
    strategy_distribution: dict[str, int]
    winner_dependent_paths: int
    k_orderings: int
    distinct_trees: int
    order_sensitive_files: list[str]
    verdict: str  # "GO" | "NO-GO"


def run_gate0(oracle: FochOracle, k: int, seed: int) -> Gate0Report:
    plan = oracle.merge_plan()
    conflicts = plan.conflicts
    mods = list(plan.conflict_mods())
    winner_dependent = sum(1 for entry in conflicts if entry.winner_dependent)

    log.info(
        "merge-plan: %d conflict paths over %d mods; strategies=%s; %d clearly winner-dependent",
        len(conflicts),
        len(mods),
        dict(Counter(e.strategy for e in conflicts)),
        winner_dependent,
    )

    if not mods:
        return Gate0Report(
            n_conflict_mods=0,
            n_conflict_paths=0,
            strategy_distribution=dict(plan.strategies),
            winner_dependent_paths=0,
            k_orderings=0,
            distinct_trees=0,
            order_sensitive_files=[],
            verdict="NO-GO",
        )

    rng = random.Random(seed)
    tree_hashes: set[str] = set()
    # rel path -> set of content hashes seen across orderings
    per_file: dict[str, set[str]] = {}
    scratch = Path(tempfile.mkdtemp(prefix="merge-bo-gate0-"))
    try:
        for i in range(k):
            ordering = mods[:]
            rng.shuffle(ordering)
            boosts = ordering_to_boosts(ordering)
            out_dir = scratch / f"out_{i}"
            oracle.materialize(boosts, out_dir)
            hashes = file_hashes(out_dir)
            # full-tree hash = hash of the sorted (path, filehash) pairs
            tree_hashes.add(json.dumps(sorted(hashes.items()), separators=(",", ":")))
            for rel, h in hashes.items():
                per_file.setdefault(rel, set()).add(h)
            shutil.rmtree(out_dir, ignore_errors=True)
            log.info("ordering %d/%d -> %d distinct trees so far", i + 1, k, len(tree_hashes))
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    order_sensitive = sorted(rel for rel, hs in per_file.items() if len(hs) > 1)
    return Gate0Report(
        n_conflict_mods=len(mods),
        n_conflict_paths=len(conflicts),
        strategy_distribution=dict(plan.strategies),
        winner_dependent_paths=winner_dependent,
        k_orderings=k,
        distinct_trees=len(tree_hashes),
        order_sensitive_files=order_sensitive,
        verdict="GO" if len(tree_hashes) > 1 else "NO-GO",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--foch-bin", type=Path, required=True, help="path to the foch binary")
    parser.add_argument("--playset", type=Path, required=True, help="path to dlc_load.json")
    parser.add_argument("--k", type=int, default=24, help="number of random orderings to sample")
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument(
        "--include-game-base",
        action="store_true",
        help="merge against installed EU4 base data (omit for fixtures / --no-game-base)",
    )
    parser.add_argument("--out", type=Path, default=None, help="write the JSON report here")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(message)s")
    oracle = FochOracle(
        foch_bin=args.foch_bin,
        playset_path=args.playset,
        include_game_base=args.include_game_base,
    )
    report = run_gate0(oracle, k=args.k, seed=args.seed)

    payload = asdict(report)
    print(json.dumps(payload, indent=2))
    if args.out is not None:
        args.out.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    log.info(
        "VERDICT %s — %d/%d distinct merged trees; %d order-sensitive files",
        report.verdict,
        report.distinct_trees,
        report.k_orderings,
        len(report.order_sensitive_files),
    )
    return 0 if report.verdict == "GO" else 1


if __name__ == "__main__":
    raise SystemExit(main())
