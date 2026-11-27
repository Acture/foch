"""Bind the locally built DLL digest into a fresh Foch package directory."""

from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

PACKAGE = Path(__file__).resolve().parents[1]
REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "src" / "tools" / "foch-dev"))

DLL_NAME = "foch_plugin_peace.dll"


def default_dll() -> Path:
	for relative in (Path("target/debug") / DLL_NAME, Path("target/release") / DLL_NAME):
		candidate = REPO / relative
		if candidate.is_file():
			return candidate
	return REPO / "target" / "debug" / DLL_NAME


def refuse_output(path: Path) -> None:
	# exists() follows links; is_symlink() still catches a broken link.
	if path.is_symlink() or path.exists():
		raise SystemExit(f"refusing existing output {path}")


def bind_manifest(text: str, digest: str) -> str:
	bound = text.replace("@DLL_SHA256@", digest)
	if "[[files]]" not in bound:
		bound = bound.rstrip("\n") + (
			"\n\n[[files]]\n" f'path = "{DLL_NAME}"\n' f'sha256 = "{digest}"\n'
		)
	if digest not in bound:
		raise SystemExit("manifest has no SHA-256 digest binding")
	if not bound.endswith("\n"):
		bound += "\n"
	return bound


def main() -> None:
	from foch_dev.runtime_host import validate_plugin

	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("output", type=Path, help="Fresh directory for the package")
	parser.add_argument("--dll", type=Path, help="Built foch_plugin_peace.dll")
	args = parser.parse_args()
	dll_path = (args.dll or default_dll()).resolve()
	data = dll_path.read_bytes()
	if not data.startswith(b"MZ"):
		parser.error("input must be the built Windows DLL")
	try:
		validate_plugin(data)
	except ValueError as error:
		parser.error(str(error))
	digest = hashlib.sha256(data).hexdigest()
	manifest = bind_manifest(
		(PACKAGE / "foch-plugin.toml").read_text(encoding="utf-8"),
		digest,
	)
	license_path = REPO / "LICENSE"
	if not license_path.is_file():
		parser.error(f"missing {license_path}")
	refuse_output(args.output)
	try:
		# mkdir without exist_ok refuses an existing output, including symlinks.
		args.output.mkdir(parents=True)
	except FileExistsError:
		parser.error(f"refusing existing output {args.output}")
	(args.output / "foch-plugin.toml").write_text(manifest, encoding="utf-8", newline="\n")
	(args.output / DLL_NAME).write_bytes(data)
	(args.output / "LICENSE").write_bytes(license_path.read_bytes())
	print(args.output.resolve())


if __name__ == "__main__":
	main()
