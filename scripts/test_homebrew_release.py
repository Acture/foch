from __future__ import annotations

import os
import subprocess
import tarfile
import tempfile
from pathlib import Path

REPO_ROOT: Path = Path(__file__).resolve().parent.parent
ARCHIVE: Path = REPO_ROOT / "scripts" / "build_source_archive.sh"
RENDER: Path = REPO_ROOT / "scripts" / "render_homebrew_formula.sh"
COMMIT: Path = REPO_ROOT / "scripts" / "commit_homebrew_formula.sh"
PHYSICAL_FORMULA: str = "packaging/homebrew/Formula/foch.rb"
GIT_ENV: dict[str, str] = {
	**os.environ,
	"LC_ALL": "C",
	"GIT_CONFIG_GLOBAL": os.devnull,
	"GIT_CONFIG_NOSYSTEM": "1",
	"GIT_AUTHOR_NAME": "test",
	"GIT_AUTHOR_EMAIL": "test@example.test",
	"GIT_COMMITTER_NAME": "test",
	"GIT_COMMITTER_EMAIL": "test@example.test",
}


def run(
	args: list[str], cwd: Path, check: bool = True
) -> subprocess.CompletedProcess[str]:
	return subprocess.run(
		args, cwd=cwd, env=GIT_ENV, check=check, capture_output=True, text=True
	)


def render(version: str) -> str:
	url: str = f"https://example.test/foch-{version}-source.tar.gz"
	return run([str(RENDER), "Acture/foch", version, url, "a" * 64], REPO_ROOT).stdout


def write_files(root: Path, paths: tuple[str, ...]) -> None:
	for path in paths:
		(root / path).parent.mkdir(parents=True, exist_ok=True)
		(root / path).write_text(path)


def init_repo(repo: Path, paths: tuple[str, ...]) -> None:
	run(["git", "init", "--quiet", "--initial-branch=master", str(repo)], repo.parent)
	write_files(repo, paths)
	run(["git", "add", "--all"], repo)
	run(["git", "commit", "--quiet", "-m", "init"], repo)


def check_archive() -> None:
	with tempfile.TemporaryDirectory() as raw:
		work: Path = Path(raw)
		init_repo(work / "schema", ("schema.cwt",))
		init_repo(work / "private", ("private.md",))
		source: Path = work / "foch"
		tracked: tuple[str, ...] = ("Cargo.toml", "src/lib.rs", "src/notes/kept.txt")
		init_repo(source, tracked)
		(source / ".gitignore").write_text(".env\ntarget/\n")
		run(["git", "add", ".gitignore"], source)
		for path, url in (("vendor/schema", "schema"), ("notes", "private")):
			run(
				[
					"git",
					"-c",
					"protocol.file.allow=always",
					"submodule",
					"add",
					"--quiet",
					str(work / url),
					path,
				],
				source,
			)
		run(["git", "commit", "--quiet", "-m", "submodules"], source)
		write_files(source, (".env", "target/debug/foch", "untracked.txt"))

		output: str = run([str(ARCHIVE), "1.2.3", "dist"], source).stdout.strip()
		if Path(output) != (source / "dist/foch-1.2.3-source.tar.gz").resolve():
			raise SystemExit(f"unexpected archive path: {output}")
		with tarfile.open(output) as archive:
			files: set[str] = {m.name for m in archive.getmembers() if m.isfile()}
		kept: tuple[str, ...] = (
			*tracked,
			".gitignore",
			".gitmodules",
			"vendor/schema/schema.cwt",
		)
		expected: set[str] = {f"foch-1.2.3/{path}" for path in kept}
		if files != expected:
			raise SystemExit(f"archive files {sorted(files)} != {sorted(expected)}")

		run(
			["git", "submodule", "deinit", "--quiet", "--force", "vendor/schema"],
			source,
		)
		if run([str(ARCHIVE), "1.2.3", "dist"], source, check=False).returncode == 0:
			raise SystemExit("archive must require initialized public submodules")


def check_render() -> None:
	formula: str = render("1.2.3")
	expected_fragments: tuple[str, ...] = (
		'homepage "https://github.com/Acture/foch"',
		'url "https://example.test/foch-1.2.3-source.tar.gz"',
		f'sha256 "{"a" * 64}"',
		'license all_of: ["AGPL-3.0-only", "GPL-3.0-only", "MIT"]\n\n  depends_on',
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


def init_tap(tap: Path, formula_dir: str) -> None:
	run(["git", "init", "--quiet", "--initial-branch=master", str(tap)], tap.parent)
	(tap / formula_dir).mkdir(parents=True)
	(tap / formula_dir / "foch.rb").write_text("class Foch < Formula\nend\n")
	if formula_dir != "Formula":
		(tap / "Formula").symlink_to(formula_dir, target_is_directory=True)
	run(["git", "add", "--all"], tap)
	run(["git", "commit", "--quiet", "-m", "init"], tap)


def commit(tap: Path, formula: Path, version: str) -> subprocess.CompletedProcess[str]:
	return run([str(COMMIT), str(tap), str(formula), version], REPO_ROOT, check=False)


def head(tap: Path) -> str:
	return run(["git", "rev-parse", "HEAD"], tap).stdout.strip()


def expect_commit(tap: Path, formula: Path, version: str, path: str) -> None:
	result: subprocess.CompletedProcess[str] = commit(tap, formula, version)
	if result.returncode != 0:
		raise SystemExit(f"commit failed for {path}: {result.stderr}")
	last: str = run(["git", "show", "--name-only", "--format=%s", "HEAD"], tap).stdout
	if last.split() != ["foch", version, path]:
		raise SystemExit(f"unexpected commit for {path}: {last!r}")
	if (tap / path).read_text() != formula.read_text():
		raise SystemExit(f"{path} does not contain the rendered formula")
	if run(["git", "status", "--porcelain"], tap).stdout:
		raise SystemExit(f"tap is dirty after committing {path}")


def check_symlinked_tap(work: Path, formula: Path) -> None:
	tap: Path = work / "symlinked"
	init_tap(tap, "packaging/homebrew/Formula")
	(tap / "Formula/foch.rb").write_text(formula.read_text())
	ignored: subprocess.CompletedProcess[str] = run(
		["git", "add", "Formula/foch.rb"], tap, check=False
	)
	if ignored.returncode == 0 or "beyond a symbolic link" not in ignored.stderr:
		raise SystemExit("fixture must reject pathspecs through the Formula symlink")
	run(["git", "checkout", "--", "."], tap)

	expect_commit(tap, formula, "1.2.3", PHYSICAL_FORMULA)
	if not (tap / "Formula").is_symlink():
		raise SystemExit("Formula symlink was replaced")
	before: str = head(tap)
	unchanged: subprocess.CompletedProcess[str] = commit(tap, formula, "1.2.3")
	if unchanged.returncode != 0 or head(tap) != before:
		raise SystemExit(f"unchanged formula must not commit: {unchanged.stderr}")


def check_plain_tap(work: Path, formula: Path) -> None:
	tap: Path = work / "plain"
	init_tap(tap, "Formula")
	expect_commit(tap, formula, "1.2.3", "Formula/foch.rb")


def check_rejected_taps(work: Path, formula: Path) -> None:
	missing: Path = work / "missing"
	missing.mkdir()
	run(["git", "init", "--quiet", str(missing)], work)
	outside: Path = work / "outside"
	init_tap(outside, "Formula")
	run(["git", "rm", "--quiet", "-r", "Formula"], outside)
	(work / "elsewhere").mkdir()
	(outside / "Formula").symlink_to(work / "elsewhere", target_is_directory=True)
	linked: Path = work / "linked"
	init_tap(linked, "Formula")
	target: Path = work / "linked-target.rb"
	target.write_text("outside")
	(linked / "Formula/foch.rb").unlink()
	(linked / "Formula/foch.rb").symlink_to(target)
	for tap in (missing, outside, linked):
		if commit(tap, formula, "1.2.3").returncode == 0:
			raise SystemExit(f"{tap.name} tap must be rejected")
	if any((work / "elsewhere").iterdir()) or (missing / "Formula").exists():
		raise SystemExit("rejected taps must not be written")
	if target.read_text() != "outside":
		raise SystemExit("a formula symlink must not be written through")


def check_commit() -> None:
	with tempfile.TemporaryDirectory() as raw:
		work: Path = Path(raw)
		formula: Path = work / "foch.rb"
		formula.write_text(render("1.2.3"))
		check_symlinked_tap(work, formula)
		check_plain_tap(work, formula)
		check_rejected_taps(work, formula)


def main() -> None:
	check_archive()
	check_render()
	check_commit()


if __name__ == "__main__":
	main()
