# Merge-quality review pack

`foch-mq review-pack` freezes the evidence needed to compare the current
Structured rollout with the committed Legacy baseline. It is an audit artifact,
not a new merge benchmark denominator.

## Fixed selection

The committed
`crates/foch-merge-quality/tests/fixtures/review-pack-selection.json` binds:

- EU4 `v1.37.5.0` / Steam build `15918133`;
- six exact dataset snapshot IDs;
- six exact archived Legacy measurement and output-CAS IDs;
- the BLAKE3 hashes of `legacy-baseline.json` and `expected.json`;
- all 36 Legacy scoring units and the 13 Structured rollout units.

Structured units must be a subset of the corresponding Legacy case. Any case,
snapshot, file, baseline, output, or denominator drift aborts the build.

## Baseline refresh

Scoring behavior changes require a scorer semver bump and an explicit refresh
of the candidate baseline fixtures. The refresh restores and rescores the six
pinned Legacy outputs; it does not rerun a Legacy merge or overwrite committed
fixtures:

```fish
target/release/foch-mq review-pack freeze-baseline \
	--out-dir crates/foch-merge-quality/dataset/.work/review-baseline-candidate
```

The output directory contains `legacy-baseline.json`, `expected.json`, and
`review-pack-selection.json`. Review their complete diff and validate a pack
against those three files before replacing the committed fixtures.

## Build contract

Build performs no Legacy merge. For each case it restores the pinned Legacy
output CAS, scores every selected Legacy unit with the current scorer, and
requires exact equality with the frozen scorer `1.3.0` `FileRecord`.

Structured runs once per case with all selected paths grouped into the same
playset, for at most six child-process executions. A failed, timed-out, or
conflicted grouped run remains explicit case evidence. It produces
`insufficient_evidence` proposals and is never retried as per-file merges or
replaced by a Legacy winner.

```fish
cargo build --release -p foch-merge-quality --bin foch-mq
target/release/foch-mq review-pack build \
	--out-dir crates/foch-merge-quality/dataset/.work/review-packs/current \
	--timeout-secs 600
```

Use `--wiki-knowledge-snapshot-id <pack-id>` to bind an already verified
advisory Wiki snapshot. Wiki evidence cannot promote a proposal to an accepted
positive judgment by itself.

## Artifact layout

The pack contains:

- `manifest.json`: identities, case-run outcomes, artifact hashes, and CAS
  bindings;
- `summary.json`: fixed denominators and execution/proposal counts;
- `units/<scoring-unit-id>.json`: 49 Legacy/Structured evidence records;
- `proposals.jsonl`: one input-bound provisional annotation per unit.

After the pack is complete, `build` idempotently appends the same proposals to
`dataset/annotations.jsonl`. Stable annotation IDs make a repeated build an
`already_present` result rather than a duplicate record.

Each unit binds the base snapshot, ordered source and compatch CAS objects,
human and candidate semantic hashes, current scorer result, diagnostics,
selected kernel, and optional Wiki snapshot. Raw order-insensitive atom
differences remain in the evidence for audit. A candidate is proposed as
`equivalent` with `ast_relation = exact_equivalent` when that raw comparison is
exact, or with `ast_relation = logical_equivalent` when the shared Structured
module normalizer proves differing raw ASTs equivalent. Every unresolved or
unavailable candidate is proposed as `insufficient_evidence`.

Accepted records in the same append-only dataset annotation log must supersede
prior records rather than mutate them. Non-identical positive
adjudications require explicit family-invariant or runtime evidence.
Non-identical GUI adjudications require runtime evidence.

## Verify and inspect

Verification executes no merge. It verifies artifact and CAS hashes, checks the
installed game/base identity, recomputes all current semantic evidence and
`FileRecord`s, and reconstructs the review-pack identity:

```fish
target/release/foch-mq review-pack verify \
	--pack-dir crates/foch-merge-quality/dataset/.work/review-packs/current
```

`show` is read-only:

```fish
target/release/foch-mq review-pack show \
	--pack-dir crates/foch-merge-quality/dataset/.work/review-packs/current

target/release/foch-mq review-pack show \
	--pack-dir crates/foch-merge-quality/dataset/.work/review-packs/current \
	--case 3635635014 \
	--path events/Elections.txt \
	--kernel structured
```

The full build remains a manual acceptance step because it runs real grouped
merges and restores the local corpus payload.
