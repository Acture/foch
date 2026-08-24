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
