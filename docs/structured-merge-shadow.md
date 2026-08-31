# Structured merge rollout history

Status: frozen research evidence. The former live Legacy-versus-Structured
shadow command surface has been removed. The public non-interactive
`foch merge` path now uses the SemanticTree product kernel; current quality is
to be measured by a fresh V2 product cohort, not by replaying the old dual-kernel
harness.

## What the shadow slice established

The historical vertical slice connected a parser-independent merge kernel to
the dependency-DAG final join. Its deliberately narrow rollout required:

- an ordinary `events` family or a merge-ready `DefinitionModule` with
  `AssignmentKey` identity;
- exactly two final DAG sinks;
- a non-empty real vanilla ancestor;
- no unresolved conflict or intent-only patch on either sink or the ancestor.

Unsupported shapes failed explicitly. Once Structured was selected, the
experiment did not silently copy a Legacy winner. Those constraints and the
resulting evidence informed the current product integration, but they are not a
separate runtime mode now.

## Architecture retained by the product

`crates/foch-merge-kernel` owns parser-independent normalized trees,
provenance, matching, revision classes, PCS ordering constraints, and typed
structural conflicts. `foch-engine` owns the Clausewitz adapter and
content-family policy:

- `country_event` and `province_event` are anchored by inner `id`;
- `option` blocks are anchored by inner `name`;
- provable `if`/`else_if`/`else` chains are normalized as guarded control flow;
- assignment kinds include their key;
- comments are trivia rather than positional content;
- assignment-key modules merge complete runtime-effective views and retain
  inactive definitions deterministically.

Exact subtree hashes are structurally verified before becoming `Exact`
matches. Left/right matching is seeded through the common base. A conflicted
merge exposes only a tentative tree and cannot be materialized as a successful
product output. Delete-versus-move, reparent, and ordered-reorder cases remain
conflict-visible.

Selected matching and amalgamation logic is adapted from Mergiraf 0.18.0 at
revision `e8e13887b85b8cb56b1dc1624c5f94e3d39182b6`. Attribution and the
upstream GPL-3.0 text live under `crates/foch-merge-kernel/`.

## Frozen evidence model

The retired harness ran Legacy and Structured in isolated processes and output
directories, then bound both arms to one content-derived comparison identity.
That identity covered the playset, resolved mod trees, retained vanilla files,
base snapshot, effective `foch.toml`, external resolution files, executable
bytes, force setting, and selected paths. Timeouts, crashes, malformed output,
or identity drift were terminal evidence; partial output was not compared.

Corpus rollout records used the immutable dataset CAS and the fixed human
compatches. The full denominator contained 36 multi-source units. Candidate
selection did not change that denominator: unselected rows retained their
historical Legacy result for projection only. Event candidates additionally
checked parseability, event/option identity, duplicate anchors, orphan control
flow, and ordered control-flow shape.

The remaining tracked artifacts are evidence, not executable workflows:

- `crates/foch-merge-quality/dataset/shadow_measurements.jsonl` is a frozen
  append-only historical stream;
- `tests/fixtures/legacy-baseline.json` and `expected.json` bind the historical
  scorer view;
- `tests/fixtures/review-pack-selection.json` preserves the former six-case,
  36-Legacy-unit, and 13-Structured-unit selection as historical metadata.

Do not append new results with an ad hoc test binary or reconstruct the retired
child protocol. The review-pack builder, verifier, acceptance test, and Fish
entrypoint are retired; the selection fixture is not a runnable workflow.

## Historical rollout results

The 2026-07-22 projection evaluated the 12 fixed `common/**` candidates plus
GE-EE `events/Elections.txt` while retaining Legacy evidence for 23 other
units. It projected strict/adjudicated acceptance from 7/36 to 12/36 and the
non-GUI view from 7/21 to 12/21, with zero previously accepted units lost.
Candidate outcomes were 5 improved, 0 regressed, 1 unchanged accepted, 4 review,
2 Structured conflicts, and 1 safety failure. Aggregate candidate runtime was
0.960x Legacy.

These numbers are rollout history, not product quality. In particular, the
underlying Legacy denominator was produced by an evaluation-only address-patch
kernel, and the selected Structured arm covered only 13 units.

### Elections history

The 2026-07-21 focused Elections run matched all 1,217 human semantic atoms and
projected one improvement. A generalized 2026-07-22 run later failed the
control-flow safety check. The 2026-07-23 focused rerun restored the safety gate
but shared only 1,186 of 1,217 human atoms, with 21 candidate-only and 31
human-only atoms, so its final disposition was `needs_review`. None of these
focused results establishes or changes a V2 product baseline.

## Current acceptance paths

Use `scripts/merge-quality/acceptance.fish` for the fixed 14-case V2 product
acceptance. It invokes the exact ignored
`workshop_product_corpus_acceptance` test, resolves the committed logical cases
from read-only Workshop content and same-library ACF files, launches the
release-built public `foch merge`, and records scorer `2.1.0`,
`semantic_tree`, and `full_product_merge`. This path does not read the legacy
`objects/` store; completed results retain compact scorer evidence.

Use `scripts/merge-quality/common-module.fish` for the separate fixed 12-unit
Common applicability gate. After a complete V2 cohort exists, use
`scripts/merge-quality/structured-rollout.fish` to compare its fixed scoring
units with the pinned Legacy cohort. None of these scripts runs a live
dual-kernel shadow comparison or restores a V1 CAS object. They are auxiliary
analysis, not alternative product acceptance paths. The first fixed 14-case V2
product cohort has not yet been completed at this checkpoint.
