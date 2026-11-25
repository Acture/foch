"""Bind the locally built DLL digest into a Foch package in a fresh directory."""

import argparse
import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "src/tools/foch-dev"))


def main():
	from foch_dev.runtime_host import validate_plugin

	parser = argparse.ArgumentParser(description=__doc__)
	parser.add_argument("dll", type=Path)
	parser.add_argument("output", type=Path)
	args = parser.parse_args()
	data = args.dll.read_bytes()
	if not data.startswith(b"MZ"):
		parser.error("input must be the built Windows DLL")
	try:
		validate_plugin(data)
	except ValueError as error:
		parser.error(str(error))
	template = (Path(__file__).resolve().parents[1] / "foch-plugin.toml.in").read_text(encoding="utf-8")
	manifest = template.replace("@DLL_SHA256@", hashlib.sha256(data).hexdigest())
	args.output.mkdir(parents=True)
	(args.output / "plugins").mkdir()
	(args.output / "plugins" / "foch_plugin_read_decide.dll").write_bytes(data)
	(args.output / "foch-plugin.toml").write_text(manifest, encoding="utf-8", newline="\n")
	fixture = Path(__file__).resolve().parents[1] / "fixtures" / "probe"
	if fixture.is_dir():
		target = args.output / "fixtures" / "probe"
		target.parent.mkdir(parents=True)
		copy_tree(fixture, target)
	print(args.output.resolve())


def copy_tree(source: Path, dest: Path) -> None:
	dest.mkdir(parents=True)
	for path in source.rglob("*"):
		relative = path.relative_to(source)
		sink = dest / relative
		if path.is_dir():
			sink.mkdir(parents=True, exist_ok=True)
		elif path.is_file():
			sink.parent.mkdir(parents=True, exist_ok=True)
			sink.write_bytes(path.read_bytes())


if __name__ == "__main__":
	main()
