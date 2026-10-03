"""The public rule artifact: database -> directory and filename pattern."""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass
from pathlib import PurePosixPath

from .catalog import CatalogDiscovery
from .families import BinaryAnalysis
from .models import Function, FunctionEvidence, ResourcePath
from .symbols import member_name
from .x86 import Pointer, StringValue, has_name, trace

LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class FileSelection:
	directory: str
	files: str


@dataclass(frozen=True)
class LoadRules:
	game_version: str
	binary_sha256: str
	databases: dict[str, tuple[FileSelection, ...]]


def directory_roots(
	constructor: FunctionEvidence, getter: FunctionEvidence
) -> dict[str, str]:
	"""Recover the enum layout from the getter and literal assignments in the constructor."""

	def member_offset(index: int) -> int:
		offsets: set[int] = {
			call.arguments[1].offset
			for call in trace(
				getter, arguments=(Pointer("arg0"), Pointer("arg1"), index)
			).calls
			if any(re.search(r"7CStringC[12]ERKS_", name) for name in call.names)
			and isinstance(call.arguments[1], Pointer)
			and call.arguments[1].base == "arg1"
		}
		if len(offsets) != 1:
			raise ValueError("directory getter does not identify one string member")
		return offsets.pop()

	start: int = member_offset(0)
	stride: int = member_offset(1) - start
	if stride <= 0 or member_offset(2) != start + 2 * stride:
		raise ValueError("directory getter is not a supported fixed-stride lookup")
	roots: dict[str, str] = {}
	for call in trace(constructor).calls:
		destination, value = call.arguments[:2]
		if not (
			has_name(call.names, "basic_string", "6assignEPKc")
			and isinstance(destination, Pointer)
			and destination.base == "arg0"
			and isinstance(value, str)
		):
			continue
		delta: int = destination.offset - start
		if delta < 0 or delta % stride != 0:
			continue
		index: int = delta // stride
		if member_offset(index) != destination.offset:
			continue
		path: PurePosixPath = PurePosixPath(value)
		if path.is_absolute() or ".." in path.parts or not value:
			raise ValueError(f"unsupported directory root: {value}")
		key: str = f"CDirectorySettings::GetOriginalDirectory({index})"
		if key in roots and roots[key] != value:
			raise ValueError(f"conflicting directory assignments for {key}")
		roots[key] = value
	if not roots:
		raise ValueError("no directory roots could be recovered")
	return roots


def discover_load_rules(
	analysis: BinaryAnalysis, catalog: CatalogDiscovery
) -> LoadRules:
	if not catalog.report.scan_complete:
		raise ValueError("a partial scan must not replace the repository's rule file")

	def inspect(pattern: str) -> FunctionEvidence:
		functions: tuple[Function, ...] = analysis.find(pattern)
		if len(functions) != 1:
			raise ValueError(
				f"expected one function for {pattern}; found {len(functions)}"
			)
		return analysis.inspect(functions[0], pseudocode=False)

	roots: dict[str, str] = directory_roots(
		inspect("__ZN18CDirectorySettingsC2Ev"),
		inspect("*CDirectorySettings*GetOriginalDirectory*"),
	)
	LOGGER.info("Recovered %d directory roots", len(roots))
	if catalog.report.bootstrap is None:
		raise ValueError("no bootstrap was identified for directory argument tracing")
	bootstrap: FunctionEvidence = analysis.inspect(
		catalog.report.bootstrap, pseudocode=False
	)
	known: dict[int, FunctionEvidence] = {
		item.function.address: item for item in catalog.functions
	}
	loaders: dict[int, Function] = {
		function.address: function
		for entry in catalog.report.entries
		for function in entry.loaders
	}
	targets: dict[int, int] = {
		item.address: item.call.address
		for item in bootstrap.instructions
		if item.call is not None
	}
	selected: dict[str, set[FileSelection]] = {}
	for call in trace(bootstrap).calls:
		loader: Function | None = loaders.get(targets.get(call.address, -1))
		if loader is None:
			continue
		owners: set[str] = {
			member.owner
			for name in call.names
			if (member := member_name(name)) is not None
		}
		paths: set[ResourcePath] = {
			arg.source
			for arg in call.arguments
			if isinstance(arg, StringValue) and isinstance(arg.source, ResourcePath)
		}
		if len(owners) != 1 or len(paths) != 1:
			continue
		owner: str = next(iter(owners))
		path: ResourcePath = next(iter(paths))
		if path.base not in roots:
			LOGGER.info("Unresolved directory root for %s: %s", owner, path.base)
			continue
		if loader.address not in known:
			known[loader.address] = analysis.inspect(loader, pseudocode=False)
		enumerations = tuple(
			item
			for item in trace(known[loader.address]).calls
			if has_name(item.names, "VFSGetEnumeratedFiles")
			or has_name(item.names, "VFSEnumerateFiles")
		)
		filters: set[str] = {
			item.arguments[2]
			for item in enumerations
			if isinstance(item.arguments[2], str)
		}
		if len(filters) != 1 or any(
			not isinstance(item.arguments[2], str) for item in enumerations
		):
			LOGGER.info("Unresolved file filter for %s via %#x", owner, loader.address)
			continue
		extension: str = next(iter(filters))
		if re.fullmatch(r"\.[A-Za-z0-9]+", extension) is None:
			LOGGER.info("Unsupported file filter for %s: %s", owner, extension)
			continue
		directory: PurePosixPath = PurePosixPath(roots[path.base]) / path.suffix.lstrip(
			"/"
		)
		if ".." in directory.parts:
			raise ValueError(f"directory escapes the game root: {directory}")
		selected.setdefault(owner, set()).add(
			FileSelection(str(directory), f"*{extension}")
		)
	return LoadRules(
		catalog.report.game_version,
		catalog.report.binary_sha256,
		{
			owner: tuple(
				sorted(
					selections,
					key=lambda selection: (selection.directory, selection.files),
				)
			)
			for owner, selections in sorted(selected.items())
		},
	)
