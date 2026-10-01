# P-830: recorded merge sources in editor hover

Implement against the root library at `4e8862e`, not the retired crates or the
unverified t5 patch. The shared EU4 editor owns definition lookup and hover
composition. `foch lsp` adapts the result and keeps its open-document text current.

Read `.foch/foch-provenance.json` on each hover. Resolve the
nearest output boundary, use an exact output-relative UTF-8 path, preserve source
ID order, and only match unique top-level block assignments whose merge identity
is their assignment key. Do not guess nested, repeated, or field-derived keys.
Missing, malformed, inaccessible, or unmatched metadata leaves schema help intact.

At the user's request, merge supplements its existing adopted-source map with
display names and output fingerprints. The version-1 artifact contains `version`,
`files` (output-relative path to BLAKE3 `content_hash` and `definitions`), and
`mod_names` (ID to display name). Hash the exact final script bytes before artifact
freeze. Keep the report's existing definition map and omit names/sidecar when
provenance is disabled. This reuses existing lineage, not a second source tracker.

For version 1, require the fingerprint to match the current disk bytes and the
decoded script to match the open buffer, then show **Merge sources** with names
and IDs in original precedence order. Byte verification survives copies and
timestamp preservation; a mismatch suppresses attribution. Decode with the
existing Paradox text decoder, including BOM handling.

The old unversioned map remains readable as **Recorded merge sources**, explicitly
describing the last merge rather than verification of the current file. Require
the open buffer to equal the decoded disk script; reject visibly stale files newer
than that sidecar. Those legacy checks cannot prove original content after copies
or preserved timestamps and must never be presented as verified attribution.

Do not cache sidecars in this slice: updates, deletion, corruption, and replacement
must be observed on the next request. Stop at the nearest merge-output or mod
boundary, even when its provenance is absent or broken. Do not fall through to a
parent's sources. Render IDs as escaped Markdown text, retaining their identity.

Positions and ranges use UTF-16 code units at the editor boundary and byte offsets
at the parser boundary, including CRLF and supplementary Unicode characters.
Apply incremental LSP edits correctly and discard text on close. Perform hover
filesystem reads off the async runtime thread.

Tests cover single/multiple contributors; same keys across files and roots;
ambiguous keys; dirty documents; stale, missing, corrupt, changed and deleted
sidecars; source order and Markdown; Unicode and CRLF; schema composition; and
the real LSP hover handler. Include a fixture produced by the public merge path.

The user also requested in-game GUI provenance and explicitly chose appending to
existing tooltips. Under existing `--provenance`, support the exact named widget
types with authored EU4 1.37.5 evidence: `iconType`, `instantTextBoxType`,
`buttonType`, and `guiButtonType`. Wrap a sole static `pdx_tooltip`, or the observed
`guiButtonType.tooltipText`, retaining `$ORIGINAL$` and appending adopted sources.
Do not alter delayed fields or guess dynamic/competing channels. Add a missing
`pdx_tooltip` only to a wholly mod-created widget without vanilla ancestry and
without any nonempty immediate/delayed tooltip channel. The base context must
be verified (`Required` or `KnownAbsent`); an explicitly disabled base cannot
establish that a widget is new.

Normalize the final GUI AST against its existing file lineage before projecting
widget subtrees; if a GUI transformation broke that correspondence, abstain.
Keep repeated widget occurrences distinct and take sources from each widget's
surviving subtree, not a parent container. Feed generated localisation through
the existing per-script survivor accounting and frozen artifact tree. Disabled
mode stays unchanged. In-game runtime rendering remains a manual verification
step; installed authored field examples do not prove every engine callback.

Preserve the historical fixture and record the supported slice in
`docs/provenance-gui-assessment.md`. Hover itself writes no game files.
