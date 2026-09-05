---
goal: Remove the foch-mq binary and integrate merge-quality into libraries, product tests, and scoped maintenance workflows
version: 1.0
date_created: 2026-08-06
last_updated: 2026-08-08
owner: foch
status: Superseded
tags: [architecture, refactor, testing, merge-quality, cli, dataset]
---

# Introduction

![Status: Superseded](https://img.shields.io/badge/status-superseded-orange)

> **Superseded 2026-08-08:** The binary/package integration work remains
> historical context, but its private input-CAS and fixed 23-snapshot product
> acceptance contract is retired. Product acceptance now follows
> `refactor-workshop-backed-merge-quality-inputs-1.md`: a fixed 14-case logical
> corpus resolved read-only from Steam Workshop plus `appworkshop_236850.acf`,
> with no full input-tree archival or input-CAS preflight.
>
> **Do not use the commands, scripts, APIs, task gates, or acceptance criteria
> below as operating instructions.** They are retained only as the decision
> record for removing the `foch-mq` binary. Acquisition, corpus/fixture refresh,
> fixture acceptance, review-pack, and semantic/full payload-export paths have
> since been deleted. The only current product gate is
> `cargo acceptance`; `export.fish` is metadata-only.

Delete the standalone `foch-mq` Cargo binary instead of renaming or embedding it as a `foch mq` product command. Keep `crates/foch-merge-quality` as a private, library-only package that owns deterministic scoring, immutable dataset records, reports, and evidence verification. Put product-quality assertions in integration tests that execute the real `foch` binary. Put state-changing corpus maintenance behind fixed Fish scripts that invoke explicitly named ignored tests; do not recreate a general-purpose hidden CLI inside libtest.

The migration corrects a current semantic error before changing packaging. The existing `lifecycle::measure` path reaches `orchestrate::score_case_from_paths_with_cache`, which calls `score::run_merge`; that function selects `MergeEvaluationKernel::AddressPatchReference`. Therefore the committed scorer `1.0.0` cohort and the current dirty scorer `1.3.0` cohort are Legacy address-patch measurements, not product SemanticTree measurements. They remain immutable historical evidence and must never be relabeled as TreeMerge or reused as the new product baseline.

The target execution boundary is:

```text
committed synthetic fixtures
        |
        +------> ordinary Rust unit/integration tests
        |
frozen private corpus snapshots
        |
        v
foch-cli ignored acceptance test
        |
        +------> exact CARGO_BIN_EXE_foch bytes
        |             |
        |             v
        |       public `foch merge --non-interactive`
        |             |
        |             v
        |       generated tree + .foch/foch-merge-report.json
        |
        v
foch-merge-quality library
score_existing_output -> append V2 measurement -> cohort report -> assertions
```

The final Cargo target surface contains one normal binary target, `foch`. `foch lsp` remains a subcommand of that binary. `parse_stats` and `symbol_dump` become opt-in Cargo examples. Full corpus measurement remains manual and ignored because it requires private local EU4 data, the local object store, and long execution time.

## 0. Command Disposition

The existing 27 leaf commands are not migrated one-for-one.

| Existing command | Final owner | Final disposition |
|---|---|---|
| `collect` | `lifecycle::collect` plus ignored maintenance test | Keep library API; invoke through `scripts/merge-quality/refresh-corpus.fish`. |
| `measure` | product corpus acceptance test plus `MeasurementRunner` | Delete command and self-spawn protocol. |
| `report` | pure dataset report API | Keep API; product acceptance writes both pinned reports. |
| `baseline` | acceptance script | Delete composite command; script runs the exact ignored test. |
| `export` | deterministic export API plus ignored maintenance test | Keep API; invoke through `scripts/merge-quality/export.fish`. |
| `run` | product acceptance test | Delete obsolete Legacy live pipeline. |
| `learn` | resolution classification library | Delete command; measurement records already store per-file human resolution. |
| `symbols` | separate full-local evidence test | Keep separate from overlap scoring; invoke through a fixed script. |
| `score-one` | none | Delete with `run`. |
| `measure-one` | none | Delete; the parent test launches `foch` directly and scores its output. |
| `semantic-diff` | scoring library tests/API | Delete command; retain pure semantic comparison. |
| `shadow-compare` | pinned Legacy evidence | Delete live dual-kernel command. |
| `shadow-case` | Structured rollout acceptance | Fold into the parameterized ignored rollout test. |
| `shadow-corpus` | Structured rollout acceptance | Convert to an ignored test using pinned Legacy outputs. |
| `common-probe` | Common-family acceptance | Convert to a fixed ignored test. |
| `knowledge snapshot` | none in foch | Delete; any future wiki acquisition belongs in a separately approved research package. |
| `knowledge verify` | none in foch | Delete with the unused knowledge subsystem. |
| `knowledge search` | none in foch | Delete with the unused knowledge subsystem. |
| `review-pack freeze-baseline` | none | Delete; the frozen Legacy baseline is an immutable committed artifact. Retain only review-pack build and verify. |
| `review-pack build` | injected runner plus ignored evidence test | Remove the default self-spawn runner. |
| `review-pack verify` | verifier API and acceptance test | Keep verification; it must not mutate annotations. |
| `review-pack show` | report JSON plus `jq` | Delete command. |
| `shadow-run-one` | none | Delete hidden worker protocol. |
| `extract-fixtures` | ignored fixture-maintenance test | Invoke through `scripts/merge-quality/refresh-fixtures.fish`. |
| `discover` | ignored Steam acquisition test | Keep feature-gated library API; invoke through `scripts/merge-quality/acquire.fish`. |
| `fetch` | ignored Steam acquisition test | Keep feature-gated library API; invoke through the same script. |
| `all` | none | Delete unsafe composite operation. |

## 1. Requirements & Constraints

- **REQ-001**: Delete `crates/foch-merge-quality/src/bin/foch_mq.rs`, its `[[bin]]` manifest target, and every active `foch-mq` command invocation.
- **REQ-002**: Do not add `foch mq`, a replacement `xtask` binary, a custom Cargo test harness, or another installed executable.
- **REQ-003**: Product merge-quality acceptance must execute the exact `foch` binary exposed by `env!("CARGO_BIN_EXE_foch")` and must invoke the public non-interactive merge path.
- **REQ-004**: The scorer must accept an already generated output tree and parsed `MergeReport`; scoring code must not call `run_merge_for_evaluation` or select a merge kernel.
- **REQ-005**: Each corpus case must execute in an independent `foch` child process with timeout, kill, signal classification, and bounded stderr retention. Persist a non-Completed terminal row immediately after classification. For Completed, first validate the summary and validate/persist the output-CAS/object record and file-result evidence, then append the terminal measurement row carrying that summary last. Finish that sequence before starting the next case.
- **REQ-006**: Ordinary CI must cover a small hermetic `foch`-to-scorer seam. The private 23-case product corpus must remain an explicit ignored acceptance test.
- **REQ-007**: Preserve all existing JSONL and CAS bytes. Do not rewrite scorer `1.0.0` or `1.3.0` records, measurement IDs, file-result foreign keys, or output objects.
- **REQ-008**: Historical reports must identify scorer `1.0.0` and `1.3.0` as `legacy_address_patch_reference`; no report or documentation may call them Structured or TreeMerge.
- **REQ-009**: New product measurements and reports must use schema `2.0.0`, scorer version `2.0.0`, `runner_protocol_version = foch-cli-merge-report-v2`, the actual `foch` BLAKE3 artifact hash, `merge_kernel = semantic_tree`, and `scope = full_product_merge`.
- **REQ-010**: Report selection must use a complete stable cohort ID. Selecting only a scorer version must fail when more than one cohort matches.
- **REQ-011**: `crates/foch-merge-quality` must be `publish = false`, have no binary target, and expose only APIs with a current test, acceptance workflow, or evidence consumer.
- **REQ-012**: `parse_stats` and `symbol_dump` must cease to be Cargo `bin` targets and remain available only as `dev-tools`-gated examples.
- **REQ-013**: State-changing maintenance workflows must use fixed repository paths, require explicit ignored-test invocation, validate their outputs, and avoid a generic argument parser that recreates `foch-mq` under another name.
- **REQ-014**: Legacy reference execution may remain only behind fixed library tests and pinned evidence. It must not be the default product quality gate.
- **REQ-015**: Every terminal measurement, including MergeFailed, Crashed, TimedOut, and Fatal, is immutable cached evidence. Resume must count a same-identity non-Completed row as cached and failed without calling the runner. Retrying requires a new identity input, normally a new engine artifact or scorer configuration (including timeout), and therefore a new cohort; never delete, overwrite, or reorder canonical JSONL to retry.
- **REQ-016**: The Steam acquisition gate proves only discovery/download integrity through its manifest and checksums. Product quality is established separately by `structured_rollout_acceptance` and the TASK-033 delta classification.
- **SEC-001**: Steam credentials, local EU4 paths, private base snapshots, Workshop payloads, and ignored CAS content must never enter logs, tracked fixtures, or public exports.
- **CON-001**: Preserve the current dirty worktree. The security lock update, control-flow repair, and scorer `1.3.0` dataset additions must be resolved as separate logical changes before moving files.
- **CON-002**: Keep the canonical dataset at `crates/foch-merge-quality/dataset/` during this refactor. Moving 13.61 GiB of ignored CAS data is a separate storage migration, not part of binary removal.
- **CON-003**: Do not run the full 23-case corpus during ordinary phases. Run focused fixtures first and perform one fresh product cohort only at the final acceptance gate.
- **CON-004**: A non-zero `foch merge` exit is not automatically a crashed measurement. If a parseable non-fatal merge report exists, score the emitted/withheld output and preserve its product status.
- **CON-005**: Do not introduce backwards-compatibility aliases for removed commands. Historical data compatibility is implemented as explicit versioned records, not command shims.
- **GUD-001**: Use trait-style composition. Model process execution as `MeasurementRunner`; model scoring as pure functions over paths, reports, and immutable case metadata.
- **GUD-002**: Use regression-first changes. Every command deletion follows a passing test or fixed script that owns the retained behavior.
- **GUD-003**: Keep long-running progress at INFO/stderr with case count, elapsed time, ETA, cache hits, and terminal counts.
- **PAT-001**: Follow the existing `review_pack::StructuredKernelRunner` injection pattern and remove its current default self-spawn implementation.
- **PAT-002**: Keep dependency direction `foch-core` / `foch-language` / `foch-engine` <- `foch-cli`, with `foch-merge-quality` used only as a private evaluation library and `foch-cli` depending on it only through `dev-dependencies`.

## 2. Implementation Steps

### Implementation Phase 1: Freeze and correct the historical contract

- **GOAL-001**: Establish an accurate, immutable migration baseline before changing execution or files.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Record the exact current `git status`, the BLAKE3 and line/object counts for tracked dataset streams, and the two complete V1 cohort triples. Confirm no measurement process is running. Do not edit or reorder JSONL. | ✅ | 2026-08-06 |
| TASK-002 | Freeze the historical execution contract as Legacy evidence, then delete the merge-quality package's live evaluator and ambiguous scoring test surface. Product regressions prove the public command reports `semantic_tree` / `full_product_merge` execution attestation. | ✅ | 2026-08-06 |
| TASK-003 | Add `crates/foch-merge-quality/dataset/measurement-cohorts.json` with exact entries for the known scorer `1.0.0` and `1.3.0` V1 triples. Each entry must state `identity_kind = orchestrator_bound_v1` and `merge_kernel = legacy_address_patch_reference`. | ✅ | 2026-08-06 |
| TASK-004 | Correct `docs/project-status.md`, `docs/merge-quality-dataset.md`, and the Notion project page so scorer `1.0.0`/`1.3.0` are historical Legacy baselines. Remove every current claim that the completed `1.3.0` 23-case measurement is a TreeMerge product baseline. | ✅ | 2026-08-06 |

Completion criteria:

- The two existing cohorts remain byte-identical and reportable.
- A failing regression prevents Legacy output from being labeled product `semantic_tree` / `full_product_merge` evidence.
- The refactor does not begin until the pre-existing dirty changes are separated from refactor edits.

Execution checkpoint after separating the pre-existing work into commits `caaa3c8`, `ab062ad`, and `b8a8880`:

- No `foch-mq`, `foch merge`, or corpus-measurement process was running.
- `measurements.jsonl`: BLAKE3 `bf7d4fd1b01389bd67f8cf430fc7441c3991bbb309f6e4f1c094861c4bdec255`; 46 JSON records (47 physical lines because the file begins with a blank line).
- `file_results.jsonl`: BLAKE3 `1bae838fabbf0989843c0c6ac5b159a05a622415522ecb1b9140044e3743f811`; 60,220 JSON records (60,221 physical lines).
- `object_records.jsonl`: BLAKE3 `6db356013a6d8f873bb83ce3bbae0d600ef9c9dd47e634ffee7d9eb68c46590e`; 120 JSON records (121 physical lines).
- V1 cohort A: scorer `1.0.0`, executable `16fcde0535ad3c759492f1aa76ad6164d466cb6fea8125a65f36c3bebb06ea91`, config `e2580bc8c745bf7aca520ce909f093028455a9745d5fae6f92b94424d2986393`, 23/23 completed snapshots.
- V1 cohort B: scorer `1.3.0`, executable `0507a19de246a59bd2f718ad2941fd4d0c9ec07d469ab911a1e6b04bb11ba519`, config `8beffefe06b044798b769b805fb556dd93769ebdbf367df3d6468ef6834d5665`, 23/23 completed snapshots.
- Integrity audit: 46 unique measurement IDs, zero dangling file-result measurement references, and zero missing merged-output content hashes.

### Implementation Phase 2: Version measurements and make cohort selection explicit

- **GOAL-002**: Read V1 and V2 measurements together without rewriting historical records or silently selecting the wrong cohort.
- Dependency: GOAL-001.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-005 | In `crates/foch-merge-quality/src/dataset.rs`, replace the single measurement wire struct with an internally tagged `MeasurementRecord` enum containing byte-compatible schema `1.0.0` V1 fields and schema `2.0.0` V2 fields. Keep object, snapshot, observation, file-result, annotation, manifest, and CAS schemas unchanged. | ✅ | 2026-08-06 |
| TASK-006 | Define V2 identity as `snapshot_id + engine_artifact(kind, algorithm, digest) + runner_protocol_version + merge_kernel + scope + scorer_version + scorer_config_hash`, using the `measurement-v2` stable-ID namespace. Exclude runner/test binary hash, runner path, output path, timestamps, and report renderer implementation. | ✅ | 2026-08-06 |
| TASK-007 | Replace the global-current-scorer filter in `lifecycle::report` with `MeasurementCohortKey` and stable `cohort_id` grouping. Add exact cohort selection; return an ambiguity error listing cohort IDs when a partial selector matches more than one cohort. | ✅ | 2026-08-06 |
| TASK-008 | Bump report output schema to `2.0.0`. Render identity kind, artifact kind/algorithm/digest, `runner_protocol_version`, scorer version/config hash, `merge_kernel`, and `scope` in JSON and Markdown. Resolve V1 kernel labels only through `measurement-cohorts.json`; reject unknown V1 triples rather than guessing. | ✅ | 2026-08-06 |
| TASK-009 | Update review-pack and file-result joins to use version-independent measurement getters, preserve every pinned V1 binding, and delete the now-unconsumed corpus-shadow implementation while retaining its immutable data stream. | ✅ | 2026-08-06 |

Completion criteria:

- A mixed V1/V2 JSONL round-trips while the original V1 byte prefix remains unchanged.
- Explicit reports can reproduce both existing 23-case cohorts.
- Every V2 identity input changes the measurement ID; operational runner details do not.

### Implementation Phase 3: Separate execution from scoring

- **GOAL-003**: Turn `foch-merge-quality` into a deterministic evaluation library that never chooses or executes a merge kernel.
- Dependency: GOAL-002.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-010 | Extract `score_existing_output_with_cache` in `crates/foch-merge-quality/src/orchestrate.rs`; keep `score.rs` limited to scoring primitives. Its request contains the case, ordered source roots, compatch root, optional base-game root, generated output root, and parsed `MergeReport`; it returns ordered file records, resolution evidence, verdict counts, and scoring timing. | ✅ | 2026-08-06 |
| TASK-011 | Remove merge execution, the 512 MiB merge thread, `MergeEvaluationKernel`, playset construction, and `run_merge_for_evaluation` imports from the pure scorer. Keep Legacy execution only in an explicitly named test/reference module while pinned reference evidence remains required. | ✅ | 2026-08-06 |
| TASK-012 | Define `MeasurementRunner` and typed `MeasurementRequest` / `TerminalMerge` contracts in `lifecycle.rs`. Refactor `measure` into `measure_with_runner`, leaving dataset verification, resume, persistence, output archival, and reporting independent of process implementation. Delete `WorkerOutput`, `measure_one`, `run_measurement_child`, `measurement_child_command`, `score_one`, and all `std::env::current_exe()` self-spawn behavior. | ✅ | 2026-08-06 |
| TASK-013 | Make immutable input-CAS verification a preflight boundary: corrupt or missing selected objects must fail before calling the runner or persisting any V2 measurement, file-result, or object-evidence row. Identity/measurement ID may be computed purely for cache lookup; that computation is not persistence. Prove the fake runner receives no request and no evidence row is appended. | ✅ | 2026-08-06 |
| TASK-014 | Refactor `review_pack.rs` to require an injected runner. Keep `StructuredKernelRunner` and `build_review_pack_with_runner`; delete `IsolatedStructuredKernelRunner` and the default build path that invokes `shadow-run-one`. | ✅ | 2026-08-06 |

Completion criteria:

- `rg 'current_exe|measure-one|score-one|shadow-run-one' crates/foch-merge-quality/src` finds no active hidden child protocol.
- Re-scoring pinned Legacy outputs through the extracted scorer reproduces ordered file records, verdict totals, and semantic hashes exactly.
- Corrupt-input preflight appends no V2 measurement, file-result, or object-evidence row and does not call the injected runner; pure identity computation for cache lookup is allowed.
- Non-Completed outcomes commit their immutable terminal row immediately. Completed outcomes commit verified output/evidence first and their terminal row last, before the next case starts.
- Resume treats same-identity terminal failures as cached failures; only a changed identity input creates a retry cohort.

### Implementation Phase 4: Add the real product acceptance seam

- **GOAL-004**: Make merge quality test the same public `foch merge` path users execute.
- Dependency: GOAL-003.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-015 | Add `foch-merge-quality` as a dev-dependency of `crates/foch-cli`; do not add it to production dependencies. Create `crates/foch-cli/tests/merge_quality/runner.rs` implementing `MeasurementRunner` around `env!("CARGO_BIN_EXE_foch")`. | ✅ | 2026-08-06 |
| TASK-016 | For each case, restore immutable source/compatch roots, write a temporary playset and isolated `FOCH_CONFIG_DIR/config.toml`, run public `foch merge <playset> --out <dir> --non-interactive`, enforce timeout/kill, rehash the executable after completion, and parse `.foch/foch-merge-report.json`. | ✅ | 2026-08-06 |
| TASK-017 | Add a non-ignored tiny product/scorer seam test in `crates/foch-cli/tests/merge_quality_corpus.rs`. It must launch real `foch`, assert `semantic_tree` / `full_product_merge` execution attestation, score the output through the library, and detect a deliberate expected-verdict mismatch. | ✅ | 2026-08-06 |
| TASK-018 | Add an ignored six-case `product_fixture_acceptance` that creates a new product baseline artifact distinct from `legacy-baseline.json` and `expected.json`. Preserve the existing Legacy fixture under an explicit historical name. | ✅ | 2026-08-06 |
| TASK-019 | Add ignored `full_product_corpus_acceptance` for the fixed 23 snapshot IDs. Use measurement/report schema `2.0.0`, scorer version `2.0.0`, `runner_protocol_version = foch-cli-merge-report-v2`, `merge_kernel = semantic_tree`, and `scope = full_product_merge`; start a fresh 0/23 V2 cohort and never reuse V1 cache entries. | ✅ | 2026-08-06 |

Completion criteria:

- The hermetic seam runs in ordinary workspace CI.
- The six-case product and Legacy artifacts cannot be confused by type, path, filename, or report label.
- Product acceptance never calls the retained-path Legacy evaluator.

### Implementation Phase 5: Convert retained workflows and delete obsolete ones

- **GOAL-005**: Preserve only workflows with a named owner and eliminate the 27-command kitchen-sink surface.
- Dependency: GOAL-004.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-020 | Convert `shadow-corpus` into ignored `structured_rollout_acceptance` against pinned Legacy CAS outputs. Fold `shadow-case` filtering into fixed test fixtures. Delete live `shadow-compare` and `shadow-run-one` execution paths after parity tests pass. | ✅ | 2026-08-06 |
| TASK-021 | Convert `common-probe` into ignored `common_module_acceptance` with the fixed case/family matrix and measurable denominator assertions. Keep overlap scoring and full-local symbol evidence as separate tests and reports. | ✅ | 2026-08-06 |
| TASK-022 | Treat the frozen Legacy baseline and selection as immutable committed artifacts; expose no freeze/regeneration API. Keep only explicit review-pack build and verify using the injected product runner. Split proposal generation from annotation recording; no test may append `annotations.jsonl` implicitly. Delete `review-pack show`. | ✅ | 2026-08-06 |
| TASK-023 | Add fixed ignored maintenance tests for collect, deterministic export, fixture refresh, full-local symbols, and Steam discover/fetch. Each test must require its prerequisites, use repository-owned output locations, validate the resulting manifest/checksums, and perform no operation when run without `--ignored --exact`. Steam acquisition attests integrity only; `structured_rollout_acceptance` and TASK-033 own quality classification. | ✅ | 2026-08-07 |
| TASK-024 | Add thin Fish entrypoints under `scripts/merge-quality/` for acceptance, corpus refresh, export, fixture refresh, review pack, symbol evidence, and Steam acquisition. Each script invokes exactly one named test/workflow and contains no scoring, dataset, or network logic. | ✅ | 2026-08-06 |
| TASK-025 | Delete obsolete `run`, `learn`, `baseline`, `all`, live dual-kernel orchestration, and the entire unused knowledge command/module/documentation set. Remove dependencies and Cargo features that have no remaining library/test consumer. | ✅ | 2026-08-06 |

Completion criteria:

- Every retained capability has a specific test or script owner listed in Section 0.
- No script accepts a generic subcommand that recreates the deleted CLI.
- `fish -n scripts/merge-quality/*.fish` succeeds.

### Implementation Phase 6: Remove all non-product binary targets

- **GOAL-006**: Make `foch` the only Cargo binary target while keeping opt-in developer diagnostics available as examples.
- Dependency: GOAL-005.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-026 | Add `publish = false` to `crates/foch-merge-quality/Cargo.toml`; delete its `[[bin]]` target and bin-only `clap` dependency; delete `src/bin/foch_mq.rs`. | ✅ | 2026-08-06 |
| TASK-027 | Move `crates/foch-cli/src/bin/parse_stats.rs` and `symbol_dump.rs` to `crates/foch-cli/examples/`. Replace their `[[bin]]` entries with `[[example]]` entries requiring `dev-tools`, and update their verified invocation docs. | ✅ | 2026-08-06 |
| TASK-028 | Change `scripts/render_homebrew_formula.sh` from `--bins` to `--bin foch`. Update `scripts/test_render_homebrew_formula.py` to require the explicit target and reject `--bins`. Keep the existing release workflow's explicit `--bin foch`. | ✅ | 2026-08-06 |
| TASK-029 | Add a CI metadata invariant that parses Cargo metadata correctly via `.targets[]."required-features"` and asserts the complete `kind == "bin"` set is exactly `foch-cli/foch`. Keep `cargo test --workspace` and strict all-target/all-feature Clippy. | ✅ | 2026-08-06 |
| TASK-030 | Remove stale `foch_lsp`, `foch-mq`, `score-one`, `measure-one`, and `shadow-run-one` ignore rules, examples, and active documentation. Do not edit or commit the untracked local `AGENTS.md`; project documentation carries the durable boundary. | ✅ | 2026-08-06 |

Completion criteria:

- Cargo metadata reports exactly one `bin` target: package `foch-cli`, target `foch`.
- Normal build, Homebrew, release archive, and VS Code packaging all use the same `foch` executable.
- No active source, manifest, workflow, or command documentation mentions `foch-mq`.

### Implementation Phase 7: Validate and establish the first product cohort

- **GOAL-007**: Close the migration with full gates and a separately identified product baseline.
- Dependency: GOAL-006.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-031 | Run formatting, strict workspace Clippy, workspace tests, Steam-feature tests that still have consumers, Fish syntax checks, Homebrew renderer tests, Cargo metadata invariants, and `git diff --check`. Fix every failure before corpus execution. | ✅ | 2026-08-06 |
| TASK-032 | Hand off the exact Fish command for the long-running release-profile `full_product_corpus_acceptance`. Require 23 unique snapshots, 23 terminal records, zero failed terminal records, and complete scorable/all-candidate reports before accepting the cohort. | ✅ | 2026-08-06 |
| TASK-033 | Compare product V2/scorer `2.0.0` against the historical V1/scorer `1.3.0` Legacy cohort by case and scoring unit. Classify every delta as product-kernel change, full-versus-retained scope change, scorer change, or defect; do not overwrite expected artifacts automatically. | | |
| TASK-034 | Update `docs/project-status.md`, `docs/merge-quality-dataset.md`, the implementation plan status, and the Notion project page with the exact product cohort ID, artifact hash, validation results, accepted deltas, blockers, and next failure-ranked merge-quality slice. | | |

Automated implementation checkpoint (2026-08-06; hardened 2026-08-07):

- `cargo fmt --all --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --locked --release -p foch-cli --test merge_quality_corpus --no-run`
- default and `steam` maintenance targets compiled without running ignored workflows
- the non-ignored public `foch` CLI-to-scorer seam passed
- Cargo binary metadata, Homebrew rendering, Fish syntax, Actionlint, Ruff,
  `ty`, grammar tests, VS Code smoke, and `git diff --check` passed
- no full corpus, Steam acquisition, six-case fixture acceptance, review-pack
  acceptance, or other ignored long-running workflow was executed
- post-implementation review hardened snapshot and V2 file-result identities,
  cached input-CAS verification, deterministic crash-window replay, a pinned
  product/scorer base view, exact scorer `1.3.0` review-pack selection, portable
  path-free attestations, and fail-closed Structured evidence validation
- final audit pinned the exact 23 candidate snapshots and six scorable snapshots,
  made V1 byte/FK regressions append-safe for V2, renamed the V2 wire field to
  `runner_protocol_version`, required full payload verification before seeding a
  v2 CAS stamp, removed the unused Legacy-freeze API, and made cached terminal
  failures visible in resume accounting
- follow-up audit closed the missing Steam acquisition contract with one typed
  corpus selection plan, confirmed-download accounting for newly needed items,
  deterministic local tree manifests/checksums, and a full post-write tree audit;
  this attests local acquisition integrity, not Steam remote freshness
- canonical measurement state remains 46 V1 records and zero V2 records; the
  long-running product cohort and TASK-033/TASK-034 remain pending

Completion criteria:

- All automated gates pass.
- A complete V2 product cohort exists and is reported separately from both V1 Legacy cohorts.
- The branch contains no `foch-mq` binary or compatibility wrapper.
- The next merge-quality repair is ranked from product output, not Legacy output.

## 3. Alternatives

- **ALT-001**: Keep `foch-mq` but mark it `publish = false`. Rejected because it preserves the oversized command surface, self-spawn coupling, and misleading product-quality boundary.
- **ALT-002**: Move every command under `foch mq`. Rejected because research acquisition, dataset mutation, and evidence tooling do not belong in the user product CLI.
- **ALT-003**: Replace `foch-mq` with an `xtask` binary. Rejected because it still creates a second general-purpose executable and encourages one-for-one command migration instead of deleting dead workflows.
- **ALT-004**: Implement the full corpus as only a library test that runs the engine in-process. Rejected because it would not test the actual product executable and one crash could abort the entire cohort.
- **ALT-005**: Rewrite the scorer and dataset pipeline in Python. Rejected because it would duplicate typed Rust semantic policies and invalidate reproducibility without reducing the underlying evaluation complexity.
- **ALT-006**: Rename or move the entire `foch-merge-quality` crate and 13.61 GiB CAS during binary removal. Rejected for this plan because packaging, identity, and storage migration have different failure modes.

## 4. Dependencies

- **DEP-001**: The current security lock update, control-flow repair, and scorer `1.3.0` dataset additions must be separated from the refactor worktree before TASK-003 or any file move.
- **DEP-002**: The committed V1 JSONL, local ignored object store, installed EU4 base snapshot, and exact 23 snapshot IDs are required for final product acceptance.
- **DEP-003**: `foch` must continue writing a complete parseable `MERGE_REPORT_ARTIFACT_PATH` for Ready, PartialSuccess, and Blocked outcomes.
- **DEP-004**: The existing `StructuredKernelRunner` abstraction and CLI integration helpers provide patterns for runner injection, environment isolation, and subprocess testing.
- **DEP-005**: `plan/architecture-tree-native-merge-1.md`, ADR 0002, and the Notion merge-corpus page define the production `semantic_tree` / `full_product_merge` and historical Legacy evidence boundary.

## 5. Files

- **FILE-001**: `Cargo.toml` — workspace target invariant context; no replacement MQ binary member.
- **FILE-002**: `crates/foch-merge-quality/Cargo.toml` — private library-only manifest and dependency cleanup.
- **FILE-003**: `crates/foch-merge-quality/src/bin/foch_mq.rs` — delete after all retained behavior has an owner.
- **FILE-004**: `crates/foch-merge-quality/src/dataset.rs` — V1/V2 measurement records and cohort identities.
- **FILE-005**: `crates/foch-merge-quality/src/lifecycle.rs` — runner injection, resume, report selection, and export APIs.
- **FILE-006**: `crates/foch-merge-quality/src/score.rs` — pure output scoring and explicit Legacy reference isolation.
- **FILE-007**: `crates/foch-merge-quality/src/orchestrate.rs` — remove live Legacy execution and hidden workers.
- **FILE-008**: `crates/foch-merge-quality/src/shadow.rs` and `src/corpus_shadow.rs` — pinned Legacy versus product rollout tests and schema bumps.
- **FILE-009**: `crates/foch-merge-quality/src/review_pack.rs` — injected runner and non-mutating verification.
- **FILE-010**: `crates/foch-merge-quality/src/common_probe.rs` and `src/symbols.rs` — fixed acceptance/evidence APIs.
- **FILE-011**: `crates/foch-merge-quality/src/knowledge.rs` and related wiki modules — remove or move out of foch after confirming no consumer.
- **FILE-012**: `crates/foch-merge-quality/tests/` — V1/V2, pure scorer, fake runner, maintenance, rollout, common, review, and fixture tests.
- **FILE-013**: `crates/foch-merge-quality/dataset/measurement-cohorts.json` — explicit historical V1 semantics.
- **FILE-014**: `crates/foch-cli/Cargo.toml` — merge-quality dev-dependency and example target declarations.
- **FILE-015**: `crates/foch-cli/tests/merge_quality_corpus.rs` and `tests/merge_quality/` — actual product runner and acceptance tests.
- **FILE-016**: `crates/foch-cli/src/bin/parse_stats.rs` and `symbol_dump.rs` — move to examples.
- **FILE-017**: `scripts/merge-quality/` — fixed Fish workflow entrypoints.
- **FILE-018**: `.github/workflows/ci.yml` — binary target invariant and retained feature gates.
- **FILE-019**: `scripts/render_homebrew_formula.sh` and `scripts/test_render_homebrew_formula.py` — explicit product binary installation.
- **FILE-020**: `docs/merge-quality-dataset.md`, `docs/structured-merge-shadow.md`, `docs/common-applicability-probe.md`, `docs/wiki-knowledge-pack.md`, and `docs/project-status.md` — command migration and baseline correction. The former review-pack guide was removed with that workflow.
- **FILE-021**: `.gitignore` — dead binary rules only; preserve dataset/CAS ignores.
- **FILE-022**: `plan/refactor-foch-mq-package-integration-1.md` — executable migration plan and status record.

## 6. Testing

- **TEST-001**: Historical kernel-boundary regression proves scorer `1.0.0`/`1.3.0` uses `legacy_address_patch_reference` while product `foch` attests `semantic_tree` / `full_product_merge`.
- **TEST-002**: Tracked V1 measurement IDs and all file-result foreign keys validate without rewriting bytes.
- **TEST-003**: Mixed V1/V2 JSONL append/read test preserves the original V1 prefix exactly.
- **TEST-004**: Cohort selection reproduces both V1 reports and rejects ambiguous partial selectors.
- **TEST-005**: V2 identity sensitivity matrix covers snapshot, artifact digest, runner protocol, scorer, config, kernel, scope, and excluded operational metadata.
- **TEST-006**: Pure scorer parity test reproduces pinned Legacy file records, verdict counts, resolution labels, and semantic hashes.
- **TEST-007**: Fake-runner lifecycle matrix covers Completed evidence-before-terminal ordering; immediate non-Completed persistence; immutable failed-cache resume accounting; interrupted replay; and corrupt input-CAS preflight before any V2 measurement/file/object evidence persistence with zero runner calls.
- **TEST-008**: Non-ignored real-`foch` seam test validates public CLI output, report parsing, product artifact binding, and scorer integration.
- **TEST-009**: Ignored six-case product fixture acceptance creates and validates a product-specific baseline without changing Legacy fixtures.
- **TEST-010**: Ignored 23-case product acceptance validates terminal completeness and both report cohorts.
- **TEST-011**: Structured rollout, Common-family, full-local symbol, and review-pack acceptance tests preserve separate denominators and mutation boundaries.
- **TEST-012**: Cargo metadata test asserts the only `kind == "bin"` target is `foch-cli/foch`; examples may exist only behind `dev-tools`.
- **TEST-013**: Homebrew renderer test requires `--bin foch` and rejects `--bins`.
- **TEST-014**: Final gates run `cargo fmt --all --check`, strict workspace Clippy, `cargo test --workspace --locked`, retained feature tests, Fish syntax checks, and `git diff --check`.

## 7. Risks & Assumptions

- **RISK-001**: Public full-product merge does more work than the retained-path Legacy evaluator and may make the 23-case run materially slower. Treat this as product performance evidence, not justification to relabel a sliced engine run as product acceptance.
- **RISK-002**: Product `semantic_tree` / `full_product_merge` results will differ sharply from historical Legacy results. A fresh cohort and explicit delta adjudication are mandatory.
- **RISK-003**: Blocked and PartialSuccess CLI exits may be mistaken for crashes if the runner trusts exit status before reading the merge report.
- **RISK-004**: A `foch` binary may be replaced between pre-run hashing and process completion. Rehash the same path after each case and mark mismatches Fatal.
- **RISK-005**: Removing the knowledge and live-shadow command surfaces may expose an undocumented consumer. Require an `rg`/documentation call-site audit before deletion; do not retain dead code solely for hypothetical use.
- **RISK-006**: Moving diagnostic bins to examples can break personal commands. Update verified examples and fail CI if normal Cargo metadata regains another bin target.
- **RISK-007**: The local CAS is private and large. Tests must fail with a concise prerequisite message and must never attempt an automatic full copy or public export.
- **RISK-008**: Mixing the current dirty dataset/security/control-flow work with architectural file moves would make review and rollback unsafe.
- **ASSUMPTION-001**: The 23 immutable snapshot objects and matching EU4 base snapshot remain available locally for the final manual run.
- **ASSUMPTION-002**: The product merge report contains enough conflict/status information for the existing scorer once execution is separated.
- **ASSUMPTION-003**: The current 23-case V1 cohorts are historical evidence only; no external consumer requires `foch-mq` command compatibility.
- **ASSUMPTION-004**: Long-running product acceptance will be launched manually by the user after all automated gates pass.

## 8. Related Specifications / Further Reading

- [`plan/architecture-tree-native-merge-1.md`](./architecture-tree-native-merge-1.md)
- [`docs/merge-quality-dataset.md`](../docs/merge-quality-dataset.md)
- [`docs/structured-merge-shadow.md`](../docs/structured-merge-shadow.md)
- [`docs/adr/0002-unify-tree-merge-kernel.md`](../docs/adr/0002-unify-tree-merge-kernel.md)
