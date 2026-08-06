# Project Status

Last updated: 2026-08-06

## Summary

`foch` is an alpha EU4 analyzer-plus-merge toolkit, and the repository is organized as a workspace monorepo. The near-term public surface is now LSP-first: VS Code/LSP can advance to a `0.1.0` preview on editor usability while the merge engine remains explicitly experimental.

## 2026-08-02 architecture checkpoint

The production structural path now runs both two-revision and multi-revision
joins through the same N-way `TreeMergeKernel`. The separate three-way kernel
and its binary-only policy hooks have been deleted. CWT runtime binding now uses
the compiled, indexed `RuleEngine` across LSP analysis and merge guidance; the
raw graph binder, duplicate validator surface, and unwired syntax-bridge
experiment have been removed.

Cache configuration is now one-root only (`FOCH_CACHE_ROOT`). Every persisted
format uses a SemVer generation parsed by the `semver` crate; obsolete
generations are deleted rather than decoded or migrated. The parser cache uses
SHA-256 content addresses over parser mode and source bytes with bincode
payloads, while mod-snapshot construction bypasses that per-file cache when the
outer content-addressed mod cache is active. Base snapshots accept only the
`1.0.0` `snapshot.bin` wire format. See
[`cache-architecture.md`](./cache-architecture.md).

Focused kernel, CWT, structured-merge, parser-cache, engine-cache, and base
snapshot tests are green at this checkpoint. No full corpus or full workspace
gate has been run, so published merge-quality counts below are unchanged. A
single-file cold/warm mod-snapshot regression now finishes in 6.51 seconds
without touching the user parser cache; most of that remaining cold cost is CWT
runtime initialization and remains a performance target.

## 2026-08-06 merge-quality package integration checkpoint

The standalone merge-quality binary surface has been removed. Normal,
default-feature, and release builds expose only the public `foch` product
binary; merge-quality collection, scoring, reporting, export, probes, and
evidence packaging remain library responsibilities exercised by repository-owned
tests.

Product measurement now crosses an injected `MeasurementRunner` boundary. The
runner launches the exact public `foch merge` artifact, while the library owns
dataset verification, terminal outcome persistence, resume, output archival,
pure scoring of the parsed `MergeReport`, and exact cohort reporting. V2
identity binds the `foch` artifact digest, runner protocol, scorer `2.0.0`,
`semantic_tree`, `full_product_merge`, the installed base snapshot, Steam build,
exact timeout, and a pinned product-base digest. The installed inventory plus
version metadata is copied into a private fixed view; the product and scorer see
only that same view, and its exact file set and bytes are reverified after every
case. The product writes kernel, scope, and base identity into `MergeReport`;
the runner rejects mismatched attestation.
A parseable non-Fatal report remains scoreable even if the process exits
nonzero; crashes, timeouts, explicit execution failures, and Fatal reports
remain distinct terminal records.

Lifecycle validation now recomputes snapshot identities, verifies selected input
CAS objects even on cache hits, binds V2 file-result IDs to their complete
payload, and resumes the file-result-before-terminal crash window only through
deterministic replay. Review packs pin the exact scorer `1.3.0` Legacy cohort,
store only portable Structured attestations, reject conflicts and handler
resolutions fail-closed, and require every Structured output in pack-local CAS.

The two existing 23-case V1 cohorts are unchanged. Their scorer `1.0.0` and
`1.3.0` records both describe the historical
`legacy_address_patch_reference` evaluator and cannot seed a V2 cache hit.

Operator workflows now use the fixed scripts under `scripts/merge-quality/`:
`acceptance.fish`, `fixture-acceptance.fish`, `review-pack.fish`,
`refresh-corpus.fish`, `export.fish`, `refresh-fixtures.fish`,
`symbol-evidence.fish`, `common-module.fish`, `structured-rollout.fish`, and
`acquire.fish`. Each selects one exact ignored test. The old live dual-kernel
shadow workflow is retained only as frozen rollout evidence.

The Steam acquisition gate now carries one corpus-derived plan through download
and evidence generation. Newly needed items require explicit SteamCMD
confirmation; canonical manifest/checksum artifacts bind the discovered corpus
and selected local tree digests, and a second full tree audit closes the
workflow. This is local acquisition integrity, not Steam remote-freshness
attestation and not a product-quality result.

This is an implementation checkpoint, not a corpus acceptance result. Formatting,
locked workspace tests, strict all-target/all-feature Clippy, the hermetic public
CLI-to-scorer seam, default/Steam maintenance compilation, Cargo target
metadata, Homebrew rendering, Fish syntax, Actionlint, Python lint/type checks,
grammar tests, and VS Code smoke all pass. The long-running fixed 23-case V2
product cohort has not completed, so no product quality counts are published
from V2 yet.

## LSP-first 0.1 preview

The VS Code extension and `foch lsp` server are the best candidate for the first user-facing `0.1.0` preview. Their version semantics are separate from merge-engine maturity, but they share the same EU4 semantic analyzer and CWT schema graph. The acceptance scope, CWTools positioning, multi-game path, and proposed agent skill/MCP surfaces are specified in [`lsp-0.1-preview.md`](./lsp-0.1-preview.md).

Current LSP/VS Code preview surface:

- EU4-only language mode and TextMate highlighting for supported Paradox script paths.
- Current-file parse diagnostics, workspace semantic findings, and CWT schema unknown-key/cardinality diagnostics.
- Schema-aware completion and hover for supported EU4 script contexts.
- Goto-definition and find-references for scripted effects/triggers, event ids, flag values, and localisation keys.
- Document symbols and workspace symbol search backed by the semantic index.
- A focused missing-localisation quick fix that creates or appends an English localisation stub in the current mod root.
- Multi-root workspace loading with `foch.toml` manifests, configured game/mod paths, and `descriptor.mod` auto-detection.
- Idle startup in unrelated workspaces with no configured or detected mod roots.
- Reload prompt when `fochLsp.*` settings change so target roots and server launch settings are rebuilt explicitly.

Still out of scope for this 0.1 preview unless implemented separately:

- rename
- broad code actions beyond missing-localisation stubs
- formatter / pretty printer
- semantic tokens
- non-EU4 game profiles
- claiming automatic merge as reliable for arbitrary modlists

## Alpha-1 readiness

The alpha-1 release-prep slice has landed the user-facing deliverables and engine surface needed for first public testers:

- Resolution DSL: `[[resolutions]]` with `match` / `handler` syntax and built-in `last_writer`, `defer`, and `keep_existing` handlers. The reference lives in [`foch-toml-resolutions.md`](./foch-toml-resolutions.md).
- Workspace manifest: `[workspace]`, `[[workspace.imports]]`, and `[[workspace.mods]]` let CLI and LSP share one Cargo-like `foch.toml` source. The reference lives in [`foch-workspace-manifest.md`](./foch-workspace-manifest.md).
- E2E merge fixture harness: 9 tests covering Union, BooleanOr, mixed-kind, recursive, and conflict-resolution scenarios.
- Graph artifacts: `definition-deps` alongside `mod-deps`, with workspace/base/per-mod scopes and `SymbolKind` filtering.
- Opt-in NoOp dedup: cross-file and per-entry NoOp dedup are available per `ContentFamily` after load-semantics verification.
- Cache pipeline: C1/C2/C3/C4 cache layers are inspectable through the `foch cache` CLI.
- DSL cleanup: the old `--fallback` surface and fallback wording are gone; `MergeReport` is slimmer and now records V1/V2 cross-tag information.
- Drift analysis: v1a vanilla symbol indexing and S002B stale-fallback distinction are in place.
- Audit/status cleanup: the p5 compensation-hack audit, p6 decoupled `merge_status` / `analysis_status` views, and CWT mapping design doc are recorded.

Current EU4 active-playset merge baseline:

- N=37 probe after the leaf-conflict fix: `manual_conflict_count = 9`.
- [`examples/eu4-default-foch.toml`](../examples/eu4-default-foch.toml) ships narrow per-path defaults that clear all 9 manual conflicts without enabling global last-writer behavior.
- Warm cache-backed iterations are seconds; cold debug runs remain around 25-30 minutes, while release+cache has been observed around 40 seconds for this baseline.

The merge-quality dataset has a separate immutable baseline lifecycle. The
standalone runner is retired; package-owned tests now inject the public product
runner while the library verifies the APFS copy-on-write object store, records
terminal outcomes, resumes by stable identity, scores existing outputs, and
produces deterministic exports. Broad Workshop candidates remain in the full
collection, while a separately versioned oracle policy selects the provisional
scoring cohort.

Important identity correction: both existing 23-case measurement cohorts use
the evaluation-only `AddressPatchReference` kernel selected by the historical
scorer. They are Legacy evidence, not measurements of the `semantic_tree`
kernel used by the public `foch merge` path. Their JSONL records,
file-result foreign keys, and CAS objects remain immutable; the first product
baseline must be a fresh measurement cohort bound to the actual `foch` binary.

The first canonical full-local baseline completed on 2026-07-13 with scorer
`1.0.0` against source
commit `4c98c9e`, EU4 `v1.37.5.0` / Steam build `15918133`, executable hash
`16fcde0535ad3c759492f1aa76ad6164d466cb6fea8125a65f36c3bebb06ea91`, and
scorer configuration hash
`e2580bc8c745bf7aca520ce909f093028455a9745d5fae6f92b94424d2986393`.
All 23 broad candidates reached `completed`; referential integrity holds across 23 unique
snapshots, 23 measurements, 30,739 file results, and 91 object records. The
historical all-candidate totals are 43/30,739 over all reference-output files
and 28/269 over multi-source files. That view is dominated by 30,293
`not_emitted` files and includes broad-search false positives, so it is retained
for audit rather than used as the current merge-success denominator.

The historical scorer `1.3.0` Legacy cohort completed on 2026-08-06. It is
pinned to executable hash
`0507a19de246a59bd2f718ad2941fd4d0c9ec07d469ab911a1e6b04bb11ba519`
and configuration hash
`8beffefe06b044798b769b805fb556dd93769ebdbf367df3d6468ef6834d5665`.
All 23 broad candidates reached `completed`. The all-candidate view accepted
83/29,481 reference-output files and 25/217 multi-source files; the six-case
scorable view accepted 11/39 reference-output files and 11/36 multi-source
files. These figures describe `legacy_address_patch_reference` only. They must
not be quoted as TreeMerge or product quality. See
[`merge-quality-dataset.md`](./merge-quality-dataset.md).

All 15 `.gui` units still diverge under the order-sensitive GUI policy.
Removing them leaves 10/21 accepted non-GUI units (47.6%), with 11
`diverges_ast`. Directory-scoped definition
modules now keep cross-filename `common/*` families in the denominator:
all four `common/scripted_triggers` units, `common/religions`, and
`common/institutions` are module-AST equivalent in the fixed 12-unit Structured
common probe. The last full schema-`2.0.0` matrix reported 6 equivalent, 2
manual-resolution, 4 semantic-mismatch, and 0 failed, but that aggregate is now
provisional. The `common/rebel_types` candidate-superset result still needs an
accepted-better judgment. The previous `common/governments`
candidate-superset label is withdrawn: the evaluator incorrectly let an
earlier `zzz_*` source file override a later `00_*` compatch file. Layer-major
precedence is covered in both probe and scorer tests. A focused schema-`2.1.0`
real-corpus rerun now accepts governments as exactly equivalent: candidate and
human each contain 2,046 semantic atoms, all 2,046 are shared, and neither side
has a one-sided atom. That focused output was not archived; the linked
machine-readable evidence remains the schema-`2.0.0` full run. The implied
matrix is 7 equivalent, 1 manual-resolution, 4 semantic-mismatch, and 0 failed,
but it remains a projection until review-pack regeneration binds the result to
its inputs.

The review-pack infrastructure fixes the six snapshots, all 36 Legacy units,
the 13 historical Structured rollout units, and each archived Legacy
measurement/output CAS. Its library builder now requires an injected runner and
has no default child process. The repository-owned ignored acceptance test and
Fish wrapper now own build plus verification, but the real review pack has not
been generated; this checkpoint therefore changes no published quality counts. See
[`merge-quality-review-pack.md`](./merge-quality-review-pack.md).

The unused advisory Wiki acquisition/search subsystem has been removed from
foch. It had no fetched canonical pack and no product, scorer, or review
consumer; any future Wiki research requires a separately owned package rather
than another merge-quality command surface.

The original focused governments process took 258,342 ms end to end, although
the unit analysis itself took only 177 ms. A live stack sample found the
remaining time in `load_snapshot -> verify_object -> digest_tree`, reopening
and hashing 5,935 files across three content-addressed objects. Probe and shadow
restoration now persist a versioned metadata stamp after a full payload audit,
reuse it only while the complete tree metadata fingerprint is unchanged, and
verify each object at most once per command. Any detected metadata change
forces a full rehash. The earlier marker-only focused rerun took 339 ms at
report level, about 762x faster, but that timing is not a benchmark for the
guarded cache.

Legacy event merging now keys repeated `option` blocks by `option.name`, and
per-path event/decision output retains vanilla-equivalent entries because the
generated file shadows the lower-layer file. In the GE-EE `Elections.txt` case this
reduced the semantic difference to 7 human-only and 2 foch-only atoms with
1,210 shared atoms. The remaining misses are repeated `after`/`if` control-flow
branches inside matched events/options, so event control-flow identity is the
next event-specific merge-quality target.

The former structured-merge shadow slice is now frozen rollout history. Its
parser-independent kernel established verified matching, base-composed
correspondence, PCS ordering, provenance, and conflict-visible
delete/move/reparent/reorder behavior before the public product runner adopted
`semantic_tree`. The historical 13-candidate projection moved its Legacy view
from 7/36 to 12/36 and from 7/21 to 12/21 outside `.gui`, but those counts cover
only a selected rollout against a Legacy address-patch denominator. They are
not current product quality. See
[`structured-merge-shadow.md`](./structured-merge-shadow.md).

The retired corpus-shadow harness restored immutable snapshot units, derived a
fixed multi-source denominator, and compared selected candidates while retaining
Legacy evidence for projection. Its report schema `2.0.0` kept execution,
retained rows, safety, and timings separately auditable. That machinery is no
longer an operator workflow; tracked records are frozen historical evidence.

The 2026-07-21 complete 36-unit projection evaluated only GE-EE
`events/Elections.txt` and retained Legacy for the other 35 units. Elections is
a strict improvement: Structured matches all 1,217 human atoms with zero
one-sided atoms, reports no diagnostics, duplicate event/option IDs, or orphan
control-flow paths, and matches the human control-flow multiset. Structured
took 54,270 ms versus Legacy's 51,229 ms (1.059x). The projection is 7 -> 8
accepted overall and 7 -> 8 across the 21 non-GUI units, with no Legacy accepted
unit lost and no terminal candidate failure. This validates Elections only;
at that checkpoint, Structured remained disabled for the broader event family.

That result is historical rather than a current release claim. In the
2026-07-22 generalized 13-candidate run, Elections failed the control-flow shape
safety check with 74 candidate-only and 31 human-only atoms. It is excluded from
that projection until the generalized control-flow path restores the
earlier event result.

A focused 2026-07-23 Elections rerun restored the generalized
control-flow safety gate without restoring exact AST equality. Structured is
parse-valid and conflict-free, has no diagnostics, duplicate event/option IDs,
or orphan control flow, and its canonical control-flow shape matches the human
compatch. The candidate shares 1,186 of 1,217 human semantic atoms, with 21
candidate-only and 31 human-only atoms, so the focused outcome is
`needs_review`, not accepted. Structured took 8,145 ms versus Legacy's
10,082 ms. This focused result does not change the 36-unit projection.

The historical 2026-07-22 Common applicability run completed the first gate
over all 12 fixed `common/**` corpus units. The probe uses
the provisional `common/<folder>` module boundary, applies each classified
`ContentFamily` policy to the same definition-module API later used by the
public `semantic_tree` join, and never publishes a generated mod. All 12 units
reached a classified result with no unsupported, parse, configuration, or
adapter failure; the previously accepted religions unit remained accepted.
Six units are order-insensitive AST equivalent, two require manual resolution,
and four are conflict-free semantic mismatches. The release-mode run took
37,398 ms; `common/scripted_effects` accounted for 29,176 ms and remains the
matcher-dominated hotspot. See
[`research/2026-07-22-common-applicability.md`](./research/2026-07-22-common-applicability.md).

A focused 2026-07-23 rerun of case `3635635014` now classifies
`common/scripted_effects` as `accepted_equivalent`. Block-valued sibling
`if`/`else` branches are paired with a LIFO stack, while a branch without a
`limit` is modeled as an unconditional fallback instead of a structural error.
The candidate and human module share all 28,066 semantic atoms with zero
one-sided atoms and no conflicts. This establishes the focused 6/12 -> 7/12
Common delta only; the 12-unit Common gate and 36-unit shadow projection have
not been rerun. The 12/36 and 12/21 figures below therefore remained the last
historical shadow projection at that checkpoint; they are not a current product
baseline.

The corresponding historical shadow projection evaluated those 12 common
units plus Elections while retaining Legacy for the other 23 scorer units.
Outcomes were 5 improved, 0 regressed, 1 unchanged accepted, 4 review, 2
structured conflict, and 1 safety failure. Strict and adjudicated acceptance
projected from 7/36 to 12/36, and non-GUI acceptance from 7/21 to 12/21, with
zero Legacy-accepted units lost. Aggregate candidate runtime was 0.960x
Legacy. This is frozen rollout evidence, not a live dual-kernel promotion gate.

Current Common-module reruns use
`scripts/merge-quality/common-module.fish`, which selects the exact ignored
`common_module_acceptance` test and asserts the fixed 12-unit cohort. The probe
accepts `evaluator_artifact_blake3` from its caller; the merge-quality library
does not select or launch a hidden evaluator executable.

The shipped product surface includes:

- `foch check`
- `foch merge-plan`
- `foch merge`
- `foch graph`
- `foch simplify`
- `foch data`
- `foch cache`
- `foch config`
- `foch workspace`
- `foch lsp`

## Current Repository Shape

The repository now has these first-class packages:

- `crates/foch-cli`
- `crates/foch-core`
- `crates/foch-language`
- `crates/foch-engine`
- `crates/foch-merge-kernel`
- `crates/foch-merge-quality`
- `packages/tree-sitter-paradox`
- `packages/vscode-foch`

The repository root is coordination-only:

- Cargo workspace manifest
- Bun workspace manifest
- shared CI
- docs and scripts

The old `src/check/` compatibility shell is gone. Internal code now imports directly from `foch_core`, `foch_language`, and `foch_engine`.

## What Exists Today

### Analyzer

The analyzer pipeline can:

- resolve a playset into an effective workspace
- load optional installed base-game snapshots
- parse Clausewitz, localisation, CSV, and JSON families
- reuse a per-mod content-addressed parse cache for unchanged mod AST and semantic-index artifacts
- build semantic indexes across base game and enabled mods
- emit strict and advisory findings
- surface overlap diagnostics through the shared runtime overlap classifier

### Merge

The merge pipeline can:

- produce deterministic `merge-plan` artifacts
- build merge IR for supported structural roots
- emit normalized Clausewitz output
- materialize a merged output tree with `descriptor.mod` and `.foch/*` sidecars
- revalidate generated output and backfill final validation buckets

### Graph

The graph pipeline can:

- export runtime `calls` graphs
- export descriptor-level `mod-deps` graphs
- export family-first semantic graphs with `--mode semantic --family <content-family-id>`
- annotate cross-mod edges with declared-dependency hints
- write deterministic `json` and `dot` artifacts for workspace/base/per-mod views and optional symbol trees
- write deterministic `semantic-graph.json` plus a static `index.html` viewer for a selected family

### Simplify

The simplify pipeline can:

- remove definitions in a target mod that are structurally equivalent to the effective base-game definition
- work either in-place or into an output copy
- write a machine-readable `simplify-report.json`

## Current Coverage Reset Loop

The current semantic-complete gameplay roots in the last verified real probe include:

- `common/country_tags`
- `common/countries`
- `common/cultures`
- `common/bookmarks`
- `common/achievements`
- `common/ages`
- `common/buildings`
- `common/cb_types`
- `common/diplomatic_actions`
- `common/event_modifiers`
- `common/new_diplomatic_actions`
- `common/church_aspects`
- `common/decrees`
- `common/defender_of_faith`
- `common/disasters`
- `common/factions`
- `common/federation_advancements`
- `common/fetishist_cults`
- `common/fervor`
- `common/flagship_modifications`
- `common/golden_bulls`
- `common/great_projects`
- `common/government_mechanics`
- `common/government_reforms`
- `common/hegemons`
- `common/holy_orders`
- `common/ideas`
- `common/institutions`
- `common/isolationism`
- `common/estate_agendas`
- `common/estate_privileges`
- `common/estates`
- `common/naval_doctrines`
- `common/parliament_bribes`
- `common/parliament_issues`
- `common/peace_treaties`
- `common/powerprojection`
- `common/personal_deities`
- `common/policies`
- `common/professionalism`
- `common/province_names`
- `common/province_triggered_modifiers`
- `common/rebel_types`
- `common/religions`
- `common/state_edicts`
- `common/subject_types`
- `common/subject_type_upgrades`
- `common/technologies`
- `common/technology`
- `common/units`
- `common/mercenary_companies`
- `map/random/scenarios`
- `map/random/tiles`
- `map/random_names`
- `common/scripted_triggers`
- `history/advisors`
- `history/countries`
- `history/diplomacy`
- `history/provinces`
- `history/wars`

The latest verified real probe is:

- `parse_only = 60`
- `semantic_complete = 69`

`map/random` is now split honestly instead of being treated as one mixed root:

- `map/random/scenarios = semantic_complete`
- `map/random/tiles = semantic_complete`
- `map/random_names = semantic_complete`

The recent low-risk common-root coverage slices now include `common/government_ranks`, `common/buildings`, `common/cb_types`, `common/diplomatic_actions`, `common/event_modifiers`, `common/new_diplomatic_actions`, `common/ages`, `common/institutions`, `common/scripted_triggers`, `common/government_reforms`, `common/ideas`, `common/province_triggered_modifiers`, `common/advisortypes`, `common/government_names`, `common/custom_gui`, `common/cultures`, `common/great_projects`, and `common/achievements`, and the latest verified real-probe baseline is `parse_only = 60` / `semantic_complete = 69`.

The static semantic viewer had one critical renderer regression immediately after ACT-157 landed: the generated `index.html` escaped CSS and JS braces incorrectly, which left the page shell visible but the graph tree blank. That regression is now fixed and covered by a renderer-level test in `foch-engine`.

Validation now splits into two tracks:

- representative family output is readable again in the static viewer
- real semantic-graph runs are now observable enough to use as a validation loop without falling back to ad hoc `/tmp` slices

A repo-backed bounded validation path now exists under `tests/corpus/eu4_real_minimized/playlist.json`. Semantic graph CLI integration coverage uses that playset to export `common/scripted_effects`, assert default-visible progress output from the `tracing` pipeline, and confirm that the generated graph contains real scripted-effect keys such as `eu4::scripted_effects::se_md_add_or_upgrade_bonus` and `eu4::scripted_effects::complex_dynamic_effect_without_alternative`.

ACT-165 has now completed that validation loop. The bounded real-data playset was exercised against `common/scripted_effects`, `common/new_diplomatic_actions`, `missions`, and `common/triggered_modifiers`, with one external sanity pass on a real workshop `common/holy_orders` graph. Across that sample set, the validation did not uncover a repeated semantic-viewer blocker: default visibility, `Show contains`, details-panel inspection, and large-family readability all held up well enough to avoid forcing an immediate viewer-refinement follow-up.

The current recommendation is therefore to return the mainline to semantic coverage promotion rather than opening an ACT-158-style viewer refinement wave. Semantic-graph work can stay on the bugfix path unless later real-family validation turns up a repeated viewer/product failure.

ACT-166 resumed that coverage line by promoting `common/buildings` from `graph_ready` to `semantic_complete`. The implementation stays intentionally narrow: it records stable top-level `building_definition` entries, preserves the existing `ScriptFileKind::Buildings` effect/trigger semantics, updates graph family classification so building definitions no longer collapse into `unknown`, and extends base-data coverage assertions accordingly. A fresh full-EU4 probe has now confirmed the updated baseline without moving `parse_only`, which means this slice cleanly converted one `graph_ready` root into a verified additional `semantic_complete` root.

ACT-167 completed the next coverage slice by promoting `common/diplomatic_actions` from `merge_ready` to `semantic_complete` without regressing its existing merge support. The implementation kept the same narrow promotion pattern as `common/buildings`: it records stable top-level `diplomatic_action_definition` entries, preserves the existing typed trigger/effect semantics already attached to `ScriptFileKind::DiplomaticActions`, maps those definitions back to `common/diplomatic_actions` in semantic graph classification, and fixes coverage-class precedence so a root that is both semantic-complete and merge-ready reports as `semantic_complete`. A fresh full-EU4 probe confirmed the new baseline without moving `parse_only`, so this slice converted one additional gameplay root into a verified `semantic_complete` root.

ACT-168 has now completed its full-probe acceptance gate. This slice promotes `common/new_diplomatic_actions` from `graph_ready` to `semantic_complete` with a deliberately narrow extractor: top-level action definitions emit `new_diplomatic_action_definition`, the `static_actions` container itself is explicitly excluded from definition resources, the existing typed trigger/effect container semantics remain unchanged, and semantic graph classification maps the new definition key back to `common/new_diplomatic_actions`. A fresh full-EU4 probe moved the verified baseline to `parse_only = 60` / `semantic_complete = 55` without regressing `parse_only`.

ACT-169 has now completed its full-probe acceptance gate. This slice promotes `common/ages` from `graph_ready` to `semantic_complete` with the same narrow promotion pattern as the prior common-root waves: top-level age entries emit `age_definition`, nested objective/ability structures remain context, and the existing typed trigger/effect handling attached to `ScriptFileKind::Ages` stays intact. A fresh full-EU4 probe moved the verified baseline to `parse_only = 60` / `semantic_complete = 56` without regressing `parse_only`.

ACT-170 has now completed its full-probe acceptance gate. This slice promotes `common/institutions` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level institution entries emit `institution_definition`, nested trigger/effect and modifier-style structures remain context, and the existing typed handling attached to `ScriptFileKind::Institutions` stays intact. A fresh full-EU4 probe moved the verified baseline to `parse_only = 60` / `semantic_complete = 57` without regressing `parse_only`.

ACT-171 has now completed its full-probe acceptance gate. This slice promotes `common/scripted_triggers` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level scripted trigger entries emit `scripted_trigger_definition`, nested `limit` and wrapper-style trigger containers remain context, and the existing typed handling attached to `ScriptFileKind::ScriptedTriggers` stays intact. A fresh full-EU4 probe moved the verified baseline to `parse_only = 60` / `semantic_complete = 58` without regressing `parse_only`.

ACT-174 has now completed its full-probe acceptance gate. This slice promotes `common/government_reforms` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level reform entries emit `government_reform_definition`, nested `ai_will_do`, modifier-style structures, and other wrapper blocks remain context, and the existing typed handling attached to `ScriptFileKind::GovernmentReforms` stays intact. A fresh full-EU4 probe confirmed `common/government_reforms = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 59` without regressing the verified baseline.

ACT-175 has now completed its full-probe acceptance gate. This slice promotes `common/ideas` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level idea groups emit `idea_group_definition`, nested `start`, `bonus`, and individual idea-entry blocks remain context, and the existing typed handling attached to `ScriptFileKind::Ideas` stays intact. A fresh full-EU4 probe confirmed `common/ideas = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 60` without regressing the verified baseline.

ACT-180 has now completed its full-probe acceptance gate. This slice promotes `common/province_triggered_modifiers` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level modifier entries emit `province_triggered_modifier_definition`, nested `potential`, `trigger`, `on_activation`, and `on_deactivation` wrapper blocks remain context, and the existing typed handling attached to `ScriptFileKind::ProvinceTriggeredModifiers` stays intact. A fresh full-EU4 probe confirmed `common/province_triggered_modifiers = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 61` without regressing the verified baseline.

ACT-181 has now completed its full-probe acceptance gate. This slice promotes `common/cb_types` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level CB entries emit `cb_type_definition`, nested `can_use` and `can_take_province` wrapper blocks remain context, and the existing typed handling attached to `ScriptFileKind::CbTypes` stays intact. A fresh full-EU4 probe confirmed `common/cb_types = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 62` without regressing the verified baseline.

ACT-182 has now completed its full-probe acceptance gate. This slice promotes `common/event_modifiers` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level event-modifier entries emit `event_modifier_definition`, nested `trigger` wrapper blocks remain context, and the existing typed handling attached to `ScriptFileKind::EventModifiers` stays intact. A fresh full-EU4 probe confirmed `common/event_modifiers = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 63` without regressing the verified baseline.

ACT-183 has now completed its full-probe acceptance gate. This slice promotes `common/advisortypes` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level adviser-type entries emit `advisor_type_definition`, nested `trigger` wrapper blocks remain context, and semantic graph classification maps the new definition key back to `common/advisortypes` without colliding with the existing `history/advisors` resource family. A fresh full-EU4 probe confirmed `common/advisortypes = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 64` without regressing the verified baseline.

ACT-184 has now completed its full-probe acceptance gate. This slice promotes `common/government_names` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level government-name entries emit `government_name_definition`, nested `trigger` wrapper blocks remain context, and semantic graph classification maps the new definition key back to `common/government_names` instead of leaving those resources uncategorized. A fresh full-EU4 probe confirmed `common/government_names = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 65` without regressing the verified baseline.

ACT-185 has now completed its full-probe acceptance gate. This slice promotes `common/custom_gui` from `graph_ready` to `semantic_complete`, but it required one mid-slice correction after the first acceptance probe exposed a bad local assumption about the real file shape. The shipped game data does not use a top-level `guiTypes` container here; instead it defines repeated top-level `custom_*` wrapper blocks whose semantic identity comes from the inner `name = ...` field. The extractor and coverage fixture were corrected to match that real layout: top-level `custom_*` blocks emit `custom_gui_definition` from their `name`, while the wrapper key itself and nested blocks such as `potential`, `trigger`, and `frame` remain context only. A fresh full-EU4 probe then confirmed `common/custom_gui = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 66` without regressing the verified baseline.

ACT-186 has now completed its full-probe acceptance gate. This slice promotes `common/cultures` from `graph_ready` to `semantic_complete` with a deliberately narrow extractor for the real EU4 file shape: top-level culture-group wrappers remain context, but named culture blocks nested one level under those groups emit `culture_definition`. Nested payload blocks such as `primary`, `male_names`, `female_names`, and similar data containers remain context only, and semantic graph classification now maps `culture_definition` back to `common/cultures` instead of leaving those resources uncategorized. A fresh full-EU4 probe confirmed `common/cultures = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 67` without regressing the verified baseline.

ACT-187 has now completed its full-probe acceptance gate. This slice promotes `common/great_projects` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level project entries emit `great_project_definition`, nested `build_trigger` and `on_built` wrapper blocks remain context, and semantic graph classification maps `great_project_definition` back to `common/great_projects` instead of leaving those resources uncategorized. A fresh full-EU4 probe confirmed `common/great_projects = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 68` without regressing the verified baseline.

ACT-188 has now completed its full-probe acceptance gate. This slice promotes `common/achievements` from `graph_ready` to `semantic_complete` with the same narrow coverage pattern as the recent common-root waves: top-level achievement entries emit `achievement_definition`, nested wrappers such as `possible`, `visible`, `happened`, and `provinces_to_highlight` remain context, and semantic graph classification maps `achievement_definition` back to `common/achievements` instead of leaving those resources uncategorized. A fresh full-EU4 probe confirmed `common/achievements = semantic_complete`, kept `parse_only = 60`, and moved `semantic_complete = 69` without regressing the verified baseline.

Finding-bucket tracks such as `ACT-32`, `ACT-31`, and `ACT-28` are now secondary observability loops. They remain useful for regression signals, but they no longer define the main plan.

## Verification

Verified locally during the completed coverage waves:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- real `foch data build eu4 ...` probes confirmed:
  - `parse_only: 85 -> 80`
  - `semantic_complete: 24 -> 29`
  - `parse_only: 80 -> 78`
  - `semantic_complete: 29 -> 31`
  - `parse_only: 78 -> 76`
  - `semantic_complete: 31 -> 33`
  - `parse_only: 76 -> 74`
  - `semantic_complete: 33 -> 35`
  - `parse_only: 74 -> 73`
  - `semantic_complete: 35 -> 36`
  - `parse_only: 73 -> 73`
  - `semantic_complete: 36 -> 39`
  - `parse_only: 73 -> 68`
  - `semantic_complete: 39 -> 44`
  - `parse_only: 68 -> 63`
  - `semantic_complete: 44 -> 49`
  - `parse_only: 63 -> 62`
  - `semantic_complete: 49 -> 50`
  - `parse_only: 62 -> 61`
  - `semantic_complete: 50 -> 51`
  - `parse_only: 61 -> 60`
  - `semantic_complete: 51 -> 52`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 52 -> 53`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 53 -> 54`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 54 -> 55`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 55 -> 56`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 56 -> 57`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 57 -> 58`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 58 -> 59`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 59 -> 60`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 60 -> 61`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 61 -> 62`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 62 -> 63`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 63 -> 64`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 64 -> 65`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 65 -> 66`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 66 -> 67`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 67 -> 68`
  - `parse_only: 60 -> 60`
  - `semantic_complete: 68 -> 69`

Verified locally during the workspace reorganization:

- `cargo check -p foch-language`
- `cargo check -p foch-engine`
- `cargo check -p foch-cli`
- `cargo check --workspace`

Verified locally during the semantic graph mode implementation:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`

Verified locally during the semantic viewer repair:

- `cargo fmt --all`
- `cargo test -p foch-engine graph::semantic -- --nocapture`
- browser validation against the regenerated `common/holy_orders` semantic viewer confirmed tree rendering and details-panel interaction

Verified locally during semantic graph observability hardening:

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test -p foch-cli semantic_graph -- --nocapture`
- `cargo test --workspace`
- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act164-probe --mode semantic --family common/scripted_effects --no-game-base`

Verified locally during ACT-165 representative-family validation:

- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act165-validation --mode semantic --family common/scripted_effects --no-game-base`
- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act165-validation --mode semantic --family common/new_diplomatic_actions --no-game-base`
- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act165-validation --mode semantic --family missions --no-game-base`
- `target/debug/foch graph tests/corpus/eu4_real_minimized/playlist.json --out /tmp/foch-act165-validation --mode semantic --family common/triggered_modifiers --no-game-base`
- `target/debug/foch graph /tmp/foch-act165-holy-orders-playlist.json --out /tmp/foch-act165-holy-orders --mode semantic --family common/holy_orders --no-game-base`

## Elections ordered-correspondence checkpoint (2026-07-23)

- Ordered matching now treats every already-matched sibling as a barrier, so an open chain cannot be paired across an interleaved closed chain. Unique exact signatures may still follow genuine moves; PCS remains responsible for reporting incompatible reorderings.
- One-sided removal preservation now requires a unique same-kind insertion in the deleted node's ordered base gap. Unrelated surviving `if` siblings no longer resurrect vanilla branches deleted by one mod.
- Complete constructor chains with an empty `define_ruler` fallback use the full effect set as their correspondence identity. Distinct candidate constructors remain independent chains instead of being rewritten into one `if` / `else_if` chain.
- Event safety compares raw structure first, then canonicalizes only the smallest control-flow owners whose shapes differ. This accepts equivalent guard normalization without paying to normalize the whole event file.
- Focused validation passed: all 59 `foch-merge-kernel` tests, all 60 Structured-module tests, three event-safety regressions, and formatting.
- The Elections-only real probe is parse-valid, conflict-free, and `control_flow_matches_human = true`; it moves from `safety_failed` to `needs_review`. It shares 1,186/1,217 human atoms, with 21 candidate-only and 31 human-only atoms. Structured took 8,145 ms versus Legacy's 10,082 ms.
- Historical evidence:
  `/private/tmp/foch-elections-selective-safety/shadow-case.json`. Neither Common
  nor the full corpus was rerun at that checkpoint, so the frozen projection
  counts were unchanged.
- Next slice: localize the remaining 21/31 atom delta by event and option, separating canonical-equivalent guard rewrites from genuine content choices before adding another merge rule.

## Practical Reading Order

1. [architecture.md](./architecture.md)
2. [merge-design.md](./merge-design.md)
3. [auto-merge-roadmap.md](./auto-merge-roadmap.md)
