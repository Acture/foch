# GUI provenance research inputs

These are **unverified historical fixtures, not production acceptance cases**.
P-830 preserves them for later GUI research. Its new bounded static-tooltip
implementation does not register these inputs with a merge test runner or
adopt the old `containerWindowType` injection expectations.

The seven files in `eu4_gui_provenance_tooltip/` are copied byte-for-byte from
commit `8db8b9b80f72bebda12c75869ca0b4a292542b87`, under
`crates/foch-engine/tests/fixtures/playsets/eu4_gui_provenance_tooltip/`.
That commit calls itself incomplete and not gate-verified. The preceding design
is in `42b71ddd744b044c1b896ab0d046ca205957c09f`, at
`docs/superpowers/specs/2026-06-28-provenance-slice-b-design.md`.

The original test was
`eu4_gui_provenance_tooltip_is_opt_in_and_preserves_existing_tooltips` in
`crates/foch-engine/tests/merge_e2e.rs` at the prototype commit. Its expectations
are retained here as research questions, not asserted current behavior:

| Case | Preserved input | Historical expectation, unverified |
| --- | --- | --- |
| Positive insertion | GUI A's `containerWindowType` named `shared_widget`, with no tooltip | With the proposed `gui_tooltip` option on, insert a generated tooltip and English localisation saying `Merged from GUI A`. |
| Existing-tooltip exclusion | GUI A's `containerWindowType` named `keeps_tooltip`, with `tooltip = existing_tooltip` | Preserve that assignment exactly once, without adding another tooltip. |
| Option-off exclusion | The same ordered two-mod playset, provenance on and proposed GUI option off | Emit neither generated GUI tooltip keys nor `localisation/foch_provenance_l_english.yml`. |
| Independent sibling | GUI B's `containerWindowType` named `other_widget`, with no tooltip | The prototype traversal could annotate this too; the original test did not independently assert its attribution. |

All three widgets are direct children of `guiTypes`. Despite its name,
`shared_widget` appears only in GUI A. This fixture does not exercise two mods
editing one widget, nested widgets, duplicate names, anonymous widgets, sprites,
or other widget types. It has no vanilla ancestor, no localisation definition
for `existing_tooltip`, and no game screenshot or runtime record. Its small
layout blocks do not establish a usable in-game screen.

The old test inspected the first generated tooltip line rather than locating
the `shared_widget` block explicitly. Even a passing old test would not prove
complete widget attribution, EU4's acceptance of the field, or hover behavior.
No pass result is imported with these files.

Keep the relative descriptor paths and `dlc_load.json` order intact. A future
focused test must supply the base evidence required by the current content
family and adjudicate its own expected output. Do not add these files to the
fixed Workshop acceptance denominator or treat the old `gui_tooltip` option as
an available current CLI option.

See the [public merge contract](../../../../../../docs/merge-design.md) for current
source boundaries and the remaining runtime verification. Private experimental
interpretation lives in the project's notes repository.
