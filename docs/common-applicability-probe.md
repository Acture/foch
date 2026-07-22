# Common applicability probe

## Scope

This checkpoint tests the Directory Module Hypothesis against every `common/**`
unit in the fixed merge-quality corpus and exercises the same definition-module
API used by the production Structured final join. The probe never publishes a
generated mod. Production use remains explicit opt-in; Legacy is still the
default merge kernel.

For each corpus unit, the probe builds four effective module views for its
`common/<folder>` prefix:

- `base`: vanilla
- `left`: vanilla plus the first source mod
- `right`: vanilla plus the second source mod
- `human`: vanilla, both source mods, then the human compatch

Files are resolved by normalized relative path in layer order. A covering
`replace_path` clears earlier files. Visible files are read in lexical path
order. Structured definition modules use the runtime-effective last definition
for duplicate top-level assignment keys; this is deliberately scoped to
Structured so the frozen Legacy baseline is unchanged.

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

Comparison uses the same module normalizer in `common-probe` and
`corpus-shadow`. Definitions identical between candidate and human are reused;
only differing definitions are canonicalized before order-insensitive AST
comparison. This affects scoring only in the Structured arm and cannot relabel
the committed Legacy baseline.

## Production activation

A definition module may reach the production Structured final join only when:

- the caller explicitly selects the Structured merge kernel;
- the `ContentFamily` declares a merge-ready `DefinitionModule` with
  `AssignmentKey` identity;
- the merge plan contains a non-empty vanilla base and at least two distinct
  non-base contributors;
- the patch DAG has exactly two final sinks;
- the structured merge returns no conflict.

Complete Structured module output bypasses Legacy's per-entry vanilla no-op
pruning. Omitting such a definition would expose a source-mod definition, not
the vanilla definition, when the source mods remain loaded. Any conflict blocks
publication; there is no winner-copy fallback.

## Gate

The fixed denominator is the 12 `common/**` units in
`tests/fixtures/legacy-baseline.json`. The report must:

- classify all 12 units;
- preserve every previously accepted unit;
- contain no unsupported-family outcome;
- distinguish accepted AST equivalence, manual resolution, semantic mismatch,
  parse failure, configuration failure, and adapter failure;
- record per-family review status and per-unit structured timing;
- include order-insensitive AST atom deltas against the effective human module.

There is no acceptance-rate threshold. The current measured matrix and
remaining semantic decisions are recorded in
[`research/2026-07-22-common-applicability.md`](./research/2026-07-22-common-applicability.md).
