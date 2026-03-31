#!/usr/bin/env python3
"""Build a prefilled EU4 builtin trigger/effect symbol catalog.

Sources:
- CWTools EU4 config (`triggers.cwt`, `effects.cwt`)
- EU4 wiki mirrors cached via r.jina.ai (`Effects`, `Conditions`, `Scope`)
- Local base game files (assignment key frequency)
"""

from __future__ import annotations

import argparse
import json
import os
import re
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set, Tuple


RESERVED_KEYWORDS = [
    "if",
    "else_if",
    "else",
    "limit",
    "trigger",
    "potential",
    "allow",
    "AND",
    "OR",
    "NOT",
]

CONTEXTUAL_KEYWORDS = [
    "effect",
    "hidden_effect",
    "custom_tooltip",
    "hidden_trigger",
    "ai_will_do",
    "modifier",
    "option",
    "after",
    "immediate",
    "country_event",
    "province_event",
    "namespace",
    "id",
    "title",
    "desc",
    "name",
    "mean_time_to_happen",
    "ai_chance",
    "chance",
    "base",
    "active",
    "on_add",
    "on_remove",
    "on_start",
    "on_end",
    "on_monthly",
    "can_start",
    "can_stop",
    "can_end",
    "progress",
    "every_owned_province",
    "country_decisions",
    "province_decisions",
    "religion_decisions",
    "government_decisions",
]

ALIAS_KEYWORDS = ["ROOT", "FROM", "THIS", "PREV"]

VALID_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_:@.-]*$")
ASSIGNMENT_KEY_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_:@.-]*)\s*=")
CWTOOLS_ALIAS_RE = re.compile(r"^alias\[(trigger|effect):([^\]]+)\]\s*=")
CWTOOLS_SCOPE_RE = re.compile(r"^##\s*scope\s*=\s*([A-Za-z_]+)")
CWTOOLS_DESC_RE = re.compile(r"^###\s*(.+)$")
WIKI_EFFECT_ROW_RE = re.compile(r"^\|\s*([A-Za-z_][A-Za-z0-9_:@.-]*)\s*\|")
WIKI_CONDITION_RE = re.compile(
    r"^([A-Za-z_][A-Za-z0-9_:@.-]*)\s+.*(?:Returns true|Hides the enclosed trigger)",
)


@dataclass
class SymbolEntry:
    name: str
    scopes: Set[str] = field(default_factory=set)
    sources: Set[str] = field(default_factory=set)
    notes: Set[str] = field(default_factory=set)
    game_count: int = 0

    def to_json(self) -> dict:
        return {
            "name": self.name,
            "scopes": sorted(self.scopes),
            "sources": sorted(self.sources),
            "notes": sorted(self.notes),
            "game_count": self.game_count,
        }


def detect_game_root() -> Optional[Path]:
    explicit = os.environ.get("FOCH_EU4_PATH")
    if explicit:
        p = Path(explicit).expanduser()
        if p.is_dir():
            return p

    home = Path.home()
    candidates = [
        home / "Library/Application Support/Steam/steamapps/common/Europa Universalis IV",
        home / ".steam/steam/steamapps/common/Europa Universalis IV",
        home / ".local/share/Steam/steamapps/common/Europa Universalis IV",
        Path("C:/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV"),
        Path("C:/Program Files/Steam/steamapps/common/Europa Universalis IV"),
    ]
    for path in candidates:
        if path.is_dir():
            return path
    return None


def should_skip_alias_name(raw: str) -> bool:
    return (
        "<" in raw
        or "enum[" in raw
        or "alias_match" in raw
        or "alias_name[" in raw
        or "scripted_effect_params" in raw
        or "value[" in raw
        or "value_set[" in raw
    )


def upsert(entries: Dict[str, SymbolEntry], name: str) -> SymbolEntry:
    if name not in entries:
        entries[name] = SymbolEntry(name=name)
    return entries[name]


def parse_cwtools_aliases(path: Path, expected_kind: str) -> Dict[str, SymbolEntry]:
    entries: Dict[str, SymbolEntry] = {}
    current_scope = "any"
    pending_note: Optional[str] = None

    for raw_line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw_line.strip()
        if not line:
            continue

        scope_m = CWTOOLS_SCOPE_RE.match(line)
        if scope_m:
            current_scope = scope_m.group(1).lower()
            continue

        note_m = CWTOOLS_DESC_RE.match(line)
        if note_m:
            pending_note = note_m.group(1).strip()
            continue

        alias_m = CWTOOLS_ALIAS_RE.match(line)
        if not alias_m:
            continue

        kind = alias_m.group(1)
        if kind != expected_kind:
            continue

        name = alias_m.group(2).strip()
        if should_skip_alias_name(name) or not VALID_NAME_RE.match(name):
            pending_note = None
            continue

        entry = upsert(entries, name)
        entry.scopes.add(current_scope)
        entry.sources.add("cwtools")
        if pending_note:
            entry.notes.add(pending_note)
        pending_note = None

    return entries


def parse_wiki_effects(path: Path) -> Dict[str, SymbolEntry]:
    entries: Dict[str, SymbolEntry] = {}
    current_scope = "any"

    for raw_line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw_line.rstrip()
        if line.startswith("Country scope"):
            current_scope = "country"
            continue
        if line.startswith("Province scope"):
            current_scope = "province"
            continue
        if line.startswith("Dual scope"):
            current_scope = "any"
            continue

        row_m = WIKI_EFFECT_ROW_RE.match(line)
        if not row_m:
            continue

        name = row_m.group(1)
        if not VALID_NAME_RE.match(name):
            continue

        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        note = ""
        if len(cells) >= 4:
            note = cells[3]

        entry = upsert(entries, name)
        entry.scopes.add(current_scope)
        entry.sources.add("eu4wiki")
        if note:
            entry.notes.add(note)

    return entries


def parse_wiki_conditions(path: Path) -> Dict[str, SymbolEntry]:
    entries: Dict[str, SymbolEntry] = {}
    skip_names = {
        "Country",
        "Province",
        "Anywhere",
        "Tag",
        "Scope",
        "Clause",
        "Identifier",
        "Integer",
        "Float",
        "Boolean",
    }

    for raw_line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw_line.strip()
        if not line:
            continue

        m = WIKI_CONDITION_RE.match(line)
        if not m:
            continue

        name = m.group(1)
        if name in skip_names:
            continue
        if not VALID_NAME_RE.match(name):
            continue

        scope = "any"
        if "Country`" in line:
            scope = "country"
        elif "Province`" in line:
            scope = "province"

        entry = upsert(entries, name)
        entry.scopes.add(scope)
        entry.sources.add("eu4wiki")

    return entries


def scan_game_assignment_counts(game_root: Path, max_files: int) -> Tuple[Counter, int]:
    counter: Counter = Counter()
    roots = [game_root / "common", game_root / "events", game_root / "decisions"]

    files: List[Path] = []
    for root in roots:
        if not root.is_dir():
            continue
        files.extend(sorted(root.rglob("*.txt")))

    if max_files > 0:
        files = files[:max_files]

    for path in files:
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue

        for line in text.splitlines():
            content = line.split("#", 1)[0]
            if "=" not in content:
                continue
            for m in ASSIGNMENT_KEY_RE.finditer(content):
                counter[m.group(1)] += 1

    return counter, len(files)


def merge_entries(*groups: Dict[str, SymbolEntry]) -> Dict[str, SymbolEntry]:
    merged: Dict[str, SymbolEntry] = {}
    for group in groups:
        for name, entry in group.items():
            target = upsert(merged, name)
            target.scopes.update(entry.scopes)
            target.sources.update(entry.sources)
            target.notes.update(entry.notes)
    return merged


def attach_game_counts(entries: Dict[str, SymbolEntry], counts: Counter) -> None:
    for name, entry in entries.items():
        entry.game_count = int(counts.get(name, 0))


def build_catalog(
    cwtools_dir: Path,
    irony_readme: Path,
    wiki_effects: Path,
    wiki_conditions: Path,
    wiki_scope: Path,
    game_root: Optional[Path],
    max_game_files: int,
) -> dict:
    triggers_cw = parse_cwtools_aliases(cwtools_dir / "triggers.cwt", "trigger")
    effects_cw = parse_cwtools_aliases(cwtools_dir / "effects.cwt", "effect")

    triggers_wiki = parse_wiki_conditions(wiki_conditions)
    effects_wiki = parse_wiki_effects(wiki_effects)

    merged_triggers = merge_entries(triggers_cw, triggers_wiki)
    merged_effects = merge_entries(effects_cw, effects_wiki)

    assignment_counts: Counter = Counter()
    scanned_files = 0
    if game_root is not None and game_root.is_dir():
        assignment_counts, scanned_files = scan_game_assignment_counts(game_root, max_game_files)

    attach_game_counts(merged_triggers, assignment_counts)
    attach_game_counts(merged_effects, assignment_counts)

    known_symbols = set(merged_triggers.keys()) | set(merged_effects.keys())
    reserved = set(RESERVED_KEYWORDS)
    contextual = set(CONTEXTUAL_KEYWORDS)
    aliases = set(ALIAS_KEYWORDS)

    game_only_candidates = []
    for name, count in assignment_counts.most_common(400):
        if name in known_symbols or name in reserved or name in contextual or name in aliases:
            continue
        if name.isupper():
            continue
        game_only_candidates.append({"name": name, "count": int(count)})
        if len(game_only_candidates) >= 120:
            break

    scope_summary = wiki_scope.read_text(encoding="utf-8", errors="replace").splitlines()[:40]
    irony_summary = []
    if irony_readme.is_file():
        lines = irony_readme.read_text(encoding="utf-8", errors="replace").splitlines()
        irony_summary = [line for line in lines if "CWTools" in line or "Special thanks" in line][
            :8
        ]

    return {
        "version": 1,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "sources": {
            "cwtools_dir": str(cwtools_dir),
            "irony_readme": str(irony_readme),
            "wiki_effects": str(wiki_effects),
            "wiki_conditions": str(wiki_conditions),
            "wiki_scope": str(wiki_scope),
            "game_root": str(game_root) if game_root else None,
        },
        "scan_meta": {
            "game_files_scanned": scanned_files,
            "game_assignment_keys_distinct": len(assignment_counts),
            "irony_summary": irony_summary,
            "wiki_scope_head": scope_summary,
        },
        "reserved_keywords": sorted(RESERVED_KEYWORDS),
        "contextual_keywords": sorted(CONTEXTUAL_KEYWORDS),
        "alias_keywords": sorted(ALIAS_KEYWORDS),
        "builtin_triggers": [
            merged_triggers[name].to_json() for name in sorted(merged_triggers.keys())
        ],
        "builtin_effects": [
            merged_effects[name].to_json() for name in sorted(merged_effects.keys())
        ],
        "game_only_candidates": game_only_candidates,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--cwtools-dir",
        type=Path,
        default=Path("/tmp/foch-sources/cwtools-eu4-config"),
    )
    parser.add_argument(
        "--irony-readme",
        type=Path,
        default=Path("/tmp/foch-sources/IronyModManager/Readme.md"),
    )
    parser.add_argument(
        "--wiki-effects",
        type=Path,
        default=Path("/tmp/foch-sources/wiki/eu4wiki_effects.md"),
    )
    parser.add_argument(
        "--wiki-conditions",
        type=Path,
        default=Path("/tmp/foch-sources/wiki/eu4wiki_conditions.md"),
    )
    parser.add_argument(
        "--wiki-scope",
        type=Path,
        default=Path("/tmp/foch-sources/wiki/eu4wiki_scope.md"),
    )
    parser.add_argument(
        "--game-root",
        type=Path,
        default=None,
        help="Optional EU4 game root. If omitted, auto-detect by platform.",
    )
    parser.add_argument(
        "--max-game-files",
        type=int,
        default=0,
        help="Limit scanned game files (0 = no limit).",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("crates/foch-language/src/data/eu4_builtin_catalog.json"),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    game_root = args.game_root.expanduser() if args.game_root else detect_game_root()

    catalog = build_catalog(
        cwtools_dir=args.cwtools_dir.expanduser(),
        irony_readme=args.irony_readme.expanduser(),
        wiki_effects=args.wiki_effects.expanduser(),
        wiki_conditions=args.wiki_conditions.expanduser(),
        wiki_scope=args.wiki_scope.expanduser(),
        game_root=game_root,
        max_game_files=args.max_game_files,
    )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(catalog, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(f"wrote {args.output}")
    print(
        "builtin sizes:",
        f"triggers={len(catalog['builtin_triggers'])}",
        f"effects={len(catalog['builtin_effects'])}",
        f"game_only_candidates={len(catalog['game_only_candidates'])}",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
