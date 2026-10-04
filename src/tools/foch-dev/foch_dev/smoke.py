from __future__ import annotations

import argparse
import json
import logging
import signal
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TextIO

from .models import Summary, array_value, object_value, read_check_output
from .repository import find_repository
from .summary import build_failure_summary, render_text_summary, summarize_findings

LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class SmokeOptions:
	playset: Path
	mods: str = ""
	out_dir: Path | None = None
	repo: Path | None = None


def add_arguments(parser: argparse.ArgumentParser) -> None:
	parser.add_argument("--playset", required=True, type=Path)
	parser.add_argument("--mods", default="")
	parser.add_argument("--out-dir", type=Path)
	parser.add_argument("--repo", type=Path)


def slugify(value: str) -> str:
	parts: list[str] = []
	for ch in value:
		if ch.isalnum():
			parts.append(ch.lower())
		else:
			parts.append("-")
	slug: str = "".join(parts).strip("-")
	while "--" in slug:
		slug = slug.replace("--", "-")
	return slug or "smoke"


def load_playset(path: Path) -> dict[str, object]:
	return object_value(json.loads(path.read_text(encoding="utf-8")))


def normalize_mod_filter(raw: str) -> set[str]:
	return {part.strip() for part in raw.split(",") if part.strip()}


def filter_playset(playset: dict[str, object], selected: set[str]) -> dict[str, object]:
	if not selected:
		return playset
	filtered_mods: list[dict[str, object]] = []
	for value in array_value(playset["mods"]):
		mod: dict[str, object] = object_value(value)
		steam_id: str = str(mod.get("steamId", "")).strip()
		display_name: str = str(mod.get("displayName", "")).strip()
		if steam_id in selected or display_name in selected:
			filtered_mods.append(mod)
	filtered: dict[str, object] = dict(playset)
	filtered["mods"] = filtered_mods
	return filtered


def enabled_mod_ids(playset: dict[str, object]) -> set[str]:
	mod_ids: set[str] = set()
	for value in array_value(playset["mods"]):
		mod: dict[str, object] = object_value(value)
		if not mod.get("enabled", False):
			continue
		steam_id: str = str(mod.get("steamId", "")).strip()
		display_name: str = str(mod.get("displayName", "")).strip()
		if steam_id:
			mod_ids.add(steam_id)
		elif display_name:
			mod_ids.add(display_name)
	return mod_ids


def forward_stream(
	stream: TextIO,
	sink: TextIO,
	chunks: list[str],
) -> None:
	try:
		for line in iter(stream.readline, ""):
			sink.write(line)
			sink.flush()
			chunks.append(line)
	finally:
		stream.close()


def run_check(
	playset_path: Path, output_path: Path, repository: Path
) -> subprocess.CompletedProcess[str]:
	command: list[str] = [
		"cargo",
		"run",
		"--offline",
		"--bin",
		"foch",
		"--",
		"check",
		str(playset_path),
		"--format",
		"json",
		"--output",
		str(output_path),
	]
	process = subprocess.Popen(
		command,
		cwd=repository,
		text=True,
		stdout=subprocess.PIPE,
		stderr=subprocess.PIPE,
		bufsize=1,
	)
	stdout_pipe = process.stdout
	stderr_pipe = process.stderr
	assert stdout_pipe is not None
	assert stderr_pipe is not None

	stdout_chunks: list[str] = []
	stderr_chunks: list[str] = []
	threads: list[threading.Thread] = [
		threading.Thread(
			target=forward_stream,
			args=(stdout_pipe, sys.stdout, stdout_chunks),
			daemon=True,
		),
		threading.Thread(
			target=forward_stream,
			args=(stderr_pipe, sys.stderr, stderr_chunks),
			daemon=True,
		),
	]
	for thread in threads:
		thread.start()

	interrupted = False
	try:
		returncode = process.wait()
	except KeyboardInterrupt:
		interrupted = True
		process.send_signal(signal.SIGINT)
		returncode = process.wait()
	finally:
		for thread in threads:
			thread.join()

	if interrupted:
		raise KeyboardInterrupt

	return subprocess.CompletedProcess(
		command,
		returncode,
		"".join(stdout_chunks),
		"".join(stderr_chunks),
	)


def run(args: SmokeOptions) -> int:
	repository: Path = find_repository(args.repo)
	out_dir: Path = (args.out_dir or repository / "target/eu4-real-smoke").resolve()
	out_dir.mkdir(parents=True, exist_ok=True)

	playset: dict[str, object] = load_playset(args.playset)
	selected: set[str] = normalize_mod_filter(args.mods)
	filtered: dict[str, object] = filter_playset(playset, selected)
	target_mod_ids: set[str] = enabled_mod_ids(filtered)
	if not target_mod_ids:
		raise ValueError("no enabled mods matched the playset/filter")
	playset_slug: str = slugify(
		f"{args.playset.stem}-{'-'.join(sorted(selected)) if selected else 'all'}"
	)

	with tempfile.NamedTemporaryFile(
		"w",
		delete=False,
		encoding="utf-8",
		dir=out_dir,
		suffix=".json",
		prefix=f"{playset_slug}-",
	) as handle:
		json.dump(filtered, handle, ensure_ascii=False)
		temp_playset_path: Path = Path(handle.name)

	run_slug: str = temp_playset_path.stem
	raw_output_path: Path = out_dir / f"{run_slug}-check.json"
	summary_json_path: Path = out_dir / f"{run_slug}-summary.json"
	summary_text_path: Path = out_dir / f"{run_slug}-summary.txt"

	started: float = time.monotonic()
	LOGGER.info(
		"Checking %d selected mods; output: %s", len(target_mod_ids), raw_output_path
	)
	try:
		result: subprocess.CompletedProcess[str] = run_check(
			temp_playset_path, raw_output_path, repository
		)
		summary: Summary
		if raw_output_path.is_file():
			try:
				data = read_check_output(raw_output_path)
			except (ValueError, KeyError) as err:
				summary = build_failure_summary(
					target_mod_ids,
					raw_output_path,
					f"invalid check output: {err}",
				)
			else:
				summary = summarize_findings(data, target_mod_ids)
		else:
			summary = build_failure_summary(
				target_mod_ids,
				raw_output_path,
				"check did not produce an output file",
			)
		summary["playset_path"] = str(args.playset)
		summary["selected_mod_filters"] = sorted(selected)
		summary["check_exit_code"] = result.returncode
		summary["stdout"] = result.stdout
		summary["stderr"] = result.stderr

		summary_json_path.write_text(
			json.dumps(summary, ensure_ascii=False, indent=2) + "\n",
			encoding="utf-8",
		)
		summary_text_path.write_text(render_text_summary(summary), encoding="utf-8")

		print(summary_text_path)
		return result.returncode or (1 if summary.get("error") else 0)
	except KeyboardInterrupt:
		print("Interrupted.", file=sys.stderr)
		return 130
	finally:
		temp_playset_path.unlink(missing_ok=True)
		LOGGER.info("Check finished in %.1fs", time.monotonic() - started)
