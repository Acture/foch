from __future__ import annotations

import subprocess
from pathlib import Path


def main() -> None:
	repo_root: Path = Path(__file__).resolve().parent.parent
	result: subprocess.CompletedProcess[str] = subprocess.run(
		[
			str(repo_root / "scripts" / "render_homebrew_formula.sh"),
			"Acture/foch",
			"1.2.3",
			"https://example.test/foch-1.2.3-source.tar.gz",
			"a" * 64,
		],
		cwd=repo_root,
		check=True,
		capture_output=True,
		text=True,
	)
	formula: str = result.stdout
	expected_fragments: tuple[str, ...] = (
		'homepage "https://github.com/Acture/foch"',
		'url "https://example.test/foch-1.2.3-source.tar.gz"',
		f'sha256 "{"a" * 64}"',
		'license all_of: ["AGPL-3.0-only", "GPL-3.0-only", "MIT"]',
		'std_cargo_args(path: "src/apps/foch-cli")',
		'"--bin", "foch"',
		"foch input inspect",
		'pkgshare.install "NOTICE.md", "LICENSE-MERGIRAF.txt"',
	)
	missing: list[str] = [
		fragment for fragment in expected_fragments if fragment not in formula
	]
	if missing:
		raise SystemExit(f"rendered formula is missing: {missing}")
	if '"--bins"' in formula:
		raise SystemExit("rendered formula must install only the foch binary")


if __name__ == "__main__":
	main()
