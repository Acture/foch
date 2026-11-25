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

## Companion native plugin declarations (`[[plugins]]`)

A Steam Workshop companion mod—or a playset `foch.toml`—can declare the native
Foch DLL plugins it expects inside `foch.toml` under `[[plugins]]`, without
introducing a separate manifest filename. Because `[project]` is optional in
the `foch.toml` schema, a mod directory can ship a `foch.toml` containing only
`[[plugins]]` tables as a discovery declaration rather than a playset manifest:

```toml
[[plugins]]
id = "dev.foch.eu4-dynamic-trade-blackhole"
repo = "Acture/eu4-dynamic-trade-blackhole"
game = "eu4"
game_versions = "=1.37.5"
required = true
min_version = "0.1.0"
sha256 = "abababababababababababababababababababababababababababababababab"
```

### `[[plugins]]` field reference

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `id` | string | *(required)* | Unique plugin identifier (for example, `"dev.foch.eu4-dynamic-trade-blackhole"`). |
| `repo` | string | omitted | Optional GitHub `owner/repo` release source used to install the plugin when it is not already in the local store. |
| `game` | string | `"eu4"` | Target game identifier. |
| `game_versions` | string | omitted | Optional game version requirement string (for example, `"=1.37.5"`). |
| `required` | boolean | `true` | Whether the declaring mod requires the plugin (`true`) or lists it as optional (`false`). |
| `min_version` | string | omitted | Optional minimum plugin version requirement in Semantic Versioning format (`X.Y.Z`). |
| `sha256` | string | omitted | Optional expected 64-character hexadecimal SHA256 digest of the plugin release archive. |

### Validation rules

`[[plugins]]` entries are validated when `foch.toml` is parsed:

- **Strict schema (`deny_unknown_fields`)**: unknown keys in `foch.toml` or
  inside any `[[plugins]]` entry are rejected.
- **Valid, unique `id`**: `id` must be a non-empty string with no whitespace,
  and duplicate `id` values within the same `foch.toml` are rejected. When
  multiple `foch.toml` files are merged along the config search path, the first
  declaration for a given `id` is retained.
- **Non-empty identifiers and strings**: `game` must be a non-empty identifier
  without whitespace. When set, `repo` and `game_versions` must not be empty or
  whitespace-only.
- **Semver `min_version`**: when set, `min_version` must parse as a valid
  Semantic Versioning (`semver::Version`) string.
- **64-hex `sha256`**: when set, `sha256` must be exactly 64 ASCII hexadecimal
  digits.

### Mod-directory checks and automatic UI discovery

- **`foch check <mod-dir>`**: passing a mod directory path to `foch check`
  (or `foch check --fix <mod-dir>`) resolves the directory through its
  `descriptor.mod` as a single enabled mod. A sibling `foch.toml` carrying only
  `[[plugins]]` and no `[project]` table coexists cleanly with `descriptor.mod`
  (whereas passing a `foch.toml` file path directly as a playset manifest still
  requires `[project]`).
- **Playset discovery in bare `foch` and `foch-desktop`**:
  `discover_declared_plugins` scans `<mod-root>/foch.toml` across the active
  playset's mods in playset order. Declarations sharing an `id` are merged into
  a single `DeclaredModPlugin` entry that preserves first-seen order, lists all
  declaring mods in `source_mods`, sets `required = true` if any declaring mod
  requires the plugin, and keeps the first supplied `repo`, `game_versions`,
  `min_version`, and `sha256`. Missing `foch.toml` files are ignored, and a
  malformed file is surfaced as a non-fatal warning. Both the bare `foch` TUI
  (`Launch & Plugins`) and `foch-desktop` feed these discovered declarations
  into the shared plugin overview so missing or disabled companion plugins are
  highlighted and can be installed from `repo` and enabled directly.
