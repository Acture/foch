# Foch VS Code Client

Preview VS Code extension for EU4 scripting.

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
code /Users/acture/repos/modus-foch/vscode-foch-lsp
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

## Package a VSIX

```bash
npm run package:vsix
```

This command expects `npx @vscode/vsce` to be available.

## Recommended settings

Example settings for local development:

```json
{
	"fochLsp.serverPath": "",
	"fochLsp.serverArgs": [],
	"fochLsp.serverCwd": "/Users/acture/repos/modus-foch",
	"fochLsp.gamePath": "/Users/acture/Library/Application Support/Steam/steamapps/common/Europa Universalis IV",
	"fochLsp.modPaths": [
		"/Users/acture/repos/modus-foch/tests/corpus/control_military_access"
	],
	"fochLsp.autoDetectMods": true,
	"fochLsp.autoDetectModsMax": 300
}
```

The client automatically sets matching files to the `EU4 Script` language (internal id `foch-eu4`).
