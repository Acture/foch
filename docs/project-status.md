# Project Status

Last verified: 2026-10-07, source layout `f73eab9` and documentation follow-up
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
- Content-family transforms run before structural merge on frozen inputs and
  bind the source bytes they read. The culture adapter adapts renamed culture
  identities and applies manifest-reviewed renames and repairs. Generated
  outputs commit or are withheld as one group with their audited inputs.
- `[gui] mode = "decisions"` migrates each mod's scripted custom GUI actions
  into one player-only main decision per mod (panel options, province searches
  with 32 multiplayer player numbers, rebound text and manifest overrides) and
  hides the migrated buttons; `window` and `mixed` are not implemented.
  The output has not been validated in game.
- `common/on_actions` (additive since patch 1.36) and `common/estates` (mods
  extend estates from their own files) stay per path instead of one definition
  module, so the game combines them as it would for the source mods.
- The test-framework worktree, rebased on `95f8554`, implements inline
  `#test(...)` annotations with `#skip`/`#ignore`/`#xfail`/`#mark`, a built-in
  runner that launches the real game, and CLI/LSP annotation help, split into
  `foch-annotation`, `foch-test`, `foch-lsp` and `foch-runner`. Unreleased. The
  built-in runner, AI-off and per-clause checks are verified end-to-end against
  real EU4 1.37.5 on native Windows; the real user directory is kept
  content-identical by backup and restore. Shared sessions, arbitrary start
  dates and hosted CI (Linux + Wine) are unverified; `tests/` blocks are not
  collected by the CLI yet. See [runtime tests](foch-runtime-tests.md) and the
  [framework design](foch-test-framework-design.md); verification detail and
  the dropped isolation investigation are in the private notes. MTTH/natural-
  trigger testing remains recorded product design only.

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
from them, the publishable crates (`foch-cli` and the workspace crates it
builds from: `foch`, `foch-annotation`, `foch-lsp`, `foch-runner`,
`foch-test`), WinGet manifests for `Acture.Foch`, the `dist.yml`, `release.yml`
and `verify-install.yml` workflows, and `THIRD-PARTY-LICENSES.txt`, the license
texts of every crate linked into `foch`, which the wheels and archives carry.

Checked on darwin-arm64 on 2026-10-10, on the OSS-343 change set after
merging master `b97bb19` (tree-sitter-paradox gitlink at its `v0.3.0` tag):
the foch-dev gates and `foch_dev check`, actionlint, Rust formatting, strict
Clippy and the workspace test build; `crate-smoke` packaged the six crates and
`tree-sitter-paradox` 0.3.0 and installed `foch` from them outside the
checkout. The merge made `foch-cli` depend on `foch-annotation`, `foch-lsp`,
`foch-runner` and `foch-test`, so a release publishes all six, and brought the
v2 CWT schema id, which also binds file names. The install reported
`foch-cli 0.0.1` and
`cwt-schema db9ba69682f2b2f924eee1f13cfefde8bfe3d3e8f3552883d0b0a2aefea3b16b (embedded)`.
`tree-sitter-paradox` 0.3.0 is on crates.io (published 2026-10-10 from its
`v0.3.0` tag) and `foch` 0.1.0 was yanked on 2026-10-08, so
`release preflight --tag v0.0.1` passes all 15 checks, noting that five of the
six crates do not exist on crates.io yet. `cargo publish --dry-run --locked`
for the six crates packaged and verified each against the crates.io grammar.

GitHub CI on [PR #74](https://github.com/Acture/foch/pull/74) at `7256b0e`
ran `dist.yml` on hosted runners: wheels and archives for `linux-x64`
(manylinux_2_28), `darwin-arm64` and `win32-x64`, each installed through uvx
and uv tool; the out-of-tree install from the six packaged crates; the
third-party license check; and the WinGet smoke with winget v1.29.380, which
passed `winget validate` and installed, upgraded and uninstalled `Acture.Foch`
from local manifests. Every install reported the identity above.

Never run: `release.yml` and `verify-install.yml`, so no GitHub release,
registry upload, release-mode WinGet install or post-publication install has
happened.

The Homebrew tap already offers an unverified `--HEAD` source build of
`master`. It fails from the merge of this source layout until its formula
lists the moved `src/packages/foch/vendor/cwtools-eu4-config` submodule.

Configured: the `crates-io` and `pypi` environments with the maintainer as
required reviewer, `PYPI_PUBLISH`, the PyPI pending publisher for `foch`
(`release.yml`, environment `pypi`) and the `HOMEBREW_TAP_TOKEN` secret.

Pending maintainer actions, in the [release runbook](RELEASE_CHECKLIST.md):

- merge this layout together with the tap's HEAD formula submodule path
  (branch `fix/foch-cwt-submodule-path` in `Acture/homebrew-ac`, OSS-302);
- tag `v0.0.1` and approve the `pypi` environment when `release.yml` asks;
- the first, token-based crates.io publish, which creates five crates
  (`foch-cli`, `foch-annotation`, `foch-lsp`, `foch-runner`, `foch-test`), then
  their trusted publishers and `CRATES_IO_PUBLISH`;
- the WinGet community pull request from `Acture/winget-pkgs`.

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

The 2026-10-07 built-in-runner work passed the full `cargo test --workspace`
suite, strict Clippy and rustfmt, and the repository contracts; the built-in
runner was verified against real EU4 1.37.5 on native Windows (`check`
isolation). These are development checks, not a hosted CI or Workshop
acceptance result. Verification detail and the 2026-10-02 protocol-v1
checkpoint are recorded in the private notes.

A bounded real `foch merge` of Europa Expanded (2164202838) and Religions and
Cultures Expanded (3342969370) on 2026-10-09, with `[gui] mode = "decisions"`
and an isolated parse cache, reported `partial_success` with no review units:
one `unsupported_input` (EE's stray `}`, OSS-359) and one `engine_failure` (EE
places `quebecois` in a second culture group, OSS-384). It wrote the GUI
decisions. This is a development check, not an acceptance result or in-game
validation.

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
