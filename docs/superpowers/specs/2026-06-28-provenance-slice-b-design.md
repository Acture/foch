# Provenance slice B design

## Scope

Build the two remaining provenance channels on top of the existing adopted-definition provenance map and sidecar. The merge output must remain byte-identical when the new GUI tooltip flag is off. LSP hover is always-on, but only reads existing `.foch/foch-provenance.json` sidecars and never writes game files.

## Channel 1: LSP hover

Extend the existing `textDocument/hover` path before schema hover fallback. The handler will parse the open document, find the hovered top-level assignment key, locate the nearest ancestor `.foch/foch-provenance.json`, compute the document path relative to that merged-output root, and look up `definition_provenance[relative_path][key]`. On a hit it returns Markdown `Merged from <mods>` with the same key range; on a miss it falls back to the current schema hover. This keeps LSP behavior deterministic and avoids game-file pollution.

## Channel 2: GUI tooltips

Add an explicit `--gui-tooltip` merge flag instead of overloading `--provenance`: provenance comments/sidecar remain the diagnostic channel, while in-game tooltip injection is a separate, invasive output mutation. The flag requires provenance collection and is only honored for EU4 GUI families under `interface/` and `common/interface/`.

For GUI files, after patch merge computes adopted provenance and before emitting, walk the merged AST. For each named GUI widget whose name appears in the provenance map, inject `tooltip = <generated_loc_key>` only if that widget has no existing tooltip. Existing modder tooltips are never overwritten. Generated keys are deterministic (`foch_provenance_<stable hash>`) and localisation lines are written to a generated UTF-8-BOM English file under `localisation/`, mapping each key to `Merged from <display names>`.

## Testing

Add an LSP unit test that builds a temporary merged tree with a provenance sidecar and verifies hover Markdown. Add a GUI merge e2e fixture covering: flag off output has no tooltip/localisation; flag on injects a tooltip and localisation for a widget lacking tooltip; an existing tooltip remains unchanged.

## Cache/versioning

Because `--gui-tooltip` changes merge output, bump `MODSET_CACHE_FORMAT_VERSION` and include the flag in the modset cache key. Do not bump DAG base cache unless non-flag output changes.
