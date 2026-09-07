from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol

from .models import (
	DefinitionIdentity,
	Evidence,
	FamilyRules,
	Function,
	FunctionEvidence,
	ResourcePath,
	ResourceScope,
)
from .x86 import Pointer, Registry, StringValue, has_name, trace

DEFAULT_BOOTSTRAP: str = "*CEU4Application*LoadDatabasesEv"


class BinaryAnalysis(Protocol):
	def find(self, pattern: str) -> tuple[Function, ...]: ...
	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence: ...


@dataclass(frozen=True)
class FamilyQuery:
	registry: str
	record: str | None = None
	bootstrap: str = DEFAULT_BOOTSTRAP


@dataclass(frozen=True)
class Discovery:
	rules: FamilyRules
	functions: tuple[FunctionEvidence, ...]


def discover_family(
	analysis: BinaryAnalysis,
	binary_sha256: str,
	query: FamilyQuery,
	*,
	game_version: str,
	pseudocode: bool = False,
) -> Discovery:
	"""Collect, trace arguments/keys, and return two conservative candidate rules."""
	if not game_version.strip() or not query.registry:
		raise ValueError("game version and registry are required")
	functions: dict[int, FunctionEvidence] = {}
	evidence: list[Evidence] = []
	unknowns: list[str] = []

	def inspect(function: Function, pseudocode: bool = pseudocode) -> FunctionEvidence:
		if function.address not in functions:
			functions[function.address] = analysis.inspect(
				function, pseudocode=pseudocode
			)
		return functions[function.address]

	def select(pattern: str, pseudocode: bool = pseudocode) -> FunctionEvidence | None:
		matches: tuple[Function, ...] = tuple(
			function
			for function in analysis.find(pattern)
			if any(".cold" not in name for name in function.names)
		)
		if len(matches) != 1:
			unknowns.append(
				f"Expected one function for {pattern}; found {len(matches)}"
			)
			return None
		return inspect(matches[0], pseudocode)

	bootstrap = select(query.bootstrap, False)
	loader = select(f"*{query.registry}*InitFromDirectory*")
	writer = select(f"*{query.registry}*AppendFromFile*")
	lookup = select(f"*{query.registry}*GetByKey*") if query.record else None
	paths: list[ResourcePath] = []
	file_filters: set[str] = set()
	reader_receivers: list[bool] = []
	if bootstrap is not None and loader is not None:
		for call in trace(bootstrap, query.registry).calls:
			if has_name(call.names, query.registry, "InitFromDirectory") and isinstance(
				call.arguments[0], Registry
			):
				argument = call.arguments[1]
				if isinstance(argument, StringValue) and isinstance(
					argument.source, ResourcePath
				):
					if argument.source not in paths:
						paths.append(argument.source)
					evidence.append(
						Evidence(
							"bootstrap",
							call.address,
							f"Directory argument {argument.source} passed to {query.registry} accessor result",
						)
					)
	if loader is not None and writer is not None:
		for call in trace(loader, query.registry).calls:
			if has_name(call.names, "VFSGetEnumeratedFiles") and isinstance(
				call.arguments[2], str
			):
				file_filters.add(call.arguments[2])
				evidence.append(
					Evidence(
						"loader",
						call.address,
						f"VFS enumeration filter: {call.arguments[2]}",
					)
				)
			if has_name(call.names, query.registry, "AppendFromFile"):
				shared_receiver: bool = call.arguments[0] == Pointer("arg0")
				reader_receivers.append(shared_receiver)
				if shared_receiver:
					evidence.append(
						Evidence(
							"loader",
							call.address,
							"Per-file reader is passed to the loader's original registry receiver",
						)
					)
	file_filter: str | None = (
		next(iter(file_filters)) if len(file_filters) == 1 else None
	)
	scope: ResourceScope | None = (
		ResourceScope(query.registry, tuple(paths), file_filter)
		if paths and file_filter and reader_receivers and all(reader_receivers)
		else None
	)
	if scope is None:
		unknowns.append(
			"Resource paths, file filter, or shared registry receiver were not established"
		)

	identity: DefinitionIdentity | None = None
	# GetName can return a display name; trace the hash/equality key instead.
	adds: tuple[Function, ...] = (
		analysis.find(f"*CHashTable*{query.record}*3Add*") if query.record else ()
	)
	finds: tuple[Function, ...] = (
		analysis.find(f"*CHashTable*{query.record}*4Find*") if query.record else ()
	)
	if (
		query.record is not None
		and len(adds) == 1
		and len(finds) == 1
		and lookup is not None
		and writer is not None
	):
		add = inspect(adds[0])
		find = inspect(finds[0])
		add_offsets: set[int] = {
			call.arguments[0].offset
			for call in trace(add, query.registry).calls
			if has_name(call.names, "CString", "GetHashValue")
			and isinstance(call.arguments[0], Pointer)
			and call.arguments[0].base == "arg1"
		}
		compared_offsets: set[int] = {
			call.arguments[0].offset
			for call in trace(find, query.registry).calls
			if has_name(call.names, "7CStringeq")
			and isinstance(call.arguments[0], Pointer)
			and call.arguments[0].base.startswith("*")
			and call.arguments[1] == Pointer("arg1")
		}
		lookup_calls_find: bool = any(
			instruction.call is not None
			and instruction.call.address == find.function.address
			for instruction in lookup.instructions
		)
		registration = select(f"*{query.registry}*Add{query.record.removeprefix('C')}*")
		writer_calls_registration: bool = registration is not None and any(
			instruction.call is not None
			and instruction.call.address == registration.function.address
			for instruction in writer.instructions
		)
		registration_calls_add: bool = registration is not None and any(
			instruction.call is not None
			and instruction.call.address == add.function.address
			for instruction in registration.instructions
		)
		offsets: set[int] = add_offsets & compared_offsets
		if (
			len(add_offsets) == len(compared_offsets) == len(offsets) == 1
			and lookup_calls_find
			and writer_calls_registration
			and registration_calls_add
		):
			offset: int = next(iter(offsets))
			identity = DefinitionIdentity(query.registry, offset, "CString", None)
			evidence.extend(
				(
					Evidence(
						"hash_add",
						add.function.address,
						f"Registration hashes the object's CString member at +{offset:#x}",
					),
					Evidence(
						"hash_find",
						find.function.address,
						"Lookup compares the same member against the query string",
					),
					Evidence(
						"lookup",
						lookup.function.address,
						"Registry GetByKey calls this hash-table Find",
					),
				)
			)
	if identity is None:
		unknowns.append(
			"An unambiguous registration/hash/equality/query chain was not established"
		)
	else:
		unknowns.append(
			"Mapping the binary key member back to a source-script field still requires review"
		)
	if paths:
		unknowns.append(
			"Directory enum meanings, VFS recursion/order and branch reachability are not resolved; scope is limited to the selected bootstrap"
		)
	return Discovery(
		FamilyRules(
			binary_sha256,
			game_version,
			query.registry,
			scope,
			identity,
			"candidate" if scope is not None or identity is not None else "unknown",
			tuple(evidence),
			tuple(unknowns),
		),
		tuple(functions.values()),
	)
