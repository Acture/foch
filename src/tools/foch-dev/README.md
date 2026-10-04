# Foch maintenance tools

One internal Python package owns repository contracts, CWT snapshot hashing and
diagnostic check workflows. It has no runtime dependencies and installs no
additional executable. Its APIs are importable; `python -m foch_dev` is the
operator entrypoint. It is not the Workshop merge-quality acceptance harness.

From the Foch repository root:

```fish
uv run --locked --project src/tools/foch-dev python -m foch_dev --help
uv run --locked --project src/tools/foch-dev python -m foch_dev check
uv run --locked --project src/tools/foch-dev python -m foch_dev schema-hash
```

`--project` preserves the caller's working directory. Commands needing a
checkout search upward from that directory; `--repo /path/to/foch` selects one
explicitly. This works for an installed wheel and for source archives without
Git metadata or private notes. `schema-hash --schema-root /path/to/config`
also works without a checkout.

For a maintainer-run diagnostic against installed mods:

```fish
uv run --locked --project src/tools/foch-dev python -m foch_dev smoke \
	--playset /absolute/path/to/playset.json --mods 123,456
uv run --locked --project src/tools/foch-dev python -m foch_dev compare \
	/path/to/baseline-summary.json /path/to/candidate-summary.json \
	--gate-rule S004 --min-absolute-drop 10 --output /tmp/comparison.json
```

`smoke` invokes `cargo run --offline --bin foch -- check`, preserving playset
order. It forwards the check's progress and writes unique raw JSON, summary JSON
and text files under `target/eu4-real-smoke/` (`--out-dir` overrides this).
Input/output arguments resolve from the caller's working directory. Missing or
invalid check output returns failure, even if the process returned zero.
`compare` validates summaries, reports rule/path deltas and exits 2 when a gate
fails. Failure summaries cannot pass through exit/fatal-error overrides.
Fewer diagnostic findings alone do not establish merge correctness.

Module boundaries:

- `contracts`: Cargo binary/example and desktop dependency/source checks.
- `schema`: the existing sorted, line-ending-normalized CWT digest.
- `models` / `summary`: typed JSON boundaries and pure finding aggregation.
- `smoke` / `compare`: execution and comparison, with typed options and reusable functions.
- `repository`: checkout discovery independent of the package installation path.

Package checks (no game or Ghidra required):

```fish
uv run --locked --project src/tools/foch-dev python -m unittest discover -s src/tools/foch-dev/tests
uv run --locked --project src/tools/foch-dev ruff check src/tools/foch-dev
uv run --locked --project src/tools/foch-dev ruff format --check src/tools/foch-dev
uv run --locked --project src/tools/foch-dev ty check src/tools/foch-dev
```

CI installs the package for contract/hash commands and checks its locked
development environment. Hooks and release orchestration stay in root
`scripts/`; Windows installer smoke belongs to `src/apps/foch-desktop/scripts/`.
EU4 builtin catalog generation belongs to the neighboring `eu4-analysis` tool.
