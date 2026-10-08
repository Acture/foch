"""Run internal maintenance workflows with python -m foch_dev."""

from __future__ import annotations

import argparse
import logging
import subprocess
from pathlib import Path

from . import compare, crates, dist, release, smoke, versions, winget
from .contracts import verify_repository
from .repository import find_repository
from .schema import SCHEMA_DIR, cwt_snapshot_hash


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
	crates.add_arguments(
		commands.add_parser(
			"crate-smoke",
			help="Package the publishable crates and install foch from them out of tree",
		)
	)
	versions.add_arguments(
		commands.add_parser(
			"version", help="Print the release tag and per-channel version spellings"
		)
	)
	dist.add_arguments(
		commands.add_parser("dist", help="Build and verify release wheels and archives")
	)
	winget.add_arguments(
		commands.add_parser(
			"winget", help="Render and check the WinGet manifests for Acture.Foch"
		)
	)
	release.add_arguments(
		commands.add_parser(
			"release",
			help="Check a release tag against every distribution channel (read-only)",
		)
	)
	args: argparse.Namespace = parser.parse_args(argv)
	logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
	try:
		if args.command == "check":
			verify_repository(find_repository(args.repo))
			return 0
		if args.command == "schema-hash":
			root: Path = args.schema_root or find_repository(args.repo) / SCHEMA_DIR
			print(cwt_snapshot_hash(root))
			return 0
		if args.command == "crate-smoke":
			return crates.run(
				crates.CrateSmokeOptions(
					args.repo, args.work_dir, args.offline, args.allow_dirty
				)
			)
		if args.command == "version":
			return versions.run(args)
		if args.command == "dist":
			return dist.run(args)
		if args.command == "winget":
			return winget.run(args)
		if args.command == "release":
			return release.run(args)
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
	except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
		parser.exit(1, f"foch-dev: {error}\n")


if __name__ == "__main__":
	raise SystemExit(main())
