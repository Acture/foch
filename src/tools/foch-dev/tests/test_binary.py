from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from foch_dev.binary import FochIdentity, scratch_environment, verify_foch

SCHEMA_ID: str = "5d636ca3ec1497a27308b712b2601fef9cb8993a14d777824486c6930a7070f3"
IDENTITY: FochIdentity = FochIdentity("0.1.0-rc.1", SCHEMA_ID)
FAKE_FOCH: str = """\
import sys
from pathlib import Path

# Like the Rust program, write UTF-8 to the pipe whatever the locale says.
sys.stdout.reconfigure(encoding="utf-8")
arguments = sys.argv[1:]
if arguments == ["--version"]:
	sys.stdout.write({version_output!r})
elif arguments == ["--help"]:
	print("Usage: foch <COMMAND>")
elif arguments[:2] == ["input", "inspect"]:
	manifest = Path(arguments[2])
	print("game: eu4")
	print("local_patch", manifest.parent / "local_patch", end="{inspect_suffix}\\n")
else:
	sys.exit({unknown_exit})
"""


def fake_foch(
	directory: Path,
	*,
	version_output: str,
	unknown_exit: int = 2,
	inspect_suffix: str = "",
) -> list[str]:
	"""A stand-in `foch` command that answers the installation checks."""
	script: Path = directory / "fake_foch.py"
	script.write_text(
		FAKE_FOCH.format(
			version_output=version_output,
			unknown_exit=unknown_exit,
			inspect_suffix=inspect_suffix,
		),
		encoding="utf-8",
	)
	return [sys.executable, str(script)]


class VerifyFochTests(unittest.TestCase):
	def check(
		self,
		*,
		version_output: str,
		unknown_exit: int = 2,
		inspect_suffix: str = "",
		prefix: str | None = None,
	) -> None:
		with tempfile.TemporaryDirectory(prefix=prefix) as directory:
			work: Path = Path(directory).resolve()
			command: list[str] = fake_foch(
				work,
				version_output=version_output,
				unknown_exit=unknown_exit,
				inspect_suffix=inspect_suffix,
			)
			verify_foch(
				command,
				IDENTITY,
				work=work,
				environment=scratch_environment(work / "home"),
			)

	def test_accepts_the_expected_install(self) -> None:
		self.check(version_output=IDENTITY.version_output())

	def test_a_non_ascii_path_survives_the_pipe(self) -> None:
		# The inspected path must match whatever the host locale's code page is.
		self.check(version_output=IDENTITY.version_output(), prefix="fóch-Ā-")

	def test_an_unreadable_descriptor_fails(self) -> None:
		with self.assertRaisesRegex(ValueError, "could not read the descriptor"):
			self.check(
				version_output=IDENTITY.version_output(),
				inspect_suffix=" descriptor_error=missing name",
			)

	def test_version_output_must_match_exactly(self) -> None:
		for output in (
			IDENTITY.version_output()
			+ "cwt-schema overridden by FOCH_CWTOOLS_SCHEMA_DIR=/tmp/rules\n",
			FochIdentity("0.1.0", SCHEMA_ID).version_output(),
			FochIdentity("0.1.0-rc.1", "0" * 64).version_output(),
			"foch 0.1.0-rc.1\n",
		):
			with (
				self.subTest(output=output),
				self.assertRaisesRegex(ValueError, "must print exactly"),
			):
				self.check(version_output=output)

	def test_an_unknown_subcommand_must_fail(self) -> None:
		with self.assertRaisesRegex(ValueError, "no-such-foch-subcommand exited 0"):
			self.check(version_output=IDENTITY.version_output(), unknown_exit=0)

	def test_identity_requires_a_full_schema_id(self) -> None:
		with self.assertRaisesRegex(ValueError, "64 lowercase hex"):
			FochIdentity("0.1.0", SCHEMA_ID.upper())


class ScratchEnvironmentTests(unittest.TestCase):
	def test_drops_foch_overrides_and_homes_every_directory(self) -> None:
		with (
			tempfile.TemporaryDirectory() as directory,
			mock.patch.dict(
				"os.environ",
				{"FOCH_CWTOOLS_SCHEMA_DIR": "/rules", "FOCH_CWT_SCHEMA_ID": "x"},
			),
		):
			home: Path = Path(directory) / "home"
			environment: dict[str, str] = scratch_environment(home)
			self.assertTrue(home.is_dir())
			self.assertNotIn("FOCH_CWTOOLS_SCHEMA_DIR", environment)
			self.assertNotIn("FOCH_CWT_SCHEMA_ID", environment)
			for key in (
				"HOME",
				"USERPROFILE",
				"APPDATA",
				"LOCALAPPDATA",
				"XDG_CONFIG_HOME",
				"XDG_CACHE_HOME",
				"XDG_DATA_HOME",
				"FOCH_CONFIG_DIR",
				"FOCH_CACHE_ROOT",
				"FOCH_DATA_DIR",
			):
				with self.subTest(key=key):
					self.assertTrue(Path(environment[key]).is_relative_to(home))


if __name__ == "__main__":
	unittest.main()
