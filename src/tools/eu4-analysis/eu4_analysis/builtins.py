"""Build a prefilled EU4 builtin trigger/effect symbol catalog.

Sources:
- CWTools EU4 config (`triggers.cwt`, `effects.cwt`)
- EU4 wiki mirrors cached via r.jina.ai (`Effects`, `Conditions`, `Scope`)
- Local base game files (assignment key frequency)
"""

from __future__ import annotations

import argparse
import json
import logging
import re
import time
from collections import Counter
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import TypedDict

LOGGER: logging.Logger = logging.getLogger(__name__)


class Symbol(TypedDict):
	name: str
	scopes: list[str]
	sources: list[str]
	notes: list[str]
	game_count: int


class GameCandidate(TypedDict):
	name: str
	count: int


class Catalog(TypedDict):
	version: int
	generated_at_utc: str
	sources: dict[str, str | None]
	scan_meta: dict[str, object]
	reserved_keywords: list[str]
	contextual_keywords: list[str]
	alias_keywords: list[str]
	builtin_triggers: list[Symbol]
	builtin_effects: list[Symbol]
	game_only_candidates: list[GameCandidate]


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
	scopes: set[str] = field(default_factory=set)
	sources: set[str] = field(default_factory=set)
	notes: set[str] = field(default_factory=set)
	game_count: int = 0

	def to_json(self) -> Symbol:
		return {
			"name": self.name,
			"scopes": sorted(self.scopes),
			"sources": sorted(self.sources),
			"notes": sorted(self.notes),
			"game_count": self.game_count,
		}


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


def upsert(entries: dict[str, SymbolEntry], name: str) -> SymbolEntry:
	if name not in entries:
		entries[name] = SymbolEntry(name=name)
	return entries[name]


def parse_cwtools_aliases(path: Path, expected_kind: str) -> dict[str, SymbolEntry]:
	entries: dict[str, SymbolEntry] = {}
	current_scope = "any"
	pending_note: str | None = None

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


def parse_wiki_effects(path: Path) -> dict[str, SymbolEntry]:
	entries: dict[str, SymbolEntry] = {}
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


def parse_wiki_conditions(path: Path) -> dict[str, SymbolEntry]:
	entries: dict[str, SymbolEntry] = {}
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


def scan_game_assignment_counts(
	game_root: Path, max_files: int
) -> tuple[Counter[str], int]:
	counter: Counter[str] = Counter()
	roots = [game_root / "common", game_root / "events", game_root / "decisions"]

	files: list[Path] = []
	for root in roots:
		if not root.is_dir():
			continue
		files.extend(sorted(root.rglob("*.txt")))

	if max_files > 0:
		files = files[:max_files]

	for index, path in enumerate(files, 1):
		text: str = path.read_text(encoding="utf-8", errors="replace")
		if index % 500 == 0 or index == len(files):
			LOGGER.info("Scanned %d/%d game files", index, len(files))

		for line in text.splitlines():
			content = line.split("#", 1)[0]
			if "=" not in content:
				continue
			for m in ASSIGNMENT_KEY_RE.finditer(content):
				counter[m.group(1)] += 1

	return counter, len(files)


def merge_entries(*groups: dict[str, SymbolEntry]) -> dict[str, SymbolEntry]:
	merged: dict[str, SymbolEntry] = {}
	for group in groups:
		for name, entry in group.items():
			target = upsert(merged, name)
			target.scopes.update(entry.scopes)
			target.sources.update(entry.sources)
			target.notes.update(entry.notes)
	return merged


def attach_game_counts(entries: dict[str, SymbolEntry], counts: Counter[str]) -> None:
	for name, entry in entries.items():
		entry.game_count = int(counts.get(name, 0))


def build_catalog(
	cwtools_dir: Path,
	irony_readme: Path | None,
	wiki_effects: Path,
	wiki_conditions: Path,
	wiki_scope: Path,
	game_root: Path | None,
	max_game_files: int,
) -> Catalog:
	if max_game_files < 0:
		raise ValueError("max_game_files cannot be negative")
	if game_root is not None and not game_root.is_dir():
		raise ValueError(f"missing game directory: {game_root}")
	triggers_cw = parse_cwtools_aliases(cwtools_dir / "triggers.cwt", "trigger")
	effects_cw = parse_cwtools_aliases(cwtools_dir / "effects.cwt", "effect")

	triggers_wiki = parse_wiki_conditions(wiki_conditions)
	effects_wiki = parse_wiki_effects(wiki_effects)

	merged_triggers = merge_entries(triggers_cw, triggers_wiki)
	merged_effects = merge_entries(effects_cw, effects_wiki)

	assignment_counts: Counter[str] = Counter()
	scanned_files = 0
	if game_root is not None and game_root.is_dir():
		assignment_counts, scanned_files = scan_game_assignment_counts(
			game_root, max_game_files
		)

	attach_game_counts(merged_triggers, assignment_counts)
	attach_game_counts(merged_effects, assignment_counts)

	known_symbols = set(merged_triggers.keys()) | set(merged_effects.keys())
	reserved = set(RESERVED_KEYWORDS)
	contextual = set(CONTEXTUAL_KEYWORDS)
	aliases = set(ALIAS_KEYWORDS)

	game_only_candidates: list[GameCandidate] = []
	for name, count in assignment_counts.most_common(400):
		if (
			name in known_symbols
			or name in reserved
			or name in contextual
			or name in aliases
		):
			continue
		if name.isupper():
			continue
		game_only_candidates.append({"name": name, "count": int(count)})
		if len(game_only_candidates) >= 120:
			break

	scope_summary = wiki_scope.read_text(
		encoding="utf-8", errors="replace"
	).splitlines()[:40]
	irony_summary = []
	if irony_readme is not None:
		lines = irony_readme.read_text(encoding="utf-8", errors="replace").splitlines()
		irony_summary = [
			line for line in lines if "CWTools" in line or "Special thanks" in line
		][:8]

	return {
		"version": 1,
		"generated_at_utc": datetime.now(UTC).isoformat(),
		"sources": {
			"cwtools_dir": str(cwtools_dir),
			"irony_readme": str(irony_readme) if irony_readme else None,
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


@dataclass(frozen=True)
class BuiltinOptions:
	cwtools_dir: Path
	wiki_effects: Path
	wiki_conditions: Path
	wiki_scope: Path
	output: Path
	irony_readme: Path | None = None
	game_root: Path | None = None
	max_game_files: int = 0


def add_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument(
		"--cwtools-dir",
		type=Path,
		default=Path(__file__).resolve().parents[4]
		/ "src/packages/foch/vendor/cwtools-eu4-config",
	)
	parser.add_argument("--wiki-effects", type=Path, required=True)
	parser.add_argument("--wiki-conditions", type=Path, required=True)
	parser.add_argument("--wiki-scope", type=Path, required=True)
	parser.add_argument("--irony-readme", type=Path)
	parser.add_argument(
		"--game-root",
		type=Path,
		help="Explicit opt-in to scan an installed game (read only).",
	)
	parser.add_argument(
		"--max-game-files",
		type=int,
		default=0,
		help="Limit game files when --game-root is supplied (0 = unlimited).",
	)
	parser.add_argument(
		"--output",
		type=Path,
		required=True,
		help="Candidate JSON destination; review before replacing embedded data.",
	)


def run(options: BuiltinOptions) -> None:
	started: float = time.monotonic()
	catalog: Catalog = build_catalog(
		cwtools_dir=options.cwtools_dir.expanduser(),
		irony_readme=options.irony_readme.expanduser()
		if options.irony_readme
		else None,
		wiki_effects=options.wiki_effects.expanduser(),
		wiki_conditions=options.wiki_conditions.expanduser(),
		wiki_scope=options.wiki_scope.expanduser(),
		game_root=options.game_root.expanduser() if options.game_root else None,
		max_game_files=options.max_game_files,
	)
	output: Path = options.output.expanduser()
	output.parent.mkdir(parents=True, exist_ok=True)
	output.write_text(
		json.dumps(catalog, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
	)
	LOGGER.info(
		"Wrote %s in %.1fs: %d triggers, %d effects, %d game candidates",
		output,
		time.monotonic() - started,
		len(catalog["builtin_triggers"]),
		len(catalog["builtin_effects"]),
		len(catalog["game_only_candidates"]),
	)
