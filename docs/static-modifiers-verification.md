# Static-modifier contribution verification

Scope: [P-553](https://linear.app/acturea/issue/P-553) and
[P-556](https://linear.app/acturea/issue/P-556), 2026-09-05, developed from
`06e3fa7` on the P-553 worktree. This is focused product-path evidence, not an
accepted Workshop cohort or an in-game playability result.

## Reproduced failures and fixes

Before the change, both public `analyze_merge` → `commit` and
`foch merge --confirm` marked the independent-change fixture safe but emitted:

| Field | Base | Tax mod | Morale mod | Vanilla carrier | Generated before | Required |
| --- | --- | --- | --- | --- | --- | --- |
| `global_tax_modifier` | 0.10 | 0.15 | 0.10 | 0.10 | 0.35 | 0.15 |
| `land_morale` | 0.10 | 0.10 | 0.20 | 0.10 | 0.40 | 0.20 |

`common/static_modifiers` selected `ScalarMergePolicy::Sum`, which synthesizes
from candidate final values, including unchanged carriers. Neither summing final
values nor assuming additive deltas establishes compatibility between authors'
intentions. The family now uses `Conflict`: existing structural selection keeps
independent changes and equivalent values, while different final values require
review unless explicit dependency ancestry establishes a downstream override.
No generic numeric reducer, loader policy, root classification or extractor was
changed.

The first conflict regression also found that an unchanged `vanilla` mod was
offered alongside `tax` and `up` as a final candidate. P-556 fixes candidate
construction in `src/merge/kernel/nway.rs` for divergent values and delete/modify
conflicts. Complete revision evidence and the ancestor remain available;
unchanged carriers are excluded from choices, while equivalent actual changes,
changed surviving subtrees and deletion tombstones remain. The product report
and public `ConflictHandler` receive the corrected candidates. No display-only
filter or new review API was introduced.

The kernel candidate fix is isolated in commit `3832e6c`; the family policy and
product-entrypoint regressions are the following P-553 commit.

## Product matrix

Every row runs twice through public analysis/commit and twice through CLI
preview/confirmation. The public test installs an explicit synthetic EU4
snapshot in an isolated process. CLI tests build it through `foch data build`.
Both use the committed [fixture](../tests/fixtures/static_modifiers/README.md).

| Scenario | Expected result |
| --- | --- |
| Independent tax/morale changes + unchanged carrier | Both changes retained; unchanged discipline and base-only definition retained |
| Reversed independent playset | Same values; contributors reflect reversed precedence |
| Changed tax + unchanged vanilla | Only tax is adopted; no duplicated base value |
| Equivalent tax changes | One value; both actual contributors retained |
| Different final tax values | One unresolved leaf; complete static-modifier module withheld |
| Opposite tax changes | Unresolved, with no automatic cancellation or sum |
| Adapter depending on both conflicting mods | Adapter's final value; no intermediate prompt |
| Adapter depending on tax + independent morale | Adapter tax and independent morale both retained |
| Ordinary later mod with adapter-like bytes | Still unresolved without declared ancestry |
| Equivalent conflicting candidates + another value | Equivalent contributors remain; vanilla is not a choice |
| Delete/modify + unchanged carrier | Deletion and modification remain choices; vanilla is not a choice |

Assertions cover reparsed definitions and every numeric field, static-definition
resource references, ordered contributor/source paths, adopted provenance,
stable review and conflict IDs, exact ancestor/candidate snippets delivered to
`ConflictHandler`, deterministic output bytes, and unchanged source bytes.
The numeric fixture has no symbolic references; its extractor exposes definition
names as `static_modifiers_definition` resource references.

With unresolved leaves, the independent localisation file still commits and
the report says `partial_success`. The CLI currently exits successfully for
this valid partial commit, so the tests assert omission and semantic report
content rather than interpreting exit code as full success. The family retains
its existing complete-module review boundary and overlay materialization policy.

## Bounded Workshop observation

The probe uses fixed case `3630934157`: ordered sources Religions and Cultures
Expanded (`3342969370`) and Europa Expanded (`2164202838`). The compatch is
resolved for case identity but is not used as a prediction feature or assumed
ground truth. Source roots are read in place. ACF identities are verified before
and after execution; byte comparisons are limited to the selected family.

Observed EU4 version: `v1.37.5.0`, Steam build `15918133`. Base snapshot:
`sha256:fec03656e735ed4680b8f121685a83c66f59130374b92b05f557a1c1c880e195`.
The generated module reparsed successfully with 380 distinct definitions and
no duplicate top-level definitions. `prestige` retained:

- unchanged `land_morale = 0.1`;
- RCE's `rce_monthly_religion_mechanic_sylvan_affinity_change = 2`;
- EE's `monthly_frankish_chivalry = 0.05`;
- adopted provenance `[3342969370, 2164202838]` in input order.

The final P-553/P-556 artifact repeated these assertions successfully in 55.52
seconds. Its executable hash, ordered ACF identities, selected semantic output
and immutable-input checks are committed in
[the compact evidence artifact](./evidence/static-modifiers-workshop.json).
Full local outputs remain in the artifact directory recorded there.

This demonstrates those selected contributions in this installed pair. It does
not adjudicate every scalar in the real module, the whole playset's references,
missing optional dependencies, or runtime loader precedence. Full source
semantic snapshots are still read/built before retention: the cold observation
took about 109 seconds before a sandbox lock restriction stopped commit; a warm
retry including commit completed in about 55 seconds. The merge itself retained
only the four family paths and produced one review unit. The probe prints this
cost boundary and writes separate artifacts under `target/p553-workshop/`.

## Reproduction and validation

Focused synthetic gates:

```fish
cargo test -p foch --test merge_static_modifiers
cargo test -p foch --lib merge::kernel::nway
cargo test -p foch-cli --test cli_integration static_modifiers_cli
```

All focused gates passed on the final worktree, as did:

```fish
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --quiet
```

The full workspace suite required permission to bind a local Unix socket used
by an existing output-transaction test. It passed without code changes or test
exclusions after that sandbox restriction was removed.

Installed-input probe, for the maintainer to run when source versions change:

```fish
set -lx FOCH_CACHE_ROOT "$PWD/target/p553-probe-cache"
cargo test -p foch-cli --test merge_quality_corpus static_modifiers_probe::workshop_static_modifiers_product_probe -- --ignored --exact --nocapture
```

It requires current installed EU4 base data and paired Workshop ACF records,
reads source mods without copying them, and may take minutes on a cold cache.
Each run prints a new artifact directory with `input.json`, `review.txt`,
`evidence.json` and committed output. It never appends to the cohort JSONL.

The complete fixed 14-case gate remains a separate manual run:

```text
cargo acceptance
```
