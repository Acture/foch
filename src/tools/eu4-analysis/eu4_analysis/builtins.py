"""Build the EU4 builtin keyword catalog embedded in Foch.

The catalog holds only the script keywords and base scope names the analyzer
reads. Triggers, effects and scope changers come from the CWT schema binding
each statement, so the catalog has no external sources.
"""

from __future__ import annotations

import argparse
import json
import logging
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import TypedDict

LOGGER: logging.Logger = logging.getLogger(__name__)


class BaseScopes(TypedDict):
	country: str
	province: str


class Catalog(TypedDict):
	version: int
	generated_at_utc: str
	reserved_keywords: list[str]
	contextual_keywords: list[str]
	alias_keywords: list[str]
	base_scopes: BaseScopes


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

BASE_SCOPES: BaseScopes = {"country": "country", "province": "province"}


def build_catalog() -> Catalog:
	return {
		"version": 1,
		"generated_at_utc": datetime.now(UTC).isoformat(),
		"reserved_keywords": sorted(RESERVED_KEYWORDS),
		"contextual_keywords": sorted(CONTEXTUAL_KEYWORDS),
		"alias_keywords": sorted(ALIAS_KEYWORDS),
		"base_scopes": BASE_SCOPES,
	}


@dataclass(frozen=True)
class BuiltinOptions:
	output: Path


def add_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument(
		"--output",
		type=Path,
		required=True,
		help="Candidate JSON destination; review before replacing embedded data.",
	)


def run(options: BuiltinOptions) -> None:
	catalog: Catalog = build_catalog()
	output: Path = options.output.expanduser()
	output.parent.mkdir(parents=True, exist_ok=True)
	output.write_text(
		json.dumps(catalog, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
	)
	LOGGER.info(
		"Wrote %s: %d reserved, %d contextual, %d alias keywords",
		output,
		len(catalog["reserved_keywords"]),
		len(catalog["contextual_keywords"]),
		len(catalog["alias_keywords"]),
	)
