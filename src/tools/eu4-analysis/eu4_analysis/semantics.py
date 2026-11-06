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
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from . import pe
from .models import Function, FunctionEvidence
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
			raise ValueError(f"primitive {name} has unknown kind {primitive.get('kind')!r}")
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


def locate(analysis: PinAnalysis, name: str, patterns: tuple[str, ...]) -> Function | None:
	"""The handler registered under `name`, or None.

	A registration call passes the keyword string and the handler together;
	the handler appears as a plain address argument of the same call.
	"""
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
		return tuple(
			Function(begin, end, (f"sub_{begin:x}",)) for begin, end in pe.keyword_reference_functions(image, name)
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
	analysis: PinAnalysis, query: SemanticsQuery, *, game_version: str, binary_sha256: str
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
		primitive["handler"] = pin(analysis, handler)
		LOGGER.info("Pinned %s to %#x", name, handler.address)
	if missing:
		catalog["status"] = "unpinned"
	else:
		catalog["status"] = "pinned"
		catalog["game_version"] = game_version
		catalog["binary_sha256"] = binary_sha256
	# Unresolved names ride along in the artifact; foch-test ignores extra keys.
	catalog["missing"] = missing
	return SemanticsResult(catalog, tuple(missing))
