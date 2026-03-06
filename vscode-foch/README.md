# Foch for VS Code

Preview VS Code extension for EU4 scripting.

Marketplace pre-release builds use `version = 0.1.0` plus the pre-release publish flag. VS Code Marketplace does not support semver prerelease suffixes such as `0.1.0-preview.1`.

This extension provides:

- `EU4 Script` language association and syntax highlighting
- Completion for builtin trigger/effect names
- Completion for workspace symbols and flag values
- Auto-detection of mod roots via `descriptor.mod`

Current non-goals for this preview build:

- No `goto definition`
- No `hover`
- No editor diagnostics/code actions yet

## Runtime model

The extension now prefers a bundled `foch_lsp` binary.

- If `fochLsp.serverPath` is set, that command is used.
- Otherwise, the extension looks for a bundled binary under `bin/<platform>-<arch>/`.
- If no bundled binary exists, it falls back to `cargo run --quiet --bin foch_lsp --`.

The fallback is useful for local development, but it is not suitable for a user-facing release.

## Local development

Install dependencies:

```bash
npm install
```

Run the extension in an Extension Development Host:

```bash
code /path/to/foch/vscode-foch
```

Then press `F5`.

## Build a bundled server

Build and copy the host-platform `foch_lsp` binary into the extension:

```bash
npm run prepare:server
```

This runs `cargo build --release --bin foch_lsp` in the repo root and copies the result to:

```text
bin/<platform>-<arch>/foch_lsp[.exe]
```

Examples:

- `bin/darwin-arm64/foch_lsp`
- `bin/darwin-x64/foch_lsp`
- `bin/win32-x64/foch_lsp.exe`

For the first public release, build the VSIX on the matching target OS/arch so the packaged binary is correct for that platform.

## Smoke test

Validate that the extension is marked as preview and that the bundled server can be spawned:

```bash
npm run smoke
```

Or run the full local packaging check:

```bash
npm test
```

## Package a pre-release VSIX

```bash
npm run package:vsix
```

This packages the extension for the current host target, for example `darwin-arm64`.

## Publish the pre-release build

Set your Marketplace token first:

```bash
export VSCE_PAT=...
```

Then publish the current host target as a pre-release:

```bash
npm run publish:pre-release
```

The `publisher` field currently targets `acture`. If your Marketplace publisher id differs, update `package.json` before publishing.

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
