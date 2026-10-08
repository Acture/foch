# Project Status

Last verified: 2026-10-07, source layout `f73eab9` and documentation follow-up
`2fc9387`. This page records the current product state; it is not a development
log. Active work, dependencies and blockers live in the Linear Foch project.

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
