# Project Status

Last verified: 2026-10-04, source layout `f73eab9` and documentation follow-up
`2fc9387`; the Distribution section states its own checks. This page records
the current product state; it is not a development log. Active work,
dependencies and blockers live in the Linear Foch project.

## Product state

Foch is an unreleased, EU4-only alpha at `0.0.1`. It analyzes an ordered playset,
preserves compatible mod contributions under verified content-family semantics,
and reports ambiguity for review. It is not a verified one-click merger for
arbitrary modlists.

- `foch merge` freezes the complete analysis and review before confirmation.
  Commit validates the frozen inputs and installs safe files or complete
  definition modules; unsafe units may be withheld with `partial_success`.
- Playset order and declared dependencies are semantic inputs. Source mods and
  the game installation remain read-only. Paired Workshop ACF records provide
  installed version identity.
- The CWT rule pack is compiled from the public vendor submodule into the
  executable; `foch --version` reports its schema identity.
- The CLI includes `foch lsp`. The Tauri application links the main Rust library
  directly and provides input inspection and merge-review browsing. Desktop
  commit/export and a durable `MergeSession` are not implemented.
- Editor and desktop provenance views expose adopted merge sources. Their
  existence does not establish packaged desktop readiness or in-game behavior.
- The P-736 branch carries validated `GamePath` / `GamePathBuf` identities
  through input, semantic, provenance and cache boundaries; native paths remain
  at physical I/O. Its rebased sources follow the current workspace layout.

See [merge design](merge-design.md), [architecture](architecture.md),
[cache behavior](cache-architecture.md) and [known issues](known-issues.md)
for the current contracts and limits.

## Product acceptance

The fixed Workshop gate contains 14 cases and 26 unique items. No complete
cohort for the current product has been accepted. Installed local availability
must not shrink that denominator.

The latest recorded acceptance attempt timed out in the cold-cache preview,
before the case-scoring phase started. A bounded retry after delta indexing
still timed out with `map/positions.txt` in flight. This remains an acceptance
blocker; fixture success and cache improvements do not establish cohort quality.

The maintainer runs the long gate manually:

```fish
cargo acceptance
```

Acceptance re-parses and semantically scores generated output. It does not
launch EU4 or establish in-game playability. Follow the
[acceptance contract](merge-quality-dataset.md) before interpreting results.

## Distribution

No channel has published a Foch release. The supported binary targets are
`linux-x64` (glibc 2.28), `darwin-arm64` (macOS 11) and `win32-x64`. Every
channel must install a `foch` whose `--version` prints `foch-cli <version>` and
the `cwt-schema` id embedded at the release tag. Installation results are not
merge-quality results. An alpha release does not wait for an accepted cohort
(decided 2026-10-09); its notes state that installing is not merge readiness
and link Product acceptance.

Implemented: maturin bin wheels for PyPI project `foch`, release archives made
from them, the publishable `foch` and `foch-cli` crates, WinGet manifests for
`Acture.Foch`, the `dist.yml`, `release.yml` and `verify-install.yml`
workflows, and `THIRD-PARTY-LICENSES.txt`, the license texts of every crate
linked into `foch`, which the wheels and archives carry.

Checked on darwin-arm64 on 2026-10-08, on the OSS-343 change set over
`4ad1a98` (tree-sitter-paradox gitlink `0e8eab4`): the foch-dev gates and
`foch_dev check`, Rust formatting, strict Clippy and the workspace test build;
`crate-smoke` packaged `foch`, `foch-cli` and `tree-sitter-paradox` 0.3.0 and
installed `foch` from them outside the checkout; a 0.0.1 wheel built with
maturin 1.15.0 passed `twine check --strict`, `dist archive` wrote
byte-identical archives twice, and `dist smoke` installed it offline through
`uvx`, `uv tool install`, `upgrade` (no newer version, so no upgrade path) and
`uninstall`, matching the archive's executable. Each install reported
`foch-cli 0.0.1` and
`cwt-schema 5d636ca3ec1497a27308b712b2601fef9cb8993a14d777824486c6930a7070f3 (embedded)`.
`release preflight --tag v0.0.1` fails on one blocker, after the 2026-10-08
yank of `foch` 0.1.0: crates.io has no `tree-sitter-paradox` 0.3.0.

GitHub CI on [PR #74](https://github.com/Acture/foch/pull/74) at `f765523`
ran `dist.yml` on hosted runners: wheels and archives for `linux-x64`
(manylinux_2_28), `darwin-arm64` and `win32-x64`, each installed through uvx
and uv tool; the out-of-tree crate install; the third-party license check; and
the WinGet smoke with winget v1.29.380, which passed `winget validate` and
installed, upgraded and uninstalled `Acture.Foch` from local manifests. Every
install reported the identity above.

Never run: `release.yml` and `verify-install.yml`, so no GitHub release,
registry upload, release-mode WinGet install or post-publication install has
happened.

The Homebrew tap already offers an unverified `--HEAD` source build of
`master`. It fails from the merge of this source layout until its formula
lists the moved `src/packages/foch/vendor/cwtools-eu4-config` submodule.

Pending maintainer actions, in the [release runbook](RELEASE_CHECKLIST.md):

- publish `tree-sitter-paradox` 0.3.0 by merging
  [Acture/tree-sitter-paradox#12](https://github.com/Acture/tree-sitter-paradox/pull/12)
  after configuring its publishers; until then `foch` cannot be published to
  crates.io;
- update the tap's HEAD formula submodule path when merging this layout
  (OSS-302), and add its token and sync path;
- create the `crates-io` and `pypi` environments and publish variables, and
  register the PyPI pending publisher;
- the first, token-based crates.io publish of `foch-cli`, and the WinGet
  community pull request.

Channel results from `verify-install.yml`: none yet.

## Development verification

The recorded source-layout checkpoint passed Rust formatting, strict workspace
Clippy and the full workspace test suite, as well as the independent fuzz check,
grammar tests, desktop frontend checks/build, VS Code smoke and PyGhidra unit
tests. The documentation follow-up passed the standard commit and push gates;
GitHub CI at `2fc9387` also passed.

A complete source copy without `.git` or private notes successfully installed
the CLI with the same embedded CWT identity. A fresh clone retrieved both public
build submodules at their recorded revisions and resolved all Cargo packages.
These are development checks, not a Workshop acceptance result.

## Evidence and maintenance

Current source, immutable measurements and raw evidence belong in this code
repository. The CLI harness owns its
[measurement data](../src/apps/foch-cli/tests/merge_quality/data/); compact probe
artifacts live in its `probes/` directory. JSONL measurement streams are
append-only and resumable per case. Do not stage, truncate, restore or rewrite
dirty records without first establishing their identity and the user's decision.

`docs/` contains current public guidance. Private research, past verification
reports and superseded status logs belong in the notes repository; public builds
and documentation do not require access to it.

Before selecting work, inspect Git and local input state, then check Linear.
Update this page when a verified current product fact changes. Keep experiment
history in notes and execution status in Linear.
