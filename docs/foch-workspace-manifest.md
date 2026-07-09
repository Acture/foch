# `foch.toml` Workspace Manifest

`foch.toml` can describe the workspace that `foch check`, `foch merge`, `foch graph`, `foch simplify`, and the VS Code LSP should analyze.

```toml
[workspace]
game = "eu4"
game_path = "/path/to/Europa Universalis IV"
paradox_data_path = "/path/to/Paradox Interactive/Europa Universalis IV"

[[workspace.imports]]
kind = "dlc_load"
path = "/path/to/Paradox Interactive/Europa Universalis IV/dlc_load.json"

[[workspace.mods]]
id = "local_patch"
path = "../mods/local_patch"

[[workspace.mods]]
steam_id = "2164202838"
```

Paths inside `[workspace]` are resolved relative to the containing `foch.toml` unless they are absolute. `[[workspace.imports]]` currently supports the launcher `dlc_load.json` format and preserves launcher mod order. Explicit `[[workspace.mods]]` entries append after imports unless `position` is set.

Steam support is installed-only: `steam_id` resolves through the configured or detected Steam root and does not subscribe, download, or update Workshop items.

Use `foch workspace resolve ./foch.toml` to inspect the resolved game root and mod roots before running analysis or merge commands.
