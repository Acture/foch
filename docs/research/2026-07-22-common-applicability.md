# Common applicability probe results

The 2026-07-22 fixed 12-unit `common/**` corpus probe passed the Common
Applicability Gate. Structured classified every unit, produced no unsupported,
parse, configuration, or adapter result, and preserved the previously accepted
religions unit.

Machine-readable evidence:
[`evidence/2026-07-22-common-applicability.json`](evidence/2026-07-22-common-applicability.json)

## Matrix

| Outcome | Units |
| --- | ---: |
| Accepted order-insensitive AST equivalent | 2 |
| Manual resolution required | 2 |
| Conflict-free semantic mismatch | 8 |
| Probe failure | 0 |

The accepted units are `common/religions` and `common/scripted_effects`.
The result is evidence for the provisional folder-module boundary, not a
production rollout decision.

## Boundary finding

All 12 candidate modules retain every human top-level definition. Eleven have
the exact human top-level key set. `common/rebel_types` has no missing key and
two candidate-only definitions (`ita_monarchy_rebels` and
`ita_republican_rebels`) that the human compatch intentionally omitted.

This removes file-name layout as the explanation for the previous common-unit
failures. The remaining differences are inside definitions:

- All four `common/scripted_triggers` units contain one duplicate
  `is_expanded_mod_active/has_global_flag` atom. BooleanOr needs flattening and
  deduplication in the structured policy.
- `common/rebel_types` reports six `move_move` conflicts around repeated/list
  content; its tentative candidate also retains the two human-omitted rebels.
- `common/governments` reports 22 `insert_insert` and nine `move_move`
  conflicts. Repeated list items need the declared OrderedUnion semantics
  instead of generic ordered-tree identity.
- `common/institutions` loses three repeated `trade_goods = cloves` atoms under
  nested modifier/OR containers. Repeated sibling identity/cardinality remains
  under-modeled.
- `common/ages` differs on three atoms, including numeric `35` versus human
  `50`. The generic kernel receives the family Sum policy but does not yet
  synthesize numeric reducer values.
- `common/buildings` and `common/tradegoods` differ inside repeated modifiers,
  scalar choices, and control-flow branches. They require family reducers and
  control-flow semantics, not another file-layout rule.

## Runtime

The probe completed in 123,260 ms without invoking a `foch` subprocess or
materializing a mod. It removes definitions that are unchanged in both source
views before tree matching, then projects the active merge over the complete
base module and compares the complete result with the complete human module.

`common/scripted_effects` still takes 80,263 ms for 848 active definitions and
is matcher-dominated (77.3 seconds). The next performance work should partition
active top-level definitions into independent kernel calls or add top-level
anchor partitioning inside the kernel; harness I/O is not the dominant cost.
