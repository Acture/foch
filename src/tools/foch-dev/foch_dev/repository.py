"""Resolve a checkout independently of where this package is installed."""

from pathlib import Path


def find_repository(start: Path | None = None) -> Path:
	location: Path = (start or Path.cwd()).resolve()
	for path in (location, *location.parents):
		if (path / "Cargo.toml").is_file() and (
			path / "src/packages/foch/Cargo.toml"
		).is_file():
			return path
	raise ValueError(f"no Foch checkout found from {location}; specify --repo")
