"""Typed adapter that drives the `foch` CLI as the merge oracle.

The integration seam (verified against `crates/foch-engine/tests/fixtures`):

* IN  — a priority ordering is injected as a `foch.toml` in the process CWD
        (`[[resolutions]] mod=".." priority_boost=N`). foch's config search is
        cwd -> playset_root -> ~/.config (foch-core/src/config.rs:663-675), so a
        cwd-local foch.toml applies without mutating the user's playset.
* OUT — `foch merge-plan --format json` yields the conflict graph
        (`MergePlanResult`, foch-core/src/model/merge.rs); `foch merge --out`
        materializes the merged mod tree (the artifact an error oracle evaluates).

Note on `winner_dependent`: for `last_writer_overlay` / `manual_conflict` the
file-level winner is order-sensitive and visible in the plan. For deep merges
(`structural_merge` / `localisation_merge`) order only changes *leaf-collision*
winners, which surface solely in the materialized bytes — hence Gate 0 hashes the
tree rather than trusting the plan's file-level `winner`.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Spacing between adjacent ranks when an ordering is encoded as per-mod boosts.
# Wide enough to dominate base playlist precedence regardless of playset size.
BOOST_STEP = 1000

# foch writes its own report artifacts here inside the --out tree; excluded from
# content hashing because they carry timestamps and derived stats.
FOCH_META_DIR = ".foch"


@dataclass(frozen=True)
class Contributor:
    mod_id: str
    precedence: int
    is_base_game: bool


@dataclass(frozen=True)
class PlanEntry:
    path: str
    strategy: str
    contributors: tuple[Contributor, ...]
    winner: str | None

    @property
    def is_conflict(self) -> bool:
        return len(self.contributors) > 1

    @property
    def winner_dependent(self) -> bool:
        """True when the file-level winner alone changes the output.

        Deep merges can still vary at leaf collisions; that is why prevalence is
        ultimately decided by the materialized tree hash, not this flag.
        """
        return self.strategy in ("last_writer_overlay", "manual_conflict")


@dataclass(frozen=True)
class MergePlan:
    strategies: dict[str, int]
    entries: tuple[PlanEntry, ...]

    @property
    def conflicts(self) -> tuple[PlanEntry, ...]:
        return tuple(e for e in self.entries if e.is_conflict)

    def conflict_mods(self) -> tuple[str, ...]:
        """Non-base mods that participate in at least one conflict.

        These are the only decision-variable dimensions worth ordering — the
        partial-order insight: independent mods cannot change any winner.
        """
        seen: dict[str, None] = {}
        for entry in self.conflicts:
            for contributor in entry.contributors:
                if not contributor.is_base_game:
                    seen.setdefault(contributor.mod_id, None)
        return tuple(seen)

    @classmethod
    def from_json(cls, payload: Mapping[str, Any]) -> MergePlan:
        entries: list[PlanEntry] = []
        for raw in payload.get("paths", []):
            contributors = tuple(
                Contributor(
                    mod_id=str(c["mod_id"]),
                    precedence=int(c["precedence"]),
                    is_base_game=bool(c.get("is_base_game", False)),
                )
                for c in raw.get("contributors", [])
            )
            winner = raw.get("winner") or None
            entries.append(
                PlanEntry(
                    path=str(raw["path"]),
                    strategy=str(raw["strategy"]),
                    contributors=contributors,
                    winner=str(winner["mod_id"]) if isinstance(winner, dict) else None,
                )
            )
        return cls(strategies=dict(payload.get("strategies", {})), entries=tuple(entries))


def ordering_to_boosts(ordering: Sequence[str], step: int = BOOST_STEP) -> dict[str, int]:
    """Encode a total ordering (lowest-priority first) as per-mod priority boosts."""
    return {mod_id: rank * step for rank, mod_id in enumerate(ordering)}


@dataclass
class FochOracle:
    foch_bin: Path
    playset_path: Path  # path to dlc_load.json (left read-only, never mutated)
    include_game_base: bool = False
    _run_root: Path = field(
        default_factory=lambda: Path(tempfile.mkdtemp(prefix="merge-bo-")), repr=False
    )

    def _base_flags(self) -> list[str]:
        return [] if self.include_game_base else ["--no-game-base"]

    def _config_cwd(self, boosts: Mapping[str, int]) -> Path:
        """Write a foch.toml encoding `boosts` into a fresh CWD dir; return it."""
        run_dir = Path(tempfile.mkdtemp(dir=self._run_root))
        lines: list[str] = []
        for mod_id, boost in sorted(boosts.items()):
            lines.append(f'[[resolutions]]\nmod = "{mod_id}"\npriority_boost = {boost}\n')
        (run_dir / "foch.toml").write_text("\n".join(lines), encoding="utf-8")
        return run_dir

    def merge_plan(self, boosts: Mapping[str, int] | None = None) -> MergePlan:
        run_dir = self._config_cwd(boosts or {})
        proc = subprocess.run(
            [
                str(self.foch_bin),
                "merge-plan",
                str(self.playset_path),
                "--format",
                "json",
                *self._base_flags(),
            ],
            cwd=run_dir,
            capture_output=True,
            text=True,
            check=True,
        )
        return MergePlan.from_json(json.loads(proc.stdout))

    def materialize(self, boosts: Mapping[str, int], out_dir: Path) -> Path:
        run_dir = self._config_cwd(boosts)
        subprocess.run(
            [
                str(self.foch_bin),
                "merge",
                str(self.playset_path),
                "--out",
                str(out_dir),
                "--non-interactive",
                "--force",
                *self._base_flags(),
            ],
            cwd=run_dir,
            capture_output=True,
            text=True,
            check=True,
        )
        return out_dir


def tree_hash(out_dir: Path, exclude_dir: str = FOCH_META_DIR) -> str:
    """Deterministic content hash of a merged mod tree, ignoring foch's reports."""
    digest = hashlib.sha256()
    for path in sorted(p for p in out_dir.rglob("*") if p.is_file()):
        rel = path.relative_to(out_dir)
        if rel.parts and rel.parts[0] == exclude_dir:
            continue
        digest.update(rel.as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def file_hashes(out_dir: Path, exclude_dir: str = FOCH_META_DIR) -> dict[str, str]:
    """Per-file content hashes (relative path -> sha256), ignoring foch's reports."""
    out: dict[str, str] = {}
    for path in out_dir.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(out_dir)
        if rel.parts and rel.parts[0] == exclude_dir:
            continue
        out[rel.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out
