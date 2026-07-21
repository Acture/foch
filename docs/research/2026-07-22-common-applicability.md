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
| Accepted order-insensitive AST equivalent | 3 |
| Manual resolution required | 2 |
| Conflict-free semantic mismatch | 7 |
| Probe failure | 0 |

The accepted units are `common/religions`, `common/institutions`, and
`common/scripted_effects`. The result is evidence for the provisional
folder-module boundary, not a production rollout decision.

## Boundary finding

All 12 candidate modules retain every human top-level definition. Eleven have
the exact human top-level key set. `common/rebel_types` has no missing key and
two candidate-only definitions (`ita_monarchy_rebels` and
`ita_republican_rebels`) that the human compatch intentionally omitted.

This removes file-name layout as the explanation for the common-unit failures.
The structured fixes in this checkpoint also change the failure taxonomy:

- `common/institutions` is now exactly equivalent. Residual repeated-key
  siblings no longer fall back to key-only matching, and this family explicitly
  preserves one-sided members of surviving `OR` blocks. That restores the three
  human `trade_goods = cloves` alternatives without broad delete suppression.
- All four `common/scripted_triggers` candidates now flatten and deduplicate the
  repeated `is_expanded_mod_active/has_global_flag` condition. Their remaining
  differences are human-only tag branches plus logically equivalent nested OR,
  singleton OR, and implicit-AND forms; raw atom paths do not prove a merge bug.
- `common/rebel_types` no longer reports `move_move`. The candidate retains two
  complete source definitions (`ita_monarchy_rebels` and
  `ita_republican_rebels`) omitted by human, yielding 89 candidate-only and zero
  human-only atoms. This needs an accepted-better judgment, not deletion logic.
- `common/ages` is a human choice, not a missing Sum reducer: base and Subjects
  use `global_colonial_growth = 50`, Expanded Events changes it to `35`, and
  human rejects that one-sided change. Human also chooses the Subjects control
  flow and omits EE's direct `colony = 5` branch.
- `common/tradegoods` likewise contains manual choices: human invents `0.15`
  between source values `0.2` and `0.1`, selects the base/TGE coal chain over
  EE's one-sided replacement, and restores a modifier removed by TGE.
- `common/governments`' 22 `insert_insert` conflicts are all false semantic
  conflicts between distinct comments (for example, `ME Reforms` versus
  `GE Reforms`). Comments currently enter positional recovery as unanchored
  leaves; they should instead use trivia identity and never require a content
  resolution.
- `common/buildings`' 16 ambiguities are all repeated weighted `modifier`
  records, primarily under `ai_will_do`. The family already declares
  `ListMergePolicy::Replace`, but Structured does not yet dispatch that policy
  over bare lists or weighted-record collections. The next slice should model
  trivia, bare lists, and weighted modifiers as distinct node types, then apply
  their reducers before reporting a manual conflict.

## Runtime

The probe completed in 229,758 ms without invoking a `foch` subprocess or
materializing a merged mod. Unit view construction totals 6,188 ms and
structured merge totals 102,488 ms. `common/scripted_effects` alone takes
77,730 ms in merge and remains matcher-dominated. Roughly 121 seconds are
outside reported unit view/merge time, so snapshot/fixture preparation is also
a first-order bottleneck. The next performance slice should add a comparable
case/family selector and phase timing before changing either cache or matcher
architecture.
