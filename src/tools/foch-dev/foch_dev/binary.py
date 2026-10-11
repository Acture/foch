"""Check that an installed `foch` is the release it claims to be.

Every distribution channel installs the same program: the crates.io and
Homebrew source builds, the PyPI wheel run through `uvx` or `uv tool`, and the
release archive behind WinGet. A `FochIdentity` names that program:
`foch --version` must print exactly `foch-cli <version>` and
`cwt-schema <id> (embedded)`, with no override line. `verify_foch` runs one
installation in a scratch home with every caller `FOCH_*` variable removed, so
neither an override nor existing user data can make it pass. It then checks
that `--help` succeeds, that an unknown subcommand fails, and that
`input inspect` resolves a minimal `foch.toml` project and reads its mod's
descriptor.

Output is decoded as UTF-8, which is what the Rust program writes to a pipe;
the locale code page would garble a non-ASCII path on Windows.

The installation is a command prefix rather than a path, so a check can run an
executable directly or through a launcher such as `uvx --from <wheel> foch`.
"""

from __future__ import annotations

import logging
import os
import re
import shlex
import subprocess
import tempfile
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

LOGGER: logging.Logger = logging.getLogger(__name__)

SCHEMA_ID: re.Pattern[str] = re.compile(r"[0-9a-f]{64}")
FIXTURE_MOD: str = "local_patch"
UNKNOWN_SUBCOMMAND: str = "no-such-foch-subcommand"
# What `input inspect` appends to a mod whose descriptor.mod it could not read.
DESCRIPTOR_ERROR: str = " descriptor_error="
# Generous enough for a first `uvx` run that downloads a wheel or a Python.
COMMAND_TIMEOUT_SECONDS: float = 600.0


@dataclass(frozen=True)
class FochIdentity:
	"""The package version and embedded CWT schema a release must report."""

	version: str
	cwt_schema_id: str

	def __post_init__(self) -> None:
		if SCHEMA_ID.fullmatch(self.cwt_schema_id) is None:
			raise ValueError(
				f"cwt-schema id must be 64 lowercase hex digits: {self.cwt_schema_id!r}"
			)

	def version_output(self) -> str:
		return f"foch-cli {self.version}\ncwt-schema {self.cwt_schema_id} (embedded)\n"


def run_command(
	command: Sequence[str],
	*,
	cwd: Path,
	env: Mapping[str, str] | None = None,
	capture: bool = False,
	timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
	"""Run a command with logged timing; stream its output unless captured."""
	LOGGER.info("running %s", shlex.join(command))
	started: float = time.monotonic()
	result: subprocess.CompletedProcess[str] = subprocess.run(
		list(command),
		cwd=cwd,
		env=None if env is None else dict(env),
		check=False,
		stdin=subprocess.DEVNULL,
		stdout=subprocess.PIPE if capture else None,
		stderr=subprocess.PIPE if capture else None,
		text=True,
		encoding="utf-8",
		timeout=timeout,
	)
	LOGGER.info(
		"%s exited %d in %.1fs",
		Path(command[0]).name,
		result.returncode,
		time.monotonic() - started,
	)
	return result


def timed_run(
	command: Sequence[str],
	*,
	cwd: Path,
	env: Mapping[str, str] | None = None,
	capture: bool = False,
	timeout: float | None = None,
) -> str:
	"""Run a command that must succeed; return its stdout when captured."""
	result = run_command(command, cwd=cwd, env=env, capture=capture, timeout=timeout)
	if result.returncode != 0:
		detail: str = f":\n{result.stdout}{result.stderr}" if capture else ""
		raise ValueError(f"{shlex.join(command)} exited {result.returncode}{detail}")
	return result.stdout if capture else ""


def scratch_environment(home: Path) -> dict[str, str]:
	"""The caller's environment without Foch overrides, homed in `home`.

	Foch's config, cache and data directories, and the platform directories
	`dirs` would otherwise derive them from, all point below `home`.
	"""
	home.mkdir(parents=True, exist_ok=True)
	environment: dict[str, str] = {
		key: value for key, value in os.environ.items() if not key.startswith("FOCH_")
	}
	environment.update(
		HOME=str(home),
		USERPROFILE=str(home),
		APPDATA=str(home / "AppData" / "Roaming"),
		LOCALAPPDATA=str(home / "AppData" / "Local"),
		XDG_CONFIG_HOME=str(home / ".config"),
		XDG_CACHE_HOME=str(home / ".cache"),
		XDG_DATA_HOME=str(home / ".local" / "share"),
		FOCH_CONFIG_DIR=str(home / "foch-config"),
		FOCH_CACHE_ROOT=str(home / "foch-cache"),
		FOCH_DATA_DIR=str(home / "foch-data"),
	)
	return environment


def write_fixture(root: Path) -> tuple[Path, Path]:
	"""A one-mod EU4 `foch.toml` project; returns the manifest and mod root."""
	mod_root: Path = root / FIXTURE_MOD
	mod_root.mkdir(parents=True)
	(mod_root / "descriptor.mod").write_text(
		'name="Foch install check"\n', encoding="utf-8"
	)
	manifest: Path = root / "foch.toml"
	manifest.write_text(
		"[project]\n"
		'game = "eu4"\n'
		"\n"
		"[[project.mods]]\n"
		f'id = "{FIXTURE_MOD}"\n'
		f'path = "{FIXTURE_MOD}"\n',
		encoding="utf-8",
	)
	return manifest, mod_root


def verify_foch(
	command: Sequence[str],
	identity: FochIdentity,
	*,
	work: Path,
	environment: Mapping[str, str],
) -> None:
	"""Run the installation checks for `command` inside a fresh `work` subdirectory."""
	check: Path = Path(tempfile.mkdtemp(prefix="foch-check-", dir=work)).resolve()

	def foch(*arguments: str) -> subprocess.CompletedProcess[str]:
		return run_command(
			[*command, *arguments],
			cwd=check,
			env=environment,
			capture=True,
			timeout=COMMAND_TIMEOUT_SECONDS,
		)

	def succeed(*arguments: str) -> str:
		result = foch(*arguments)
		if result.returncode != 0:
			raise ValueError(
				f"foch {shlex.join(arguments)} exited {result.returncode}:\n"
				f"{result.stdout}{result.stderr}"
			)
		return result.stdout

	reported: str = succeed("--version")
	if reported != identity.version_output():
		raise ValueError(
			f"{shlex.join(command)} --version must print exactly\n"
			f"{identity.version_output()}but printed\n{reported}"
		)
	LOGGER.info(
		"foch-cli %s embeds cwt-schema %s", identity.version, identity.cwt_schema_id
	)
	if not succeed("--help").strip():
		raise ValueError("foch --help printed nothing")
	rejected = foch(UNKNOWN_SUBCOMMAND)
	if rejected.returncode == 0:
		raise ValueError(f"foch {UNKNOWN_SUBCOMMAND} exited 0")

	manifest, mod_root = write_fixture(check / "fixture")
	inspected: str = succeed("input", "inspect", str(manifest))
	missing: list[str] = [
		fragment
		for fragment in ("game: eu4", FIXTURE_MOD, str(mod_root))
		if fragment not in inspected
	]
	if missing:
		raise ValueError(f"foch input inspect output lacks {missing}:\n{inspected}")
	if DESCRIPTOR_ERROR in inspected:
		raise ValueError(
			f"foch input inspect could not read the descriptor:\n{inspected}"
		)
