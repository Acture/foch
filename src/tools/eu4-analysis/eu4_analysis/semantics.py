"""Pin the offline test model's primitives to decompiled EU4 handlers.

Reads the semantics catalog embedded in foch-test (`interpret/semantics.json`),
locates each primitive's registration in the symbol-bearing EU4 executable,
decompiles the handler, and writes a catalog whose entries carry handler
pins. The output is a drop-in replacement for the embedded file: reviewed by
a person first, exactly like the builtin catalog candidate.
"""

from __future__ import annotations

import hashlib
import json
import logging
from collections import Counter
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from . import pe
from .models import Function, FunctionEvidence
from .pe import PEImage
from .tokens import (
	GOLD_TOKENS,
	PINNED_EU4_SHA256,
	Calibration,
	DebugHandler,
	TokenTable,
	calibrate,
	debug_handlers,
	discover_token_table,
)
from .x86 import Pointer, StringValue, trace

LOGGER: logging.Logger = logging.getLogger(__name__)

DEFAULT_EFFECT_PATTERNS: tuple[str, ...] = (
	"*CEffectDatabase*",
	"*EffectDatabase*",
	"*CEffect*DataBase*",
)
DEFAULT_TRIGGER_PATTERNS: tuple[str, ...] = (
	"*CTriggerDatabase*",
	"*TriggerDatabase*",
	"*CTrigger*DataBase*",
)


class PinAnalysis(Protocol):
	"""A Ghidra session bounded to symbol lookup and function inspection.

	`image` carries the raw binary when the backend provides it, so a
	symbol-less executable can still be searched by string reference.
	"""

	image: object | None

	def find(self, pattern: str) -> tuple[Function, ...]: ...

	def at(self, address: int) -> Function: ...

	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence: ...


@dataclass(frozen=True)
class SemanticsQuery:
	"""The catalog to pin and the registry symbol patterns to search."""

	catalog: Path
	effect_patterns: tuple[str, ...] = DEFAULT_EFFECT_PATTERNS
	trigger_patterns: tuple[str, ...] = DEFAULT_TRIGGER_PATTERNS


@dataclass(frozen=True)
class SemanticsResult:
	"""The updated catalog plus what a run could not resolve."""

	catalog: dict
	missing: tuple[str, ...]


def load_catalog(path: Path) -> dict:
	catalog: dict = json.loads(path.read_text(encoding="utf-8"))
	if catalog.get("format") != 1:
		raise ValueError(f"unsupported semantics format {catalog.get('format')}")
	for name, primitive in catalog.get("primitives", {}).items():
		if primitive.get("kind") not in {"effect", "trigger"}:
			raise ValueError(
				f"primitive {name} has unknown kind {primitive.get('kind')!r}"
			)
	return catalog


def patterns_for(kind: str, query: SemanticsQuery) -> tuple[str, ...]:
	return query.effect_patterns if kind == "effect" else query.trigger_patterns


def keyword_of(argument: object) -> str | None:
	"""The keyword string an argument carries, if any.

	A direct `LEA` of a string literal traces to a plain `str`; a string
	built through the engine's CString helpers traces to a `StringValue`.
	"""
	if isinstance(argument, str):
		return argument
	if isinstance(argument, StringValue) and isinstance(argument.source, str):
		return argument.source
	return None


def locate(
	analysis: PinAnalysis, name: str, patterns: tuple[str, ...]
) -> Function | None:
	"""The handler registered under `name`, or None.

	On a PE image the static registration tables name the handler directly;
	otherwise a registration call passes the keyword string and the handler
	together, and the handler appears as a plain address argument of the
	same call.
	"""
	image = getattr(analysis, "image", None)
	if isinstance(image, pe.PEImage):
		for begin in pe.table_handlers(image, name):
			# The table entry is the evidence: the handler itself.
			return Function(begin, image.function_end(begin), (f"sub_{begin:x}",))
	for function in registration_functions(analysis, name, patterns):
		evidence = analysis.inspect(function, pseudocode=False)
		for call in trace(evidence).calls:
			if not any(keyword_of(argument) == name for argument in call.arguments):
				continue
			for argument in call.arguments:
				if (
					isinstance(argument, Pointer)
					and argument.base.startswith("address:")
					and argument.offset == 0
				):
					address = int(argument.base.split(":", 1)[1], 16)
					return analysis.at(address)
	return None


def registration_functions(
	analysis: PinAnalysis, name: str, patterns: tuple[str, ...]
) -> tuple[Function, ...]:
	"""Functions that may register `name`.

	With symbols (the macOS image) these are the symbol-pattern hits; a
	stripped Windows image instead yields every function whose code
	references the keyword string, discovered through `.pdata` ranges and
	RIP-relative `lea` scanning.
	"""
	symbolic = tuple(
		function for pattern in patterns for function in analysis.find(pattern)
	)
	if symbolic:
		return symbolic
	image = getattr(analysis, "image", None)
	if isinstance(image, pe.PEImage):
		# The static registration tables hand the handler over directly;
		# code-reference tracing is the fallback. Both return loaded-image
		# addresses, the space Ghidra addresses use.
		handlers = pe.table_handlers(image, name)
		if handlers:
			return tuple(
				Function(begin, image.function_end(begin), (f"sub_{begin:x}",))
				for begin in handlers
			)
		return tuple(
			Function(begin, end, (f"sub_{begin:x}",))
			for begin, end in pe.keyword_reference_functions(image, name)
		)
	return ()


def pin(analysis: PinAnalysis, function: Function) -> dict:
	evidence: FunctionEvidence = analysis.inspect(function)
	if evidence.pseudocode is None:
		raise ValueError(f"no pseudocode for handler {function.address:#x}")
	symbol: str = function.names[0] if function.names else f"sub_{function.address:x}"
	return {
		"symbol": symbol,
		"address": function.address,
		"end": function.end,
		"pseudocode_sha256": hashlib.sha256(evidence.pseudocode.encode()).hexdigest(),
	}


def pin_catalog(
	analysis: PinAnalysis,
	query: SemanticsQuery,
	*,
	game_version: str,
	binary_sha256: str,
) -> SemanticsResult:
	catalog: dict = load_catalog(query.catalog)
	missing: list[str] = []
	for name, primitive in sorted(catalog["primitives"].items()):
		try:
			handler = locate(analysis, name, patterns_for(primitive["kind"], query))
		except (ValueError, RuntimeError) as error:
			# A batch boundary retains bounded-analysis failures and
			# continues with the other primitives.
			missing.append(name)
			LOGGER.warning("Could not analyze %s: %s", name, error)
			continue
		if handler is None:
			missing.append(name)
			LOGGER.warning("No handler found for %s (%s)", name, primitive["kind"])
			continue
		try:
			primitive["handler"] = pin(analysis, handler)
			LOGGER.info("Pinned %s to %#x", name, handler.address)
		except (ValueError, RuntimeError) as error:
			# A batch boundary retains bounded-analysis failures and
			# continues with the other primitives.
			missing.append(name)
			LOGGER.warning("Could not analyze %s: %s", name, error)
	if missing:
		catalog["status"] = "unpinned"
	else:
		catalog["status"] = "pinned"
		catalog["game_version"] = game_version
		catalog["binary_sha256"] = binary_sha256
	# Unresolved names ride along in the artifact; foch-test ignores extra keys.
	catalog["missing"] = missing
	return SemanticsResult(catalog, tuple(missing))


def _handler_record(handler: DebugHandler, image_base: int) -> dict[str, object]:
	return {
		"address": handler.address,
		"debug_string": handler.string,
		"debug_string_rva": handler.string_rva,
		"end": handler.end,
		"evidence": "debug_string",
		"lea_rva": handler.lea_rva,
		"prologue": handler.prologue,
		"prologue_hex": handler.prologue_hex,
		"rva": handler.address - image_base,
		"symbol": f"sub_{handler.address:x}",
	}


def _token_field(table: TokenTable | None, name: str, trusted: bool) -> int | None:
	if not trusted or table is None:
		return None
	return table.id_of(name)


def _treatment(
	name: str,
	kind: str,
	table: TokenTable | None,
	trusted: bool,
	handlers: dict[str, DebugHandler],
	ambiguous: frozenset[str],
	image_base: int,
) -> tuple[str, str | None, int | None, dict[str, object] | None]:
	"""Status, failure, token id, and handler record.

	A debug-string function is a handler pin. A calibrated token id without
	that function stays ``token_only`` and is never reported as pinned.
	"""
	handler = handlers.get(name)
	if handler is not None and handler.kind == kind:
		return (
			"pinned",
			None,
			_token_field(table, name, trusted),
			_handler_record(handler, image_base),
		)
	if not trusted:
		failure = "token_table_absent" if table is None else "token_table_uncalibrated"
		return "unpinned", failure, None, None
	assert table is not None
	ids = table.ids_of(name)
	if len(ids) > 1:
		return "unpinned", "token_ambiguous", None, None
	if len(ids) == 0:
		return "unpinned", "token_absent", None, None
	if name in ambiguous:
		return "token_only", "handler_ambiguous", ids[0], None
	if handler is not None and handler.kind != kind:
		return "token_only", "kind_mismatch", ids[0], None
	return "token_only", "handler_unproven", ids[0], None


def _calibration_record(result: Calibration) -> dict[str, object]:
	return {
		"checks": [
			{
				"expected": expected,
				"found": found,
				"match": found == expected,
				"name": name,
			}
			for name, expected, found in result.checks
		],
		"matched": result.matched,
		"passed": result.passed,
		"total": result.total,
	}


def _table_record(table: TokenTable | None) -> dict[str, object] | None:
	if table is None:
		return None
	return {
		"first_rva": table.first_rva,
		"id_width": table.id_width,
		"last_rva": table.last_rva,
		"records": len(table.entries),
		"section": table.section,
		"stride": table.stride,
	}


def review_catalog(
	image: PEImage,
	catalog: dict,
	*,
	game_version: str,
	gold: Mapping[str, int] | None = None,
) -> dict:
	"""Baseline string scan versus token-table plus debug-string handlers.

	The catalog's embedded handler pins are not copied. This report is the
	read-only evidence for one executable.
	"""
	if catalog.get("format") != 1:
		raise ValueError(f"unsupported semantics format {catalog.get('format')}")
	expected = GOLD_TOKENS if gold is None else gold
	table = discover_token_table(image)
	checked = calibrate(table, expected)
	handlers, ambiguous = debug_handlers(image)
	primitives: dict[str, dict[str, object]] = {}
	baseline_counts: Counter[str] = Counter()
	treatment_counts: Counter[str] = Counter()
	failure_counts: Counter[str] = Counter()
	for name, primitive in catalog["primitives"].items():
		kind = primitive.get("kind")
		if kind not in {"effect", "trigger"}:
			raise ValueError(f"primitive {name} has unknown kind {kind!r}")
		baseline, consoles = pe.baseline_observation(image, name)
		status, failure, token_id, handler = _treatment(
			name,
			kind,
			table,
			checked.passed,
			handlers,
			ambiguous,
			image.image_base,
		)
		baseline_counts[baseline] += 1
		treatment_counts[status] += 1
		if failure is not None:
			failure_counts[failure] += 1
		record: dict[str, object] = {
			"baseline": baseline,
			"console_handlers": list(consoles),
			"failure": failure,
			"handler": handler,
			"kind": kind,
			"token_id": token_id,
			"treatment": status,
		}
		primitives[name] = record
	total = len(primitives)
	if (
		sum(treatment_counts.values()) != total
		or sum(baseline_counts.values()) != total
	):
		raise RuntimeError("review counts do not cover every primitive")
	return {
		"binary_sha256": image.sha256,
		"calibration": _calibration_record(checked),
		"comparison": {
			"baseline": {
				"ambiguous": baseline_counts["ambiguous"],
				"no_handler": baseline_counts["no_handler"],
				"pinned": baseline_counts["pinned"],
				"rejected_console_cmd": baseline_counts["rejected_console_cmd"],
				"string_absent": baseline_counts["string_absent"],
			},
			"failures": dict(sorted(failure_counts.items())),
			"total": total,
			"treatment": {
				"pinned": treatment_counts["pinned"],
				"token_only": treatment_counts["token_only"],
				"unpinned": treatment_counts["unpinned"],
			},
		},
		"format": 1,
		"game_version": game_version,
		"kind": "semantics-pin-review",
		"pinned_binary_match": image.sha256 == PINNED_EU4_SHA256,
		"pinned_binary_sha256": PINNED_EU4_SHA256,
		"primitives": primitives,
		"token_table": _table_record(table),
	}
