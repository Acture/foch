# Common applicability probe

## Scope

This checkpoint tests the Directory Module Hypothesis against every `common/**`
unit in the fixed merge-quality corpus. It does not change production content
loading, merge planning, output paths, or rollout policy.

For each corpus unit, the probe builds four effective module views for its
`common/<folder>` prefix:

- `base`: vanilla
- `left`: vanilla plus the first source mod
- `right`: vanilla plus the second source mod
- `human`: vanilla, both source mods, then the human compatch

Files are resolved by normalized relative path in layer order. A covering
`replace_path` clears earlier files. Visible files are read in lexical path
order, and duplicate top-level assignment keys use the last visible
definition. This is a provisional model to measure, not a claim about the
game's loader.

## Execution

The shared Clausewitz structured adapter merges `base`, `left`, and `right`
with the classified `ContentFamily` merge policies. Event-only post-processing
is excluded. A structured conflict is a classified Manual Resolution Required
result, not an unsupported-family result.

Before tree matching, the probe removes top-level definitions that are
semantically unchanged in both source views. It merges the remaining active
definitions, projects them back over the complete base module, and compares
that complete candidate with the complete human module. This partitioning is
semantics-preserving for the probe's assignment-key module model and prevents
unrelated vanilla definitions from dominating matcher cost.

The probe never writes a generated mod. It may write candidate previews under
its report directory for inspection; those files are research artifacts and
are not publishable merge output.

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

There is no target acceptance rate in this checkpoint. The measured matrix is
the input to later semantic-policy work.
