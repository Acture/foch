"""Discover loader candidates first, then retain a result for every candidate."""

from __future__ import annotations

import logging
import time
from collections.abc import Callable
from dataclasses import dataclass, replace
from typing import Literal

from .families import (
	DEFAULT_BOOTSTRAP,
	BinaryAnalysis,
	Discovery,
	FamilyQuery,
	discover_family,
)
from .models import FamilyRules, Function, FunctionEvidence, ResourcePath
from .symbols import MemberName, member_name, registration_record
from .x86 import StringValue, trace

LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class CatalogEntry:
	registry: str
	loaders: tuple[Function, ...]
	observed_paths: tuple[ResourcePath, ...]
	record_candidates: tuple[str, ...]
	status: Literal["pending", "candidate", "unknown", "error"] = "pending"
	rules: FamilyRules | None = None
	error: str | None = None


@dataclass(frozen=True)
class CatalogReport:
	game_version: str
	binary_sha256: str
	bootstrap: Function | None
	entries: tuple[CatalogEntry, ...]
	scan_complete: bool
	unknowns: tuple[str, ...]
	format_version: int = 1


@dataclass(frozen=True)
class CatalogDiscovery:
	report: CatalogReport
	functions: tuple[FunctionEvidence, ...]


def loader_method(member: MemberName) -> bool:
	return "Directory" in member.method or (
		"FromFile" in member.method
		and member.method.startswith(("Init", "Load", "Read"))
	)


def inventory(
	analysis: BinaryAnalysis, bootstrap: FunctionEvidence | None
) -> tuple[CatalogEntry, ...]:
	members: dict[str, dict[int, Function]] = {}
	loaders: dict[str, dict[int, Function]] = {}
	records: dict[str, set[str]] = {}
	paths: dict[str, list[ResourcePath]] = {}
	# Database classes cover registries initialized outside the selected bootstrap.
	# Named file/directory loaders also expose owners without a Database suffix.
	for pattern in ("*DataBase*", "*Database*", "*FromDirectory*", "*FromFile*"):
		for function in analysis.find(pattern):
			for name in function.names:
				member: MemberName | None = member_name(name)
				if member is None or ".cold" in name:
					continue
				if not member.owner.lower().endswith("database") and not loader_method(
					member
				):
					continue
				members.setdefault(member.owner, {})[function.address] = function
				if loader_method(member):
					loaders.setdefault(member.owner, {})[function.address] = function
				if (record := registration_record(member)) is not None:
					records.setdefault(member.owner, set()).add(record)
	if bootstrap is not None:
		for call in trace(bootstrap).calls:
			for name in call.names:
				member = member_name(name)
				if member is None or not loader_method(member):
					continue
				members.setdefault(member.owner, {})
				for argument in call.arguments:
					if isinstance(argument, StringValue) and isinstance(
						argument.source, ResourcePath
					):
						owner_paths: list[ResourcePath] = paths.setdefault(
							member.owner, []
						)
						if argument.source not in owner_paths:
							owner_paths.append(argument.source)
	return tuple(
		CatalogEntry(
			owner,
			tuple(
				sorted(
					loaders.get(owner, {}).values(),
					key=lambda function: function.address,
				)
			),
			tuple(paths.get(owner, ())),
			tuple(sorted(records.get(owner, ()))),
		)
		for owner in sorted(members)
	)


def discover_catalog(
	analysis: BinaryAnalysis,
	binary_sha256: str,
	*,
	game_version: str,
	bootstrap_pattern: str = DEFAULT_BOOTSTRAP,
	limit: int | None = None,
	progress: Callable[[CatalogReport], None] | None = None,
) -> CatalogDiscovery:
	"""Keep unsupported/error/pending entries in the denominator, including bounded runs."""
	if not game_version.strip() or (limit is not None and limit <= 0):
		raise ValueError("game version must be non-empty and limit must be positive")
	started: float = time.perf_counter()
	bootstraps: tuple[Function, ...] = analysis.find(bootstrap_pattern)
	bootstrap: FunctionEvidence | None = (
		analysis.inspect(bootstraps[0], pseudocode=False)
		if len(bootstraps) == 1
		else None
	)
	unknowns: list[str] = [
		"Inventory covers symbol-bearing Database/DataBase classes and named file/directory loaders; it is not proof of complete EU4 subsystem coverage",
		"Observed paths are caller arguments, not verified resource-scope rules; bootstrap reachability and indirect loading remain unresolved",
	]
	if bootstrap is None:
		unknowns.append(
			f"Expected one bootstrap for {bootstrap_pattern}; found {len(bootstraps)}"
		)
	entries: list[CatalogEntry] = list(inventory(analysis, bootstrap))
	functions: dict[int, FunctionEvidence] = (
		{bootstrap.function.address: bootstrap} if bootstrap is not None else {}
	)
	report: CatalogReport = CatalogReport(
		game_version,
		binary_sha256,
		bootstrap.function if bootstrap is not None else None,
		tuple(entries),
		False,
		tuple(unknowns),
	)
	if progress is not None:
		progress(report)
	selected: list[int] = sorted(
		range(len(entries)),
		key=lambda index: (not entries[index].observed_paths, entries[index].registry),
	)
	if limit is not None:
		selected = selected[:limit]
	for position, index in enumerate(selected, start=1):
		entry: CatalogEntry = entries[index]
		LOGGER.info(
			"Analyzing %s (%d/%d selected; %d discovered)",
			entry.registry,
			position,
			len(selected),
			len(entries),
		)
		record: str | None = (
			entry.record_candidates[0] if len(entry.record_candidates) == 1 else None
		)
		try:
			result: Discovery = discover_family(
				analysis,
				binary_sha256,
				FamilyQuery(entry.registry, record, bootstrap_pattern),
				game_version=game_version,
			)
		# A batch boundary retains bounded-analysis failures and continues other registries.
		# Unexpected programming/bridge errors propagate; the last progress report survives.
		except (ValueError, RuntimeError) as error:
			entries[index] = replace(entry, status="error", error=str(error))
			LOGGER.warning("Could not analyze %s: %s", entry.registry, error)
		else:
			entries[index] = replace(
				entry, status=result.rules.status, rules=result.rules
			)
			functions.update((item.function.address, item) for item in result.functions)
		report = replace(
			report,
			entries=tuple(entries),
			scan_complete=all(item.status != "pending" for item in entries),
		)
		if progress is not None:
			progress(report)
		elapsed: float = time.perf_counter() - started
		LOGGER.info(
			"Completed %d/%d in %.1fs; estimated %.1fs remaining",
			position,
			len(selected),
			elapsed,
			elapsed / position * (len(selected) - position),
		)
	return CatalogDiscovery(report, tuple(functions.values()))
