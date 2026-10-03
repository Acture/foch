from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from eu4_analysis.__main__ import parse_args, run
from eu4_analysis.builtins import BuiltinOptions, Catalog, build_catalog


class BuiltinTests(unittest.TestCase):
	def test_catalog_merges_sources_and_counts_only_explicit_game_input(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			(root / "triggers.cwt").write_text(
				"## scope = country\nalias[trigger:owns] = int\n"
			)
			(root / "effects.cwt").write_text(
				"## scope = province\nalias[effect:add_core] = scalar\n"
			)
			(root / "effects.md").write_text(
				"Country scope\n| add_core | a | b | test note |\n"
			)
			(root / "conditions.md").write_text("owns Country` Returns true if owned\n")
			(root / "scope.md").write_text("Scopes\n")
			game: Path = root / "game"
			(game / "common").mkdir(parents=True)
			(game / "common/a.txt").write_text("owns = 1\nadd_core = 2 # owns = 3\n")
			catalog: Catalog = build_catalog(
				root,
				None,
				root / "effects.md",
				root / "conditions.md",
				root / "scope.md",
				None,
				0,
			)
			self.assertEqual(
				catalog["builtin_triggers"][0]["sources"], ["cwtools", "eu4wiki"]
			)
			self.assertEqual(
				catalog["builtin_effects"][0]["scopes"], ["country", "province"]
			)
			self.assertEqual(catalog["scan_meta"]["game_files_scanned"], 0)
			self.assertEqual(catalog["builtin_triggers"][0]["game_count"], 0)
			catalog = build_catalog(
				root,
				None,
				root / "effects.md",
				root / "conditions.md",
				root / "scope.md",
				game,
				1,
			)
			self.assertEqual(catalog["scan_meta"]["game_files_scanned"], 1)
			self.assertEqual(catalog["builtin_triggers"][0]["game_count"], 1)
			options = parse_args(
				[
					"builtins",
					"--cwtools-dir",
					str(root),
					"--wiki-effects",
					str(root / "effects.md"),
					"--wiki-conditions",
					str(root / "conditions.md"),
					"--wiki-scope",
					str(root / "scope.md"),
					"--output",
					str(root / "candidate.json"),
				]
			)
			assert isinstance(options, BuiltinOptions)
			self.assertIsNone(options.game_root)
			run(options)
			self.assertEqual(
				json.loads((root / "candidate.json").read_text())["builtin_effects"][0][
					"name"
				],
				"add_core",
			)

	def test_bad_game_directory_is_not_silently_ignored(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			root: Path = Path(directory)
			with self.assertRaisesRegex(ValueError, "missing game directory"):
				build_catalog(root, None, root, root, root, root / "missing", 0)


if __name__ == "__main__":
	unittest.main()
