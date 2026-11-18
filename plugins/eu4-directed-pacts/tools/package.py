"""Bind the locally built DLL digest into a Foch package in a fresh directory."""

import argparse
import hashlib
import sys
from pathlib import Path

# This plugin is built in the public Foch checkout; reuse its package validator.
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "src/tools/foch-dev"))



def main():
	from foch_dev.runtime_host import validate_plugin

	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("dll", type=Path)
	parser.add_argument("output", type=Path)
	parser.add_argument("--roster-probe", action="store_true", help="Package the read diagnostic with its independent plugin identity.")
	args = parser.parse_args()
	data = args.dll.read_bytes()
	if not data.startswith(b"MZ"):
		parser.error("input must be the built Windows DLL")
	try:
		validate_plugin(data)
	except ValueError as error:
		parser.error(str(error))
	template_name = "foch-roster-probe.toml.in" if args.roster_probe else "foch-plugin.toml.in"
	dll_name = "eu4_directed_pacts_roster_probe.dll" if args.roster_probe else "eu4_directed_pacts.dll"
	template = (Path(__file__).resolve().parents[1] / template_name).read_text(encoding="utf-8")
	manifest = template.replace("@DLL_SHA256@", hashlib.sha256(data).hexdigest())
	# mkdir without exist_ok refuses an existing output, including symlinks.
	args.output.mkdir(parents=True)
	(args.output / "plugins").mkdir()
	(args.output / "plugins" / dll_name).write_bytes(data)
	(args.output / "foch-plugin.toml").write_text(manifest, encoding="utf-8", newline="\n")
	print(args.output.resolve())


if __name__ == "__main__":
	main()
