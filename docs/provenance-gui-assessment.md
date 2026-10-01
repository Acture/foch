# P-830 GUI provenance assessment

P-830 appends source attribution to a bounded set of static GUI tooltip fields
through the existing `--provenance` option. Installed EU4 files establish the
authored field names and widget types. The June prototype's generic `tooltip`
insertion is not revived. General widget support and in-game behavior remain
unverified; no additional GUI CLI option is introduced.

This assessment is dated 2026-10-01 and covers baseline
`4e8862e737f0669a57e46a4a53c8af46122e15d7` plus the P-830 working-tree changes.
Historical design commit `42b71ddd744b044c1b896ab0d046ca205957c09f` and prototype
commit `8db8b9b80f72bebda12c75869ca0b4a292542b87` are research evidence. The latter
explicitly describes itself as incomplete and not gate-verified. No historical
pass, EU4 runtime result, or accepted Workshop cohort is claimed here.

## Installed source evidence and implemented scope

The game was located through Steam's `libraryfolders.vdf` and the project's
existing `game_path`, at
`G:/SteamLibrary/steamapps/common/Europa Universalis IV`.
Its `launcher-settings.json` reports `EU4 v1.37.5.0 Inca (491d)`.
Read-only inspection of relevant base interface files found:

| Base file and line | Widget type and name | Authored field | Matching English localisation |
| --- | --- | --- | --- |
| `interface/endgamedialog.gui:107` | `iconType`, `end_score` | `pdx_tooltip = "LEDGER_SCORE"` | `localisation/ledger_l_english.yml:83` |
| `interface/endgamedialog.gui:120` | `instantTextBoxType`, `score_value` | `pdx_tooltip = "LEDGER_SCORE"` | `localisation/ledger_l_english.yml:83` |
| `interface/musicplayer.gui:62` | `buttonType`, `music_next_button` | `pdx_tooltip = "MUSICPLAYER_NEXT"` | `localisation/musicplayer_l_english.yml:6` |
| `interface/countrytradeview.gui:644` | `guiButtonType`, `sort_name` | `pdx_tooltip = "TRADE_NAME_SORT_TOOLTIP"` | `localisation/tradenodes_l_english.yml:182` |
| `interface/eventwindow.gui:119` | `guiButtonType`, `event_option_goto_button` | `tooltipText = "GOTO"` | `localisation/common_sense_l_english.yml:1671` |

`countrytradeview.gui:645` separately declares `pdx_tooltip_delayed`. The base
files also use the case-sensitive legacy field `delayedTooltipText`. These
fields remain untouched. Many legacy `tooltip` assignments are empty; that
is not evidence for inserting a generic `tooltip` on arbitrary controls.

The new [GUI renderer](../src/merge/output/materialize/gui_provenance.rs) handles
`.gui` outputs under `interface/` and `common/interface/`, inside `guiTypes`.
It supports exactly the four widget spellings in the table and requires a
unique, nonblank textual `name`. It wraps a sole nonblank safe `pdx_tooltip`, or
`tooltipText` only on `guiButtonType`, as
`$ORIGINAL_KEY$\n\nMerged from <display names>`. Names use the existing
sanitization helper and fall back to source IDs. The original reference is
preserved verbatim, without trimming or rewriting source localisation.

Dynamic expressions, padded keys, malformed or repeated fields, competing
immediate channels, nonblank generic `tooltip`, and already generated wrappers
are left unchanged. `containerWindowType`, sprite definitions, and unobserved
widget spellings are outside this scope.

If every immediate and delayed channel is absent or blank, the renderer may
add or fill `pdx_tooltip` with source-only text. This requires complete nonempty
origin records throughout the widget subtree, no vanilla ancestry, and a
verified ancestor mode (`Required` or `KnownAbsent`). With game-base analysis
explicitly disabled, mod-only lineage does not establish a new widget and
missing-field insertion is skipped. Authored static fields can still be
annotated in that mode.

These are source-level transformation boundaries. They do not prove that a
mod-created widget name has no engine callback, that each field is shown on
every screen, or that the generated tooltip renders correctly. The game was
not launched and no Workshop tree was recursively scanned.

## What transfers from the old prototype

The design at `docs/superpowers/specs/2026-06-28-provenance-slice-b-design.md`
in `42b71dd` proposed a separate `--gui-tooltip` flag. In `8db8b9b`, the retired
`crates/foch-engine/src/merge/output/materialize/patch_structural.rs` implemented
`inject_gui_tooltips` after patch merge and before emission. It selected
`interface` and `common/interface` descriptor IDs, visited only direct children
of the descriptor's containers, and looked up contributors under `type:name`.
Unnamed or unlisted children fell back to their assignment key.

It appended `tooltip = foch_provenance_<16 hex>` when no direct `tooltip`
assignment existed. Keys hashed output path and widget key without an
occurrence coordinate, so repeated identities could share attribution. The
fixed UTF-8-BOM English file `localisation/foch_provenance_l_english.yml` was
written from a global entries map before cross-file no-op pruning. The patch
also implicitly enabled provenance when its GUI option was enabled.

That injector depended on a simultaneous signature-based container-child
provenance traversal in the retired
`crates/foch-engine/src/merge/planning/patch_deps.rs`. Copying the injector alone
would omit its assumptions; restoring that traversal would introduce a second
lineage system beside the current semantic lineage.

The [preserved research fixture README](../tests/fixtures/provenance_gui_research/README.md)
records the original positive and negative expectations. Seven playset files
retain their exact historical bytes. They cover an absent tooltip, preservation
of an existing tooltip, and option-off behavior on direct `containerWindowType`
children. These expectations are unverified, are not production acceptance,
and do not define the newly supported scope. The old test found the first
generated line rather than addressing a particular widget, and its fixture
contains neither a vanilla ancestor nor a definition for `existing_tooltip`.

## Current content-family and lineage boundaries

The [EU4 family registry](../src/game/eu4/content/families.rs), in
`GUI_TYPES_NAMED_CHILD_TYPES` and the `interface` / `common/interface`
descriptors, defines named-child merge identities for `guiTypes`, `spriteTypes`,
`bitmapfonts`, and `objectTypes`. It uses the `name` field,
`ScalarMergePolicy::GuiWidget`, and edit-wins-over-remove; `gfx` shares these
policies. These are merge policies, not a tooltip-field allowlist. Neither the
`ui` compatibility label nor CWT coverage establishes GUI runtime behavior.

The GUI families remain per-path units: load-policy promotion excludes
`ContainerChildFieldValue`. The explicit per-path fallback for
`interface/state_view` appears in
[`classify_database_entry`](../src/merge/path_plan.rs). The current
`FileAnalysis::NoVerifiedBase` handling in
[materialization](../src/merge/output/materialize.rs) also remains authoritative;
the historical ready-status assertion does not override it.

[`compute_semantic_definition_provenance`](../src/merge/planning/dag_merge.rs)
projects a file partition's adopted sources to top-level assignment keys. A
`guiTypes` entry therefore cannot establish independent attribution for its
nested widgets. The GUI renderer instead normalizes the final file through
`ClausewitzFileAdapter` and requires exact equality with the `File` lineage tree.
A structural AST/tree walk then preserves each widget occurrence, including
nested and repeated names. The renderer unions adopted `sources` within that
widget's subtree and retains precedence order. Cumulative `origins` are used
to bound missing-field insertion, not to credit overridden contributors.

[`structural.rs`](../src/merge/output/materialize/structural.rs) calls
`coalesce_scroll_stack_variants` before output rendering.
[`gui.rs`](../src/merge/gui.rs) can synthesize containers under the scroll-stack
policy. If those changes produce a different normalized tree, the GUI renderer
leaves that file unchanged instead of guessing how old nodes map to new widgets.
Missing lineage, unsupported partitions, or a failed structural projection also
leave GUI output unchanged.

## Relationship to diplomatic-condition tooltips

[`materialize_condition_provenance_tooltips`](../src/merge/output/materialize/provenance_tooltip.rs)
remains restricted to `common/diplomatic_actions/`. That family has
assignment-key definitions, nested `condition` identities based on `tooltip`,
and source-isolated append behavior. Its existing wrapper retains the original
localisation reference and displays `Base:` / `Modified by:` from cumulative
subtree origins. `FinalSemanticProjection` verifies final normalized partitions
before using definition and condition occurrences.

GUI widgets use different authored fields and adopted-source attribution. The
condition implementation supplies a useful pattern for a verified final-AST
projection; its tests do not prove GUI field or runtime semantics.

Both outputs share `ProvenanceTooltipOutput` and the existing per-script
localisation map. GUI keys start `FOCH_PROVENANCE_GUI_`, retaining the existing
`FOCH_PROVENANCE_` prefix recognized by output dependency handling. The
materializer collects entries only for committed rendered scripts,
`reconcile_surviving_output_facts` removes pruned outputs, and
`write_surviving_provenance_localisation` writes surviving entries. The writer
retains BOM, all configured language headers, content-hashed filenames, and
collision checks. Existing external-write, pruning, and keep-existing
localisation-dependency rules still apply.

## Output and cache identities

The prototype bumped `MODSET_CACHE_FORMAT_VERSION` and included its GUI flag in
`build_modset_cache_context`, in the retired
`crates/foch-engine/src/merge/execute.rs`. Those APIs no longer exist under current
`src/` or `apps/`; their version string is not a current invalidation mechanism.

Current [analysis](../src/merge/analyze.rs) materializes output, validates it,
writes report and sidecar artifacts, and freezes the bytes.
[`AnalyzedArtifactTree`](../src/merge/output/artifact_tree.rs) hashes paths, entry
types, and contents and checks its digest before and after copying for commit.
The GUI transformation runs before that freeze and its script/localisation
bytes are committed together. Disabled provenance bypasses GUI transformation.

GUI keys hash the versioned namespace `foch-gui-provenance-v1`, full output path,
normalized widget node ID, field name, and original reference. This distinguishes
repeated widget occurrences and different fields or paths. The shared writer's
content digest also reflects display names and resulting localisation values.

Source snapshot identity is separate: [`snapshot_analysis_identity`](../src/input/mod_snapshot/mod.rs)
includes [`analysis_rules_version`](../src/game/eu4/base/version.rs) and the active
CWT identity. A rendering-only option does not change source semantic snapshots.
Analyzer/family/schema changes must invalidate the corresponding analysis layer;
do not restore a removed modset cache or blanket-bump unrelated caches.

Any future cache of generated outputs must distinguish the provenance option,
transform version, display-name inputs, and ordered source inputs. Any quality
measurement must record the output mode alongside executable, runner, kernel,
scope, and scorer identity. The [product runner](../apps/foch-cli/tests/merge_quality/runner.rs)
binds results to executable bytes, which alone do not distinguish options
passed to the same executable. Historical ungated output cannot be relabeled as
current product evidence. Editor hover itself writes no game output.

## Verification and remaining evidence

The renderer's [focused tests](../src/merge/output/materialize/gui_provenance/tests.rs)
cover authored and delayed channels, nested/repeated names, sibling isolation,
source precedence, adopted versus historical sources, blank fields, incomplete
ancestry, disabled mode, unrelated paths, final-tree drift, idempotence, and
path-specific keys. Development started with a no-op renderer: three positive
tests failed while two abstention tests passed. A separate regression failed
when explicitly disabled vanilla analysis still authorized a missing field;
the ancestor-mode guard fixes that boundary.

The public-merge fixture is separate from the retained June research inputs.
No fixture, parser result, or merge-quality score establishes runtime playability.
A manual EU4 check remains outstanding: record the game version, screen/widget,
language, inputs, original tooltip, appended sources, and interaction behavior.
Expansion to additional widget types or fields requires concrete loader/field
evidence, especially for engine-generated or inherited tooltips.
