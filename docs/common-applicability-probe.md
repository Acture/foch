# Common applicability probe

## Scope

This checkpoint tests the Directory Module Hypothesis against every `common/**`
unit in the frozen 12-unit expectation matrix and exercises the same
definition-module API used by the public SemanticTree product merge. The
expectation JSON supplies only paths and historical verdicts. Actual inputs are
resolved from the fixed Workshop case manifest, the paired read-only Steam ACF,
and the installed EU4 root. The probe does not restore or replay a V1 snapshot.
It never publishes a generated mod and does not select or launch a product
executable.

For each corpus unit, the probe builds four effective module views for its
`common/<folder>` prefix:

- `base`: vanilla
- `left`: vanilla plus the first source mod
- `right`: vanilla plus the second source mod
- `human`: vanilla, both source mods, then the human compatch

The source order must match the frozen expectation and the fixed Workshop case
definition exactly. Files are then read directly from the resolved installation
directories and resolved by normalized relative path in layer order. A covering
`replace_path` clears earlier files. Visible files are folded in layer-major
order and lexical path order within each layer, so a later compatch definition
wins over an earlier source definition regardless of their file names.
Structured definition modules use the runtime-effective last definition for
duplicate top-level assignment keys; this is deliberately scoped to Structured
so the frozen Legacy baseline is unchanged.

## Execution

`merge_clausewitz_definition_module` partitions complete base, left, and right
views by top-level key and merges active definitions with the classified
`ContentFamily` policies. Inactive definitions remain in the complete output.
Direct copy-through is policy-aware: identical sides are safe, but a
base-equal side may bypass the kernel only when the family's one-sided-removal
policy is `Remove`. Control-flow definitions and policy-preserved alternatives
always pass through Structured.

Top-level comments are detached before partitioning, merged as trivia, and
reattached deterministically. They do not enter positional content matching.
The output is sorted deterministically after the complete module is rebuilt.

Comparison uses the same module normalizer retained by the frozen Common and
corpus-shadow evidence. Definitions identical between candidate and human are
reused; only differing definitions are canonicalized before order-insensitive
AST comparison. This cannot relabel the committed Legacy baseline.

Each selected logical case is resolved through `ResolvedWorkshopCase`. Its V2
`input_version_id` commits to the game/build identity, ordered Workshop manifest
IDs, and case topology; it does not digest Workshop files. Missing ACF records, unavailable
manifests, or missing item directories become input failures; cases are never
dynamically skipped. After evaluation, the probe reloads the exact same ACF
catalog and compares the relevant install identities. A relevant ACF drift
aborts the run before either report is written; ordinary content changes under
an unchanged ACF identity are outside this version contract.

The schema-4 report records `input_version_id` per resolved unit plus the hashes
of the case manifest, frozen expectation JSON, and evaluator artifact. The probe
has no dataset-root option and never constructs, opens, verifies, or restores an
input-CAS object.

## Run the fixed gate

The supported entrypoint is:

```fish
scripts/merge-quality/common-module.fish
```

It invokes the exact ignored test `common_module_acceptance` in
`crates/foch-merge-quality/tests/maintenance.rs`. The test requires the installed
EU4 version and every Workshop item referenced by the selected fixed cases in
the paired ACF. It asserts the denominator of 12 units and fails unless all 12
are classified with zero failed units.

`run_common_applicability_probe` accepts `case_manifest`, a complete
`Eu4Discovery`, and `evaluator_artifact_blake3` from its caller. The maintenance
test supplies the artifact identity explicitly; the probe library does not call
`current_exe`, discover a runner, or spawn a hidden command. The artifact binds
the resulting evidence to the evaluator without pretending it is a product
measurement.

## Product relationship

A definition module reaches the product SemanticTree final join only when:

- the `ContentFamily` declares a merge-ready `DefinitionModule` with
  `AssignmentKey` identity;
- the merge plan contains a non-empty vanilla base and at least two distinct
  non-base contributors;
- the patch DAG has exactly two final sinks;
- the structured merge returns no conflict.

Complete SemanticTree module output bypasses per-entry vanilla no-op
pruning. Omitting such a definition would expose a source-mod definition, not
the vanilla definition, when the source mods remain loaded. Any conflict blocks
publication; there is no winner-copy fallback.

## Gate

The fixed denominator is the 12 `common/**` units in
`crates/foch-merge-quality/tests/fixtures/legacy-baseline.json`, joined by case
ID to `workshop-product-cases-v2.json`. The report must:

- classify all 12 units;
- record a valid V2 input identity for every resolved unit and no legacy
  `snapshot_id`;
- preserve every previously accepted unit;
- contain no unsupported-family outcome;
- distinguish accepted AST equivalence, manual resolution, semantic mismatch,
  parse failure, configuration failure, and adapter failure;
- record per-family review status and per-unit structured timing;
- include order-insensitive AST atom deltas against the effective human module;
- include independent left, right, human, and candidate deltas against the
  runtime-effective vanilla base so source-backed retention is not mislabeled
  as candidate duplication.

There is no acceptance-rate threshold. The current measured matrix and
remaining semantic decisions are recorded in
[`research/2026-07-22-common-applicability.md`](./research/2026-07-22-common-applicability.md).
