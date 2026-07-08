# Foch for VS Code

**Foch for VS Code brings Foch's EU4 analyzer into the editor.**
The extension wraps the bundled `foch` binary's `lsp` subcommand, so editor diagnostics, completion, and navigation run on the same parser and semantic index as the CLI.

Marketplace pre-release builds use `version = 0.1.0` plus the pre-release publish flag. VS Code Marketplace does not support semver prerelease suffixes such as `0.1.0-preview.1`.

## Version stance

The VS Code extension is the first `0.1.0` preview surface for Foch. It is versioned separately from the merge engine's maturity: the extension focuses on EU4 editing, diagnostics, and navigation, while automatic merge remains experimental.

The release boundary and CWTools comparison are tracked in [`docs/lsp-0.1-preview.md`](../../docs/lsp-0.1-preview.md).

## What ships today

| Capability | Coverage |
| --- | --- |
| Language mode | `EU4 Script` association for Paradox script files |
| Syntax highlighting | TextMate grammar for Paradox assignments, blocks, `#` comments, Lua `--` line comments, and level-0 `--[[ ... ]]` block comments, so `common/defines/*.lua` highlights correctly |
| Diagnostics | current-file parse diagnostics, workspace semantic findings, and CWT schema warnings for unknown keys/cardinality |
| Completion | CWT schema-aware keys/aliases, builtin trigger/effect names, and workspace symbols: event ids, scripted effects, decisions, and flag values |
| Hover | CWT schema hover for supported EU4 script contexts, including type, description, scope, and cardinality when available |
| Navigation | goto-definition and find-references for scripted effects/triggers, event ids, flag values, and localisation keys |
| Symbols | document symbols and workspace symbol search from the semantic index |
| Quick fixes | create or append an English localisation stub for `missing-localisation` diagnostics |
| Workspace loading | multi-root scanning across the game directory and multiple mod directories, with mod-root auto-detection via `descriptor.mod` |

## Not yet shipped

- `rename`
- broad code actions beyond missing-localisation stubs
- formatting / pretty print
- semantic tokens
- non-EU4 game profiles

## Runtime model

The extension launches the bundled `foch` binary with the `lsp` subcommand.

- If `fochLsp.serverPath` is set, that command is used as-is.
- Otherwise, the extension looks for a bundled binary under
  `bin/<platform>-<arch>/foch[.exe]` and invokes it as `foch lsp`.
- If no bundled binary exists, it falls back to
  `cargo run --quiet --bin foch -- lsp` (development checkout only).

The fallback keeps local development simple; bundled binaries are the intended runtime model for release builds.

## Local development

From `packages/vscode-foch`:

```bash
bun install
bun run build:extension
bun run prepare:server
code .
```

Then press `F5` to open an Extension Development Host.

## Build a bundled server

`prepare:server` builds and copies the host-platform `foch` binary into the extension:

```bash
bun run prepare:server
```

It runs `cargo build --release --bin foch` in the repo root and copies the result to:

```text
bin/<platform>-<arch>/foch[.exe]
```

Examples:

- `bin/darwin-arm64/foch`
- `bin/darwin-x64/foch`
- `bin/win32-x64/foch.exe`

For public VSIX builds, build on the matching target OS/arch so the packaged binary matches the Marketplace artifact.

## Smoke test

Validate the packaged preview surface, bundled extension entry, bundled language-client helper, bundled server binary, and a real `initialize` / `shutdown` LSP handshake:

```bash
bun run smoke
```

Or run the full local packaging check:

```bash
bun run test
```

`build:extension` bundles `extension.js` into `dist/extension.js`, with `vscode` kept external and the language client dependency bundled into the extension entrypoint. It also vendors the language client's process-termination helper under `dist/vendor/`, avoiding Bun workspace symlinks or a runtime `node_modules` tree in the VSIX.

## Package a pre-release VSIX

```bash
bun run package:vsix
```

This packages the extension for the current host target, for example `darwin-arm64`.

## Publish the pre-release build

Set your Marketplace token first:

```bash
export VSCE_PAT=...
```

Then publish the current host target as a pre-release:

```bash
bun run publish:pre-release
```

The `publisher` field currently targets `acturea`.

## Recommended settings

Example settings for local development:

```json
{
  "fochLsp.serverPath": "",
  "fochLsp.serverArgs": [],
  "fochLsp.serverCwd": "/path/to/foch",
  "fochLsp.gamePath": "/Users/acture/Library/Application Support/Steam/steamapps/common/Europa Universalis IV",
  "fochLsp.modPaths": [
    "/path/to/foch/tests/corpus/control_military_access"
  ],
  "fochLsp.autoDetectMods": true,
  "fochLsp.autoDetectModsMax": 300
}
```

The client automatically sets matching files to the `EU4 Script` language (internal id `foch-eu4`).

Current semantic coverage:

| Root family | Coverage |
| --- | --- |
| `events`, `decisions`, `common/scripted_effects`, `common/scripted_triggers`, `common/diplomatic_actions`, `common/triggered_modifiers`, `common/defines` | parse diagnostics, symbols, and semantic inference |
| `interface`, `common/interface`, `gfx` | parse diagnostics; full scope/symbol inference is not yet enabled |
