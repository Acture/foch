# Foch maintenance tools

One internal Python package owns repository contracts, CWT snapshot hashing,
diagnostic check workflows and release packaging checks. It depends on
`jsonschema` and `pyyaml` for WinGet manifest validation, and installs no
additional executable. Its APIs are importable; `python -m foch_dev` is the
operator entrypoint. It is not the Workshop merge-quality acceptance harness.

From the Foch repository root:

```fish
uv run --locked --project src/tools/foch-dev python -m foch_dev --help
uv run --locked --project src/tools/foch-dev python -m foch_dev check
uv run --locked --project src/tools/foch-dev python -m foch_dev schema-hash
uv run --locked --project src/tools/foch-dev python -m foch_dev crate-smoke
uv run --locked --project src/tools/foch-dev python -m foch_dev version --tag v0.0.1
uv run --locked --project src/tools/foch-dev python -m foch_dev release preflight --tag v0.0.1
uv run --locked --project src/tools/foch-dev python -m foch_dev release unpublished
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

The published crates are `foch-cli` and every workspace crate its normal and
build dependencies reach, short of the externally released
`tree-sitter-paradox`: today `foch`, `foch-annotation`, `foch-lsp`,
`foch-runner` and `foch-test`. `check`, `crate-smoke` and `release` derive that
set from `cargo metadata`; `check` requires exactly those crates to be
publishable and every path dependency they build from pinned at its exact
version.

`crate-smoke` runs `cargo package --locked --no-verify` for the published
crates and `tree-sitter-paradox`, checks each `.crate` against the 10 MiB
crates.io limit and the files it must and must not carry (its license texts
and README byte-identical to the repository's, no `tests/`), and unpacks them
into a directory outside the checkout. There a `[patch.crates-io]` config
stands every unpacked crate but `foch-cli` in for its registry release; the
re-lock may only change those entries. `cargo install --locked` then builds the
release `foch` with the checkout's toolchain, and the installed binary must
embed the `schema-hash` of the vendored rules, print `--help` and inspect a
minimal `foch.toml`. It compiles a release build, so it is a maintainer or
release-job command, not a quick check. `--allow-dirty` packages uncommitted
changes, `--offline` uses only the cargo registry cache, and `--work-dir`
keeps the crates, build output and install in an empty directory. Passing it
does not show that a registry install works: the grammar is packaged from the
pinned submodule, so it must be released at that revision before `foch` can
be published against it.

## Release packaging

Every binary channel ships one release identity: `foch --version` prints
exactly `foch-cli <version>` and `cwt-schema <id> (embedded)`, where
`<version>` is the Cargo workspace version and `<id>` its `schema-hash`.
`version [--tag vX.Y.Z]` prints that version's `tag=`, `version=` and
`pep440=` spellings as `$GITHUB_OUTPUT` lines; only `X.Y.Z` and
`X.Y.Z-(alpha|beta|rc).N` are releasable, and `--tag` fails unless the tag
names the workspace version.

`dist` turns maturin's bin wheels (root `pyproject.toml`) into the release
archives and installs them:

```fish
uv run --locked --project src/tools/foch-dev python -m foch_dev dist maturin-version
uv run --locked --project src/tools/foch-dev python -m foch_dev dist archive \
	--wheel dist/foch-0.0.1-py3-none-macosx_11_0_arm64.whl --target darwin-arm64 --out dist
uv run --locked --project src/tools/foch-dev python -m foch_dev dist smoke \
	--wheel dist/foch-0.0.1-py3-none-macosx_11_0_arm64.whl \
	--archive dist/foch-0.0.1-darwin-arm64.tar.gz
uv run --locked --project src/tools/foch-dev python -m foch_dev dist check-binary foch
```

`archive` writes `foch-<version>-<target>.tar.gz` (`.zip` for `win32-x64`) with
the wheel's executable and license texts at its root; its bytes depend only on
the wheel and `SOURCE_DATE_EPOCH`. The supported targets are `linux-x64`
(manylinux_2_28), `darwin-arm64` (macOS 11) and `win32-x64`; a wheel tagged for
another platform floor is refused. `smoke` installs a local wheel offline, or
`--index-requirement foch==<pep440>` from an index, through `uvx`, `uv tool
install`, `upgrade` and `uninstall` in an isolated uv home, and requires the
archive's executable to be the installed one. `check-binary` runs the same
installed-binary checks on any `foch` path or PATH name, so cargo, WinGet and
Homebrew installs are verified the same way. Each check runs with every
`FOCH_*` variable removed and a scratch home. `--expect-version` and
`--expect-cwt-schema-id` default to the checkout's identity.

`winget render --asset foch-<version>-win32-x64.zip --release-date YYYY-MM-DD
--out <root>` writes the three Acture.Foch manifests under
`<root>/manifests/a/Acture/Foch/<version>/`, hashing the archive itself, and
`winget check <dir> [--release]` validates them against the WinGet 1.12.0
schemas vendored in `foch_dev/schemas/winget/` from microsoft/winget-cli under
its MIT license. `src/apps/foch-cli/scripts/smoke_winget.ps1` installs those
manifests on Windows.

`release preflight --tag vX.Y.Z` only reads the registries. It fails unless
the tag spells the releasable workspace version, the published crates pin
every path dependency exactly, crates.io has the pinned `tree-sitter-paradox`
with the checkout's bytes, no published crate has the version, `foch` 0.1.0
(another product) is yanked, PyPI and winget-pkgs lack the version, and no
published GitHub release for the tag has assets yet. A crate crates.io does not
have at all passes with a note that its first publish needs an API token,
because Trusted Publishing cannot create a crate. A draft release is invisible
to it; the release job compares a draft's assets byte for byte. Set
`GITHUB_TOKEN` to lift the anonymous GitHub API limit.

`release unpublished` prints the published crates whose version crates.io
lacks, one per line, and fails on any registry error. `release.yml` publishes
exactly those, so a re-run resumes an interrupted multi-crate publish. It also
fails when crates.io has no crate of one of them at all, which the workflow's
Trusted Publishing token cannot create, so the job stops before uploading
anything; `--allow-new` lists those crates too, for a publish with an API token
(`docs/RELEASE_CHECKLIST.md`).

Module boundaries:

- `contracts`: Cargo binary/example, the published crates and their closure, CWT location,
  desktop dependency/source checks, and the name, repository and license that
  the PyPI, WinGet and crate metadata share.
- `schema`: the `cwt_schema_id` digest, in the build script's file order.
- `crates`: the out-of-tree crate packaging and install smoke.
- `versions`: the release version and its per-channel spellings.
- `binary`: the installed-binary identity checks every channel shares.
- `dist`: release wheels and archives, and their uv install smoke.
- `winget`: WinGet manifest rendering and schema checks.
- `release`: the read-only release preflight across every registry, and the
  crates the release job publishes.
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

CI runs every command through `uv run --locked`, which installs the locked
dependencies. The release workflows are `.github/workflows/dist.yml`, `release.yml` and
`verify-install.yml`. Hooks and release orchestration stay in root
`scripts/`; Windows installer smoke belongs to `src/apps/foch-desktop/scripts/`.
EU4 builtin catalog generation belongs to the neighboring `eu4-analysis` tool.
