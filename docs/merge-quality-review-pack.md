# Merge-quality review pack

The review pack is a schema `1.0.0` audit artifact for the fixed six-case
rollout selection. It compares archived Legacy evidence with product-generated
`semantic_tree` / `full_product_merge` evidence without creating another
benchmark denominator. It is separate from V2 measurement cohorts and schema
`2.0.0` measurement reports.

There is no `foch-mq` command or default self-spawning builder. The library
entrypoint is `build_review_pack_with_runner`, and callers must inject the
runner that executes the public product artifact.

## Fixed selection

The committed
`crates/foch-merge-quality/tests/fixtures/review-pack-selection.json` binds:

- EU4 `v1.37.5.0` / Steam build `15918133`;
- six exact dataset snapshot IDs;
- six exact V1 Legacy scorer `1.3.0` measurement and output-CAS IDs, bound to
  executable `0507a19d…` and scorer config `8beffefe…` from the committed
  `legacy_address_patch_reference` cohort;
- the BLAKE3 hashes of `legacy-baseline.json` and `expected.json`;
- all 36 Legacy scoring units and the 13 historical Structured rollout units.

The Structured units are a subset of the corresponding Legacy case. Any case,
snapshot, path, baseline, output, or denominator drift invalidates the pack.
The pinned V1 measurements remain scorer `1.3.0`
`legacy_address_patch_reference` evidence; they are never relabeled as product
output.

## Acceptance workflow

The supported execution shape is a repository-owned ignored acceptance test
with an injected product runner. That test must perform the sequence below in
one auditable workflow:

1. Restore and rescore the six pinned Legacy output CAS objects without
   rerunning a Legacy merge.
2. Invoke `build_review_pack_with_runner` with the exact public `foch` artifact
   and a bounded timeout. Run at most one grouped product merge per case, and
   require its execution attestation to name `kernel = semantic_tree` and
   `scope = full_product_merge`.
3. Reject the build unless every Structured case is `ready`, exits zero,
   reports valid output, has zero manual conflicts and zero handler
   resolutions, and contains no error, fatal, conflict, or handler-resolution
   diagnostic. Never retry per path or substitute a Legacy winner.
4. Run `verify_review_pack` without executing another merge.
5. Review the complete staged diff before replacing any committed fixture.

Run the fixed workflow from the repository root:

```fish
scripts/merge-quality/review-pack.fish
```

The script selects only the ignored `review_pack_acceptance` test and supplies
its exact guard token. Do not substitute a test binary path or reintroduce a
hidden child protocol.

`legacy-baseline.json`, `expected.json`, and `review-pack-selection.json` are
immutable committed inputs. There is no freeze/regeneration API or staged
baseline-generation step. The supported review-pack surface is build plus
verify; changing those fixtures requires a separately reviewed dataset
migration.

## Build contract

For each case, the builder restores the pinned Legacy output, scores every
selected Legacy unit, and requires exact equality with the frozen `FileRecord`.
The injected runner receives one grouped request per case. Selected paths bound
the review evidence; they do not narrow product execution, which remains
`semantic_tree` / `full_product_merge`. A successful result is archived into
the same verified CAS format inside the staged review pack. It never appends
the canonical dataset's object stream or writes its canonical CAS.

The pack contains:

- `manifest.json`: identities, portable case-run attestations, diagnostic
  kinds, artifact hashes, and CAS bindings. It never embeds the raw shadow
  manifest or filesystem paths;
- `summary.json`: fixed denominators and execution/proposal counts;
- `units/<scoring-unit-id>.json`: 49 Legacy/Structured evidence records;
- `proposals.jsonl`: input-bound provisional proposals.

Building evidence does not implicitly accept or append adjudications. Accepted
records in `annotations.jsonl` are a separate explicit operation and must
supersede prior records rather than mutate them.

Each Structured output hash must resolve in the pack-local `objects/` store
before construction succeeds. Precomputed evidence without those objects is
rejected rather than producing a pack that only fails later during
verification.

Each unit binds the base snapshot, ordered source and compatch CAS objects,
human and candidate semantic hashes, scorer result, diagnostics, selected
review arm (`legacy` or `structured`), and optional historical
knowledge-snapshot ID. Raw atom differences remain available for audit. Exact
or proven logical equivalence may produce an `equivalent` proposal;
unavailable or unresolved candidates produce `insufficient_evidence`.

Non-identical positive adjudications require explicit family-invariant or
runtime evidence. Non-identical GUI adjudications require runtime evidence.

## Verification and inspection

`verify_review_pack` executes no merge. It applies the same clean
`semantic_tree` / `full_product_merge` attestation rules as the builder,
verifies artifact and CAS hashes, checks the installed game/base identity,
recomputes semantic evidence and `FileRecord`s, and reconstructs the
review-pack identity.

Inspection is ordinary file review: read `summary.json`, `manifest.json`, and
the selected unit JSON with standard tools such as `jq`. There is no package
command for `show` or `verify`.
