"""Run internal maintenance workflows with python -m foch_dev."""

from __future__ import annotations

import argparse
import logging
import subprocess
from pathlib import Path

from . import compare, smoke
from .contracts import verify_repository
from .repository import find_repository
from .schema import cwt_snapshot_hash


def main(argv: list[str] | None = None) -> int:
	parser: argparse.ArgumentParser = argparse.ArgumentParser(description=__doc__)
	commands = parser.add_subparsers(dest="command", required=True)
	contracts = commands.add_parser("check", help="Verify Cargo and desktop contracts")
	contracts.add_argument("--repo", type=Path)
	schema = commands.add_parser("schema-hash", help="Print the normalized CWT digest")
	schema.add_argument("--repo", type=Path)
	schema.add_argument("--schema-root", type=Path)
	smoke.add_arguments(
		commands.add_parser("smoke", help="Run and summarize a diagnostic check")
	)
	compare.add_arguments(
		commands.add_parser("compare", help="Compare diagnostic summaries")
	)
	args: argparse.Namespace = parser.parse_args(argv)
	logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
	try:
		if args.command == "check":
			verify_repository(find_repository(args.repo))
			return 0
		if args.command == "schema-hash":
			root: Path = (
				args.schema_root
				or find_repository(args.repo) / "vendor/cwtools-eu4-config"
			)
			print(cwt_snapshot_hash(root))
			return 0
		if args.command == "smoke":
			return smoke.run(
				smoke.SmokeOptions(args.playset, args.mods, args.out_dir, args.repo)
			)
		return compare.run(
			compare.CompareOptions(
				baseline=args.baseline,
				candidate=args.candidate,
				rules=tuple(args.rules),
				gate_rule=args.gate_rule,
				min_absolute_drop=args.min_absolute_drop,
				min_relative_drop=args.min_relative_drop,
				max_top_path_share=args.max_top_path_share,
				top_path_limit=args.top_path_limit,
				allow_nonzero_exit=args.allow_nonzero_exit,
				allow_fatal_errors=args.allow_fatal_errors,
				output=args.output,
			)
		)
	except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
		parser.exit(1, f"foch-dev: {error}\n")


if __name__ == "__main__":
	raise SystemExit(main())
