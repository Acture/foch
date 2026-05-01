# Foch for VS Code

Preview VS Code extension for EU4 scripting, backed by the merged `foch`
binary's `lsp` subcommand.

Marketplace pre-release builds use `version = 0.1.0` plus the pre-release publish flag. VS Code Marketplace does not support semver prerelease suffixes such as `0.1.0-preview.1`.

This extension provides:

- `EU4 Script` language association and syntax highlighting
- Completion for builtin trigger/effect names
- Completion for workspace symbols and flag values
- `goto definition` for scripted effects, event ids, flag values, and localisation keys
- Diagnostics for current-file parse errors
- Diagnostics for workspace semantic findings
- Multi-root scanning for `game` + multiple `mod` directories
- Auto-detection of mod roots via `descriptor.mod`

Current non-goals for this preview build:

- No `hover`
- No `find references`
- No `rename`
- No code actions yet

## Runtime model

The extension launches the bundled `foch` binary with the `lsp` subcommand
(previously a separate `foch_lsp` binary; merged in 0.1.0-alpha so a single
`cargo install` ships everything).

- If `fochLsp.serverPath` is set, that command is used as-is.
- Otherwise, the extension looks for a bundled binary under
  `bin/<platform>-<arch>/foch[.exe]` and invokes it as `foch lsp`.
- If no bundled binary exists, it falls back to
  `cargo run --quiet --bin foch -- lsp` (development checkout only).

The fallback is useful for local development, but bundled binaries are the
intended runtime model for release builds.

## Local development

Install dependencies:

```bash
bun install
```

Run the extension in an Extension Development Host:

```bash
code /path/to/foch/packages/vscode-foch
```

Then press `F5`.

## Build a bundled server

Build and copy the host-platform `foch` binary into the extension:

```bash
bun run prepare:server
```

This runs `cargo build --release --bin foch` in the repo root and copies the
result to:

```text
bin/<platform>-<arch>/foch[.exe]
```

Examples:

- `bin/darwin-arm64/foch`
- `bin/darwin-x64/foch`
- `bin/win32-x64/foch.exe`

For the first public release, build the VSIX on the matching target OS/arch so the packaged binary is correct for that platform.

## Smoke test

Validate that the extension is marked as preview and that the bundled server can be spawned:

```bash
bun run smoke
```

Or run the full local packaging check:

```bash
bun run test
```

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

- script roots: `events`, `decisions`, `common/scripted_effects`, `common/diplomatic_actions`, `common/triggered_modifiers`, `common/defines`
- UI parsing roots: `interface`, `common/interface`, `gfx`

UI files currently contribute parse diagnostics, but not full scope/symbol inference.
