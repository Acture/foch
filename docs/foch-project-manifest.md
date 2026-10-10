# `foch.toml` Project Manifest

`foch.toml` describes the ordered input that `foch check`, `foch merge`,
`foch graph`, `foch simplify`, and `foch lsp` analyze.

```toml
[project]
game = "eu4"
game_path = "/path/to/Europa Universalis IV"
paradox_data_path = "/path/to/Paradox Interactive/Europa Universalis IV"

[[project.imports]]
kind = "dlc_load"
path = "/path/to/Paradox Interactive/Europa Universalis IV/dlc_load.json"

[[project.mods]]
id = "local_patch"
path = "../mods/local_patch"

[[project.mods]]
steam_id = "2164202838"
```

Paths inside `[project]` are relative to the containing `foch.toml` unless they
are absolute. `[[project.imports]]` currently accepts `kind = "dlc_load"` and
preserves Launcher order. Explicit `[[project.mods]]` entries follow imports
unless an entry sets `position`.

Each explicit mod may identify a local directory with `path`, an installed
Workshop item with `steam_id`, or both an `id` and location. `enabled = false`
excludes an entry. Steam resolution is installed-only: Foch does not subscribe,
download, or update Workshop items.

Use the read-only inspector before analysis:

```fish
foch input inspect ./foch.toml
```

Conflict rules (`[[resolutions]]`), dependency overrides (`[[overrides]]`), and
emission settings share the same file. See
[the resolution reference](./foch-toml-resolutions.md) for their contracts.

## Reviewed culture transformations

Culture merging can identify a unique one-to-one rename with an unchanged
definition body relative to the effective dependency parent. A reviewed mapping
can also identify a rename whose fields changed. It must name the enabled
contributor, its winning culture file, and the SHA256 of that file's original
bytes. Analysis checks the endpoints against the analyzed vanilla and dependency
views, then adapts supported culture references in mods and vanilla together.

`[[cultures.repairs]]` supplies exact edits for a malformed culture source. For
example, suppose the source file contains exactly these bytes, without a newline:

```text
g renamed = { primary = AAA } }
```

With `old` present in the analyzed ancestor, this configuration inserts the
missing group opener and records the reviewed identity change:

```toml
[[cultures.repairs]]
mod = "rename"
file = "common/cultures/base.txt"
sha256 = "5ad75a8ee06a574661832188eee5da85d8d1dcdfd0a84b8b521ec4b728e23965"
edits = [{ start = 1, end = 1, expected = "", replacement = " = {" }]

[[cultures.renames]]
from = "old"
to = "renamed"
mod = "rename"
file = "common/cultures/base.txt"
sha256 = "5ad75a8ee06a574661832188eee5da85d8d1dcdfd0a84b8b521ec4b728e23965"
```

The `mod` value is the resolved input ID shown by `foch input inspect`.
`file` must be a literal `common/cultures/<filename>.txt` path. Hashes cover raw
source bytes, including encoding and line endings. Edit ranges are zero-based,
half-open UTF-8 byte offsets in the decoded original text; all edits use that
same original coordinate space. `expected` must match exactly, and ranges must
not overlap. Repairs are applied before mappings, then parsed and checked for
valid culture hierarchy. Neither operation writes to the source mod.

Mappings and repairs are frozen during merge analysis, included in cache and
review evidence, and checked for source drift again before commit. A changed
source requires reviewing and updating the decision. A conflicting, stale or
unverifiable mapping is not applied. Reviewed identity mappings do not override
field conflicts or prove that a group move preserves group-dependent behavior.

Reference adaptation currently requires the full input inventory: retained-path
analysis and configurations with `extra_ignore_patterns` defer transformations
because omitted scripts may still reference the old identity. Related culture
and script outputs are withheld together when adaptation needs review; unrelated
safe output remains committable. Conditional split/fusion scripts retain their
branches and distinct destinations; the merger does not infer arbitrary
one-to-many or many-to-one identity mappings.

## Reviewed syntax repairs

When a definition's syntax error has no trustworthy automatic repair, the
merge holds its unit for review and its notes offer the one-token repairs that
could be meant, the likeliest first, with a `[[repairs]]` entry for the first
one ready to copy into `foch.toml`:

```toml
[[repairs]]
mod = "b"
file = "common/scripted_triggers/b_triggers.txt"
sha256 = "<SHA256 of the file's raw bytes>"
edits = [{ start = 59, end = 59, expected = "", replacement = "}\n" }]
```

An entry may name any Clausewitz script of an enabled mod, but not a `.lua`,
localisation, CSV or JSON file. It uses the same SHA256 binding and edit
coordinates as culture repairs. After its edits, the definitions they touch must parse with no repair of their own;
elsewhere in the file the usual automatic repairs still apply. A reviewed
repair is frozen with the analysis, adds its evidence to the units that read
the file, and is checked for source drift again before commit: a file whose
bytes no longer match the review is rejected as stale. It never writes to the
source mod.
