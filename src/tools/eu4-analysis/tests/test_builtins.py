from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from eu4_analysis.__main__ import parse_args, run
from eu4_analysis.builtins import BuiltinOptions, Catalog, build_catalog

EMBEDDED_CATALOG: Path = (
	Path(__file__).resolve().parents[4]
	/ "src/packages/foch/src/game/eu4/base/data/eu4_builtin_catalog.json"
)


class BuiltinTests(unittest.TestCase):
	def test_catalog_matches_the_embedded_keywords_and_scopes(self) -> None:
		catalog: Catalog = build_catalog()
		embedded: dict[str, object] = json.loads(
			EMBEDDED_CATALOG.read_text(encoding="utf-8")
		)
		for key in (
			"reserved_keywords",
			"contextual_keywords",
			"alias_keywords",
			"base_scopes",
		):
			self.assertEqual(catalog[key], embedded[key], key)

	def test_builtins_command_writes_only_the_catalog(self) -> None:
		with tempfile.TemporaryDirectory() as directory:
			output: Path = Path(directory) / "nested/candidate.json"
			options = parse_args(["builtins", "--output", str(output)])
			assert isinstance(options, BuiltinOptions)
			run(options)
			self.assertEqual(
				set(json.loads(output.read_text(encoding="utf-8"))),
				{
					"version",
					"generated_at_utc",
					"reserved_keywords",
					"contextual_keywords",
					"alias_keywords",
					"base_scopes",
				},
			)


if __name__ == "__main__":
	unittest.main()
