# Common applicability probe results

The 2026-07-22 fixed 12-unit `common/**` corpus probe passed the Common
Applicability Gate. Structured classified every unit, produced no unsupported,
parse, configuration, or adapter result, and preserved the previously accepted
religions unit.

Machine-readable evidence:
[`evidence/2026-07-22-common-applicability.json`](evidence/2026-07-22-common-applicability.json)
is the schema-`2.0.0` full 12-unit run. The later schema-`2.1.0` governments
probe was focused and was not archived, so its numbers remain provisional
until the review pack regenerates bound evidence.

## Matrix

| Outcome | Units |
| --- | ---: |
| Accepted order-insensitive AST equivalent | 6 |
| Manual resolution required | 2 |
| Conflict-free semantic mismatch | 4 |
| Probe failure | 0 |

The accepted units are all four `common/scripted_triggers` cases,
`common/religions`, and `common/institutions`. The accepted set is not only a
probe result: the same definition-module merge API now runs in the production
Structured final join. Structured remains explicit opt-in and does not change
the Legacy default.

## Production shadow

A 13-candidate shadow run exercised the 12 common units plus GE-EE
`events/Elections.txt` against the fixed 36-unit scorer denominator:

- 5 improved, 0 regressed, and 1 remained accepted;
- 4 need review, 2 withheld output on structured conflicts, and 1 failed the
  event safety gate;
- projected strict and adjudicated acceptance both rose from 7/36 to 12/36;
- projected non-GUI acceptance rose from 7/21 to 12/21;
- no Legacy-accepted unit was lost;
- aggregate candidate runtime was 0.960x Legacy for these 13 units.

The five improvements are the four scripted-trigger units and institutions.
Religions remains accepted. Buildings and scripted effects correctly withhold
publication; rebel types, trade goods, ages, and governments remain explicit
semantic-review cases. Elections currently fails control-flow shape safety and
does not contribute to the projection.

## Boundary finding

The provisional `common/<folder>` boundary is sufficient for this first
production slice. The merge builds complete runtime-effective module views,
partitions them by top-level definition, and retains inactive definitions in
the generated module. Output therefore no longer depends on source file names.

The implementation also closes three false-difference classes:

- top-level comments are detached as trivia, merged independently, and attached
  deterministically instead of participating in positional tree matching;
- Boolean `OR` nodes flatten and deduplicate equivalent descendants while
  policy-preserved one-sided alternatives still pass through the kernel;
- scalar reducers can synthesize a deterministic numeric result. In trade
  goods, the configured average reducer produces the human `0.15` value from
  source values `0.2` and `0.1`.

The six rejected units now have narrow explanations:

- `common/rebel_types` is a strict candidate superset: 95 candidate-only atoms
  from two complete source definitions and zero human-only atoms. This needs an
  accepted-better judgment or an explicit exclusion, not deletion logic.
- `common/buildings` has 16 ambiguous matches among repeated weighted
  `modifier` records under `courthouse`; publication is withheld.
- `common/tradegoods` has eight candidate-only atoms after the numeric reducer
  succeeds. They are the retained cloves exclusion, coal control-flow branch,
  and dyes modifier that the human compatch chose to omit.
- `common/ages` keeps Expanded Events' one-sided value and colony branch while
  the human compatch selects the other source's value and control flow.
- The original `common/governments` result reported 152 candidate-only atoms.
  That attribution is invalid: the evaluator folded all visible files in
  global lexical-path order, allowing an earlier source `zzz_*` file to
  override a later compatch `00_*` file. The 2026-07-24 layer-major fix covers
  both the applicability probe and the production scorer. The focused
  schema-`2.1.0` focused run reported exact equivalence: 2,046 candidate atoms,
  2,046 human atoms, all shared, with zero one-sided atoms. That result is not
  part of the linked schema-`2.0.0` artifact and must be regenerated before it
  is accepted as evidence. The original 152-atom claim remains an evaluator
  artifact.
- `common/scripted_effects` has three explicit control-flow policy conflicts;
  the engine withholds output rather than guessing.

## Runtime

The release-mode probe completed in 37,398 ms without spawning `foch` or
materializing a merged mod. `common/scripted_effects` accounts for 29,176 ms,
so its nested control-flow matching remains the dominant optimization target.
The production 13-candidate shadow run completed with a 0.960x aggregate
Structured-to-Legacy runtime ratio, although scripted effects alone remains
substantially slower because it reaches the expensive conflict path.

The 2026-07-24 focused governments rerun exposed a separate harness startup
cost. End-to-end report time was 258,342 ms while the module unit itself took
177 ms. Sampling showed the process in `ObjectStore::verify_object` and
`digest_tree`, reopening and hashing 5,935 files from three already
content-addressed objects. Probe and shadow restoration now keep a versioned
metadata stamp after a full payload audit. A later process reuses that stamp
only while every path, file size, modification time, executable bit, and
symlink target is unchanged; a metadata change forces a full rehash. Within
one command, each object is verified at most once. The earlier marker-only
focused rerun completed in 339 ms, about 762x faster, but that timing predates
the metadata guard and is diagnostic rather than a current benchmark.
