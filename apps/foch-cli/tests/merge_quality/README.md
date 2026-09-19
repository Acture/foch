# Product merge acceptance

This directory is test-only support for the public `foch` executable. It is
compiled only by `tests/merge_quality_corpus.rs`; there is no merge-quality
library or product binary.

The fixed denominator is `fixtures/workshop-product-cases-v2.json`: 14 logical
cases and 26 unique Steam Workshop items. `fixtures/CREDITS.md` records their
provenance. The non-ignored contract test pins both counts and the manifest's
exact digest, so unavailable local Workshop items cannot silently reduce the
cohort.

The files below `data/` are append-only evidence streams:

- `input_versions.jsonl`
- `observations.jsonl`
- `measurements.jsonl`
- `file_results.jsonl`

Never truncate, reorder, normalize, or rewrite their existing bytes. An
interrupted cohort is valid history, but it is not an accepted baseline.
Generated compact evidence objects and work directories are ignored by Git.

The maintainer entrypoint is:

```text
cargo acceptance
```

The repository Cargo alias runs a Rust orchestrator inside this test harness.
It first runs the cache-residency gate, then the complete fixed cohort in separate
processes, clearing cache-cap overrides and stopping at the first failure. It
requires no shell-specific runtime. Both stages are long, real-Workshop tests
and should be launched manually.

## Automatic newest-page exploration

```text
cargo workshop-probe
```

This separate Rust integration-test entrypoint fetches the newest EU4 Workshop
page, freezes its 30 items in page order, downloads unavailable items with
SteamCMD, verifies every source against its paired ACF, prepares matching base
data if necessary, and invokes Cargo's exact `foch merge` executable. Merge
analyzes before committing safe units. Source trees are read in place; the
generated mod and Launcher stub stay in the probe's output directory.

The default working directory is `target/workshop-probe`. It retains
`selection.json`, download locations, and a fresh `runs/run-*` directory per
attempt. Repeating the same command reuses that selection, installed inputs, and
matching base data, and attempts the merge again with the current executable.
Use `FOCH_WORKSHOP_PROBE_DIR` for a new selection or an independent run; concurrent
jobs cannot share the same directory. The fixed acceptance cohort is unchanged.

SteamCMD must be installed. `FOCH_STEAMCMD` can specify its executable.
The job automatically selects the most-recent remembered account with automatic
login enabled in Steam's `config/loginusers.vdf`, or the sole eligible account.
`FOCH_STEAM_ACCOUNT` overrides that choice, including explicit `anonymous` use.
Missing or ambiguous account metadata produces an actionable error when downloads
are needed; it never silently switches to anonymous. Desktop login metadata
selects an account but does not establish that SteamCMD has valid credentials.
Authenticate in SteamCMD once if necessary; ordinary subsequent runs need only
`cargo workshop-probe`. Reauthenticate if Steam expires or revokes its cached login.
The job reuses SteamCMD's cached credentials, never requests a password on stdin,
and records login/download failures. It does not store passwords. Downloads,
base preparation, and merge each have a 30-minute timeout and progress logging.
Each progress heartbeat includes the latest stderr line; structural-file logs
identify the path before analysis and record its elapsed time afterwards. The
heartbeat is subprocess liveness, not proof that a merge unit has completed.

Each attempt writes `report.json`, including a failed stage and error on failure.
Successful input preparation writes the ordered `foch.toml`, ACF-based
`input.json`, and `descriptors.json` with declared dependencies and resets.
After a completed merge, `gaps.json` lists deferred paths/reasons, validation
findings, dependency misuse, and version mismatches. The generated mod retains
the normal plan, merge report, and contribution provenance. Input identities and
the executable are checked again after the merge. Missing inputs never shrink
the selection; a SteamCMD success exit code is insufficient.

Engine failures and invalid generated syntax fail the job. Unsupported inputs
and genuine user-choice conflicts remain inspectable results. A completed probe
is a discovery result, not a semantic correctness score or in-game test.

Ordinary tests cover the pipeline using synthetic installed Workshop inputs and
an injected downloader, while base preparation and merging use the real CLI:

```text
cargo test -p foch-cli --test merge_quality_corpus workshop_probe
```

The full newest-page job can be long; launch the single Cargo command manually.
It then performs every stage without manual manifest generation or log collation.
