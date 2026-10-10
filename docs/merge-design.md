# Merge Design

This document defines the current product merge contract. Foch is an EU4-aware
N-way semantic merger, not a generic three-way text merger and not a wrapper
around exact-path overwrite order.

## Goals

- preserve compatible contributions from an ordered EU4 mod playset;
- model verified loader semantics at the correct file, definition, or
  definition-module boundary;
- use the analyzed EU4 base snapshot as the semantic ancestor;
- make genuine ambiguity reviewable instead of silently choosing a winner;
- compute the complete result before confirmation; and
- atomically commit the exact reviewed bytes to a separate mod directory.

Localisation and unsupported content families may use narrower strategies. CWT
rules are evidence for shape and editor behavior, not proof of runtime load or
merge semantics.

The local implementation for preserving gameplay mechanisms across definition
and reference changes is described in
[the culture reference merge design](./superpowers/specs/2026-10-01-culture-reference-merge-design.md).
`merge::transform` owns source-bound edits, frozen transformation plans, cache
identities, and connected output dependencies. `ContentFamilyDescriptor` selects
an adapter; `game::eu4::cultures` supplies culture identity inference, typed
reference adaptation, group semantics and output validation. Tree matching and
DAG merging consume the generic `EntityTransform` contract. Culture is the first
production adapter; this does not establish support for transformations in
other content families. See the [boundary refactor](./superpowers/plans/2026-10-01-generic-transform-adapter.md).

Transformation outputs that share a merge unit are committed or withheld
together, transitively. An incomplete group does not withdraw an independent
group, and `--force` cannot bypass the group's output validation.

## Product flow

```text
inspect input
    -> analyze semantic result
    -> review every unit
    -> confirm target/replacement
    -> commit frozen artifacts
```

`foch merge <input> --out <dir>` performs analysis and review without touching
the target. A TTY confirmation or `--confirm` authorizes commit. In
non-interactive use, analysis remains read-only unless `--confirm` is also
present.

There is no separate CLI merge-plan command in this contract. Path
classification is part of merge analysis. There is also no implemented
`MergeSession`; long-lived session design is deferred.

## Inputs and precedence

An input is either the Launcher's `dlc_load.json` plus sibling `.mod`
descriptors, or a `[project]` manifest in `foch.toml`. Resolution produces:

- the concrete EU4 game root and version;
- the ordered enabled mod contributors;
- declared dependencies and reviewed overrides;
- paired Workshop ACF installation identities when available; and
- the installed EU4 base-snapshot identity.

Playset order is semantic. Every path strategy, revision DAG, review
contributor, cache identity, and report must preserve it. Sorting contributors
merely to make a key deterministic is incorrect.

Source mods and the game installation are read-only. Normal analysis reads
installed Workshop content in place and does not copy or recursively hash whole
trees into an input CAS.

## Units and content families

A merge review unit is one of:

- **file** — a single output-relative path; or
- **definition module** — all files that jointly define one loader-level
  module for a concrete EU4 content family.

Exact-path overlap is neither necessary nor sufficient for a semantic conflict.
The EU4 content-family registry in `src/packages/foch/src/game/eu4/content` decides discovery,
module partition, key policy, ordering, base requirements, and supported merge
behavior.

The analyzed EU4 base snapshot is the ancestor for structural merge. Missing
vanilla input is never treated as an empty ancestor unless the content family
explicitly opts into a verified empty-base policy.

## Analysis strategies

The internal path plan assigns one strategy before materialization:

| Strategy | Contract |
| --- | --- |
| `copy_through` | One effective non-base contribution is copied unchanged. |
| `last_writer_overlay` | Verified loader semantics select the highest-precedence contributor. |
| `structural_merge` | Supported Clausewitz definitions are adapted to the semantic tree and N-way merged against the base ancestor. |
| `localisation_merge` | Keys are unioned; the highest-precedence contributor wins only for the same localisation key. |
| `manual_conflict` | The path cannot be safely analyzed automatically and needs review or withholding. |

Structural analysis builds revision DAGs and definition-module views, adapts
supported EU4 syntax into the semantic-tree kernel, materializes deterministic
Clausewitz bytes into a Rust-owned artifact tree, and re-parses/rechecks that
tree. None of this changes `--out`.

The stable production backend identity is `gumtree-pcs-nway`. The retained
`address-patch` backend is comparative evaluation history and is not the public
product path.

## Review contract

Each planned target resolves exactly once into a `MergeUnitOutcome` with:

- stable normalized ID (`file:<path>` or `module:<family>/<module>`);
- normalized path, content family, and unit kind;
- stable snake-case strategy;
- one disposition;
- concise summary, optional committed output path, and notes; and
- ordered, de-duplicated contributors with display name, precedence, source
  paths, and base-game flag.

Dispositions are:

| Disposition | Meaning |
| --- | --- |
| `safe` | Analysis produced a supported semantic result. |
| `copy` | The effective source can be copied without semantic synthesis. |
| `needs_user_choice` | Supported candidates remain ambiguous and need a reviewed decision. |
| `unsupported_input` | The input shape is outside verified behavior. |
| `engine_failure` | A bounded backend failure/panic was caught for this unit. |
| `deferred` | A configured explicit defer handler intentionally withheld the unit. |

The review summary counts every unit exactly once. Duplicate IDs or output
paths, double resolution, and pending units are invariant failures. If
cross-file pruning removes generated output, the unit remains in review with
`output_path = null`; its semantic disposition is not rewritten.

An implicit interactive/TUI defer remains `needs_user_choice`. `deferred` is
reserved for an explicit configured defer decision. `--force` must not emit an
explicitly deferred unit.

## Source syntax repairs

A source file that fails to parse makes every unit that reads it
`unsupported_input`, and for a definition module that is the whole folder-wide
database. Foch repairs the errors that have one trustworthy reading, in its own
parsed copy only. The source file is never changed.

The file is split at its definition heads, the lines that start at column 1
with `key =` or `key {`. Each segment is parsed on its own, so an error stays
in its definition instead of swallowing the rest of the file. A column-1 line
inside a sound block is recognised because the segments around it parse
cleanly together. For a broken segment Foch tries every one-token edit:

- leave out a `{` or `}`;
- add a `}` before a line;
- add a `{` after an `=` that ends its line; or
- end a string that has no closing quote at the end of its line.

An edit counts only if the segment then parses as exactly one definition. It
is applied when one of three kinds of evidence holds:

- every such edit gives the same tree;
- the schema for the file rejects the block-or-value shape of some value in
  every tree but one, the one with the fewest such values; or
- one tree changes the tree the text gives unedited least: it alone moves
  the fewest statements to another parent, or adds or drops the fewest.

The schema is consulted only where the file's game path is known, which is
how a merge reads every mod file. Only shape is used: the schema's other
diagnostics are not reliable enough yet to choose between readings, and where
its binding cannot tell the trees apart they all keep the same count. Only the
trees that move the fewest statements, and the one the indentation agrees
with most, are checked against it.

Indentation only checks the last choice. When the indentation clearly
favours another tree, with fewer than half as many lines indented other than
their depth, the evidence conflicts and the segment is not repaired.
Indentation is read in the file's own style, tabs or spaces, and lines
indented in the other style count for neither.

A segment that needs two edits, or whose readings tie, is not repaired. Only
a segment on its own is repaired, never one read together with the next. The
repaired file must then parse cleanly as a whole into exactly the repaired
segments; otherwise nothing is repaired. The search is bounded per file and
gives up on a file that would need more. A reviewed repair from `foch.toml`
must parse cleanly by itself; no automatic repair is added to it.

A repair can read the file differently from the game. A stray `}` that closes
a block early makes the game read the rest of that block as top-level
statements, which a definition file does not allow; Foch keeps them in the
block.

Each unit that read a repaired file carries a note naming the mod, file, line,
column, edit and evidence. The merge report lists the same facts in
`source_repairs`.

A file only one mod ships is copied, and the copy is written with its
repairs: each one-token edit is made in the file's own bytes and encoding, so
nothing else in it changes. That requires the decoded text to encode back to
exactly the original bytes; a file where it does not, or a UTF-16 file, is
copied unchanged with a warning. A copied file with an isolated definition is
copied unchanged, with a note.

A broken segment with no trustworthy repair is isolated when it starts with a
definition head whose key the file does not repeat. Its text is left out up to
the next head that follows a line at column 1, so a column-1 line inside the
broken block is not taken for a definition; the rest of the file is parsed as
usual. The merge reads the isolated definition as the mod's parent has it,
never as deleted, so that mod's version of it is missing from the result:

- the unit is held for review as `needs_user_choice`, and its notes and the
  report's `isolated_definitions` name the mod, file, lines and the one-token
  repairs that could be meant, the likeliest first;
- `--force` keeps the unit with the parent's version and a warning, on the
  default backend only. Any other backend would read the absence as a
  deletion, so it always holds the unit.

A reviewed `[[repairs]]` entry in `foch.toml` applies one of the proposals, or
any exact edit, to Foch's copy of the file, after which it merges as usual; see
[the project manifest](./foch-project-manifest.md#reviewed-syntax-repairs).

Text that names no definition, a key the file repeats, as events repeat
`country_event`, and any other error without a repair still make the file
unsupported, and `--force` does not change that.

Repairs are chosen by the parser's stable diagnostic codes, never by message
text. They do not apply to `.lua` files, which a Lua interpreter loads.
Repairs and isolation are part of the analysis rules identity, so cached
snapshots and frozen analyses made before them are not reused. How EU4 itself
handles each error has not been confirmed in a game log.

## Resolution policy

Reviewed static decisions live in `foch.toml` `[[resolutions]]`. Exact conflict
IDs and files outrank pattern rules. Built-in handlers are dispatched by the
main library’s merge resolution registry. See
[foch-toml-resolutions.md](./foch-toml-resolutions.md).

Foch never applies a broad last-writer policy merely because it would avoid a
conflict. `--force` is limited to supported `needs_user_choice` fallbacks; it
does not reinterpret unsupported input, engine failure, or explicit defer as
safe.

## Commit contract

Analysis freezes:

- the generated artifact tree and its byte identity;
- the ordered product-input attestation;
- the installed base-snapshot identity;
- any existing output bytes consumed by `keep_existing`; and
- the fingerprint of a non-empty replacement target when the user asks to
  replace it.

`AnalyzedMerge::commit` revalidates those guards and atomically installs the
frozen tree. It does not parse, run a merge backend, resolve a new conflict, or
read a different external resolution file. Drift fails before target mutation.

`--confirm` authorizes the reviewed commit but not replacement of a non-empty
directory. Replacement needs a separate TTY confirmation/fingerprinted
authorization. Batch jobs must use a new or empty target.

## Partial results and statuses

Unsafe units are withheld at file or complete definition-module granularity;
unrelated safe units may still commit.

- `ready` — all required output is safe and validation passed.
- `partial_success` — safe output committed while one or more units were
  withheld or used an explicit supported fallback.
- `blocked` — an explicit non-conflict gate prevents activation/commit.
- `fatal` — input, validation, emission, I/O, cancellation, or invariant failure
  prevents a trustworthy analyzed result.

A partial result is not permission to activate the generated mod blindly. The
report must identify withheld units and activation safety.

## Output tree

A committed result contains only the separate generated mod tree. It never
normalizes or modifies source inputs.

```text
<out>/
  descriptor.mod
  common/...
  events/...
  localisation/...
  .foch/
    foch-merge-plan.json
    foch-merge-report.json
    foch-provenance.json      # only when requested
    foch-merge-trace.json     # only when requested
```

`foch-merge-plan.json` records the internal deterministic target
classification used by analysis; its name is an artifact compatibility
contract, not a separate command. `foch-merge-report.json` records status,
validation, backend/scope/base attestation, ordered product-input identity,
withheld units, and handler outcomes.

Provenance output is opt-in. When disabled, it must not perturb ordinary
emitted bytes.

With `--provenance`, `.foch/foch-provenance.json` is a version-1 object:
`version: 1`, `files` keyed by output-relative path, and `mod_names` keyed by
source ID. Each file contains `content_hash` (lowercase BLAKE3 of its exact final
bytes) and `definitions` (merge key to adopted source IDs in playset precedence).
Only surviving generated definitions and their source names are recorded. The
report retains its existing `definition_provenance` map and adds
`provenance_mod_names`; fingerprints belong to the frozen output artifact.

Shared EU4 editor hover and `foch lsp` append these sources to schema help for
unique top-level block definitions whose family uses assignment-key identity.
Attribution requires matching final bytes and a buffer equal to the decoded
disk file. The nearest lexical output/mod boundary is authoritative; directory
links cannot switch outputs or relative file identities. Missing, corrupt,
stale, ambiguous, or unsupported records leave schema help intact. Old raw-map
sidecars remain readable with an explicit historical, unverified label.

The same option appends provenance to supported GUI static tooltips in
structurally merged `.gui` files under `interface/` and `common/interface/`.
Named `iconType`, `instantTextBoxType`, `buttonType`, and `guiButtonType` controls
can wrap `pdx_tooltip`; `guiButtonType` also supports `tooltipText`. Generated
localisation retains `$ORIGINAL_KEY$` and adds `Merged from ...`, using the
control's adopted subtree sources. Delayed tooltip fields remain unchanged.
Missing tooltips are added only with verified base context and wholly mod-created
ancestry; `--no-game-base` does not establish that absence. Dynamic, competing,
unsupported fields or an unverified final-tree projection are left unchanged.
Localisation is emitted only for surviving scripts through the same mechanism
as diplomatic-condition provenance. See the [public verification record](./project-status.md#editor-and-gui-merge-provenance-2026-10-01); automated tests do not establish in-game rendering.

## Determinism and safety invariants

- identical frozen inputs, policy, base snapshot, and Foch version produce the
  same review units and output bytes;
- paths in artifacts use normalized `/` separators and safe relative paths;
- contributors retain semantic precedence;
- source trees are never destinations;
- output is staged and installed atomically under a same-target lock;
- failure before commit leaves the target unchanged;
- replacement authorization is invalidated by target drift; and
- product acceptance re-parses and scores generated output but does not launch
  EU4 or prove in-game playability.

## Required tests

Merge changes should cover the smallest owning boundary first and then the CLI
integration path. The contract requires regressions for:

- ordered input and base ancestry;
- every analysis strategy and review disposition;
- definition-module aggregation and cross-file pruning;
- conflict-handler precedence and explicit defer;
- bounded backend failure/panic conversion;
- no writes during analysis;
- artifact, input, base, prior-output, and replacement-target drift;
- commit without semantic recomputation;
- partial success withholding only unsafe units; and
- deterministic artifacts across repeated runs.
