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

## Combinatorial exploration of installed mods

```text
cargo combination-probe
```

This separate bug-finding entrypoint composes merges from already installed
Workshop mods instead of the fixed cohort. Its universe is the enabled Workshop
mods of the current launcher playset in playset order, or the ordered ids in
`FOCH_COMBO_PROBE_MODS=id1,id2,...`. Nothing is downloaded. Missing and local
mods are listed as unavailable in the run's `universe.json`; they do not affect
the acceptance denominator.

It runs analysis-only `foch merge --non-interactive --review-all` on, in order,
every singleton, the pairs that share a top-level content directory (most shared
paths first), and precedence prefixes doubling from 4 mods up to the whole
universe, the actual goal. Every
combination keeps playset order and includes the declared dependencies its mods
resolve to inside the universe. Panics, CLI errors, timeouts, fatal plan errors
and `engine_failure` units are failures; `FOCH_COMBO_PROBE_FAIL_ON` adds review
dispositions such as `unsupported_input` or `needs_user_choice`. A new failure
signature is delta-debugged (ddmin) to a 1-minimal culprit set and attributed to
a mod, a pair, or a group, with the content family of the failing unit. A
signature already explained by a culprit set inside a later combination is
counted, not minimized again.

The default working directory is `target/combination-probe`
(`FOCH_COMBO_PROBE_DIR`). `outcomes.jsonl` and `minimized.jsonl` are
append-only and keyed by the executable hash, game version and ordered Workshop
`id@manifest` identities, so rerunning the same command resumes: recorded
combinations are not merged again, and a new executable starts a fresh history
beside the old one. `summary.md` and `summary.json` are regenerated after every
merge from the records for the current executable. Each attempt keeps per-case
`foch.toml` and logs under `runs/run-*`.

Budgets keep it bounded: `FOCH_COMBO_PROBE_MAX_RUNS` (new merges per
invocation, minimization included; default 100), `FOCH_COMBO_PROBE_MAX_SIZE`
(largest prefix; default the whole universe), `FOCH_COMBO_PROBE_PHASES`
(`singleton,pair,prefix`), and `FOCH_COMBO_PROBE_TIMEOUT_SECS` per merge
(default 1800). Results are measurement history for investigation, never a
product-quality claim. Turn each reproduced failure into a focused regression
fixture before fixing it at its content-family or kernel boundary.

Ordinary tests cover the pipeline using synthetic installed Workshop inputs and
an injected downloader, while base preparation and merging use the real CLI:

```text
cargo test -p foch-cli --test merge_quality_corpus workshop_probe
```

The full newest-page job can be long; launch the single Cargo command manually.
It then performs every stage without manual manifest generation or log collation.
