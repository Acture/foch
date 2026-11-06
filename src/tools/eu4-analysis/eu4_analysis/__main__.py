"""Discover EU4 loading rules, inspect functions, or build a builtin catalog."""

from __future__ import annotations

import argparse
import json
import logging
import os
import re
import tempfile
import time
from dataclasses import asdict, dataclass
from pathlib import Path

from . import builtins
from .builtins import BuiltinOptions
from .catalog import CatalogDiscovery, CatalogReport, discover_catalog
from .families import DEFAULT_BOOTSTRAP, Discovery, FamilyQuery, discover_family
from .ghidra import GhidraSession, validate_output_directory
from .load_rules import LoadRules, discover_load_rules
from .models import Function, FunctionEvidence
from .semantics import (
	DEFAULT_EFFECT_PATTERNS,
	DEFAULT_TRIGGER_PATTERNS,
	SemanticsQuery,
	pin_catalog,
)

LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class CatalogQuery:
	bootstrap: str
	limit: int | None


@dataclass(frozen=True)
class Options:
	binary: Path
	installation: Path
	workspace: Path
	game_version: str
	timeout: int
	query: CatalogQuery | FamilyQuery | str
	rules_directory: Path | None


@dataclass(frozen=True)
class SemanticsOptions:
	binary: Path
	installation: Path
	workspace: Path
	game_version: str
	timeout: int
	query: SemanticsQuery
	output: Path


def label(value: str) -> str:
	if re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", value) is None:
		raise argparse.ArgumentTypeError("use a non-empty alphanumeric version or name")
	return value


def parse_args(argv: list[str] | None = None) -> Options | BuiltinOptions:
	parser: argparse.ArgumentParser = argparse.ArgumentParser(description=__doc__)
	commands = parser.add_subparsers(dest="command", required=True)
	discover = commands.add_parser(
		"discover", help="Discover all loader candidates and infer family rules"
	)
	builtins.add_arguments(
		commands.add_parser(
			"builtins",
			help="Write the builtin keyword catalog",
		)
	)
	inspect = commands.add_parser("inspect", help="Export one function and its calls")
	semantics = commands.add_parser(
		"semantics",
		help="Pin the offline model's primitives to decompiled handlers",
	)
	for command in (discover, inspect, semantics):
		command.add_argument("--binary", required=True, type=Path)
		command.add_argument("--game-version", required=True, type=label)
		command.add_argument(
			"--ghidra", type=Path, default=os.environ.get("GHIDRA_INSTALL_DIR")
		)
		command.add_argument(
			"--workspace",
			type=Path,
			default=Path(__file__).resolve().parents[4] / "target" / "eu4-analysis",
		)
		command.add_argument("--timeout", type=int, default=45)
	discover.add_argument(
		"--registry", type=label, help="Optionally restrict analysis to one registry"
	)
	discover.add_argument(
		"--record", type=label, help="Record type for a single-registry identity probe"
	)
	discover.add_argument("--bootstrap", default=DEFAULT_BOOTSTRAP)
	discover.add_argument(
		"--rules-dir",
		type=Path,
		default=Path(__file__).resolve().parents[4]
		/ "src/packages/foch/src/game/eu4/content/rules",
		help="Versioned database loading rules (default: src/packages/foch/src/game/eu4/content/rules)",
	)
	discover.add_argument(
		"--limit",
		type=int,
		help="Bound a batch probe; other inventory entries remain pending",
	)
	inspect.add_argument(
		"--symbol", required=True, help="Glob matching one function address"
	)
	semantics.add_argument(
		"--catalog",
		required=True,
		type=Path,
		help="The foch-test semantics.json to read and replace",
	)
	semantics.add_argument(
		"--output",
		required=True,
		type=Path,
		help="Candidate pinned catalog destination; review before replacing the embedded file",
	)
	semantics.add_argument(
		"--effect-registry",
		action="append",
		metavar="GLOB",
		help="Symbol pattern of an effect registry (repeatable; default: %(default)s)",
		default=list(DEFAULT_EFFECT_PATTERNS),
	)
	semantics.add_argument(
		"--trigger-registry",
		action="append",
		metavar="GLOB",
		help="Symbol pattern of a trigger registry (repeatable; default: %(default)s)",
		default=list(DEFAULT_TRIGGER_PATTERNS),
	)
	args: argparse.Namespace = parser.parse_args(argv)
	if args.command == "builtins":
		return BuiltinOptions(args.output)
	if args.ghidra is None:
		parser.error("set --ghidra or GHIDRA_INSTALL_DIR")
	if args.timeout <= 0:
		parser.error("--timeout must be positive")
	if args.command == "semantics":
		return SemanticsOptions(
			args.binary.resolve(),
			args.ghidra.resolve(),
			args.workspace.resolve(),
			args.game_version,
			args.timeout,
			SemanticsQuery(
				args.catalog.resolve(),
				tuple(args.effect_registry),
				tuple(args.trigger_registry),
			),
			args.output.resolve(),
		)
	query: CatalogQuery | FamilyQuery | str
	if args.command == "discover":
		if args.record and not args.registry:
			parser.error("--record requires --registry; omit both for batch discovery")
		if args.limit is not None and (args.limit <= 0 or args.registry):
			parser.error(
				"--limit must be positive and is only used for batch discovery"
			)
		query = (
			FamilyQuery(args.registry, args.record, args.bootstrap)
			if args.registry
			else CatalogQuery(args.bootstrap, args.limit)
		)
	else:
		query = args.symbol
	return Options(
		args.binary.resolve(),
		args.ghidra.resolve(),
		args.workspace.resolve(),
		args.game_version,
		args.timeout,
		query,
		args.rules_dir.resolve() if args.command == "discover" else None,
	)


def write_json(path: Path, value: object) -> None:
	path.parent.mkdir(parents=True, exist_ok=True)
	with tempfile.NamedTemporaryFile(
		mode="w",
		encoding="utf-8",
		dir=path.parent,
		prefix=f".{path.name}.",
		delete=False,
	) as temporary:
		temporary_path: Path = Path(temporary.name)
		try:
			temporary.write(json.dumps(value, indent=2, sort_keys=True) + "\n")
			temporary.flush()
			# Windows scanners briefly lock freshly created files; retry the
			# atomic replace and fall back to a plain write.
			for attempt in range(10):
				try:
					temporary_path.replace(path)
					break
				except PermissionError:
					if attempt == 9:
						path.write_text(
							json.dumps(value, indent=2, sort_keys=True) + "\n",
							encoding="utf-8",
						)
					else:
						time.sleep(0.2 * (attempt + 1))
		finally:
			try:
				temporary_path.unlink(missing_ok=True)
			except PermissionError:
				pass
	LOGGER.info("Wrote %s", path)


def run(options: Options | BuiltinOptions | SemanticsOptions) -> None:
	if isinstance(options, BuiltinOptions):
		builtins.run(options)
		return
	if isinstance(options, SemanticsOptions):
		with GhidraSession(
			options.binary, options.installation, options.workspace, options.timeout
		) as analysis:
			result = pin_catalog(
				analysis,
				options.query,
				game_version=options.game_version,
				binary_sha256=analysis.image.sha256,
			)
			write_json(options.output, result.catalog)
			LOGGER.info(
				"Semantics: %d primitives; %d missing handlers",
				len(result.catalog["primitives"]),
				len(result.missing),
			)
		return
	if options.rules_directory is not None:
		validate_output_directory(
			options.binary, options.installation, options.rules_directory
		)
	with GhidraSession(
		options.binary, options.installation, options.workspace, options.timeout
	) as analysis:
		directory: Path = (
			options.workspace / "exports" / options.game_version / analysis.image.sha256
		)
		if isinstance(options.query, CatalogQuery):

			def progress(report: CatalogReport) -> None:
				write_json(directory / "catalog.json", asdict(report))

			catalog: CatalogDiscovery = discover_catalog(
				analysis,
				analysis.image.sha256,
				game_version=options.game_version,
				bootstrap_pattern=options.query.bootstrap,
				limit=options.query.limit,
				progress=progress,
			)
			for item in catalog.functions:
				write_json(
					directory / "functions" / f"{item.function.address:x}.json",
					asdict(item),
				)
			if catalog.report.scan_complete:
				assert options.rules_directory is not None
				rules: LoadRules = discover_load_rules(analysis, catalog)
				write_json(
					options.rules_directory / f"{options.game_version}.json",
					asdict(rules),
				)
				LOGGER.info(
					"Wrote loading rules for %d databases", len(rules.databases)
				)
			LOGGER.info(
				"Catalog: %d discovered; %d candidates; %d unknown; %d errors; %d pending",
				len(catalog.report.entries),
				sum(item.status == "candidate" for item in catalog.report.entries),
				sum(item.status == "unknown" for item in catalog.report.entries),
				sum(item.status == "error" for item in catalog.report.entries),
				sum(item.status == "pending" for item in catalog.report.entries),
			)
		elif isinstance(options.query, FamilyQuery):
			result: Discovery = discover_family(
				analysis,
				analysis.image.sha256,
				options.query,
				game_version=options.game_version,
			)
			directory /= options.query.registry
			write_json(directory / "rules.json", asdict(result.rules))
			write_json(
				directory / "evidence.json", [asdict(item) for item in result.functions]
			)
			LOGGER.info(
				"Result: %s; %d unresolved items",
				result.rules.status,
				len(result.rules.unknowns),
			)
		else:
			functions: tuple[Function, ...] = analysis.find(options.query)
			if len(functions) != 1:
				raise ValueError(
					f"symbol must match one function address; found {len(functions)}"
				)
			function: FunctionEvidence = analysis.inspect(functions[0])
			write_json(
				directory / f"{function.function.address:x}.json",
				{
					"binary_sha256": analysis.image.sha256,
					"game_version": options.game_version,
					"function": asdict(function),
				},
			)


def main() -> None:
	logging.basicConfig(
		level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
	)
	run(parse_args())


if __name__ == "__main__":
	main()
