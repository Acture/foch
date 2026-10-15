# Changelog

## 0.1.0

- First preview release.
- Adds `EU4 Script` language association and syntax highlighting.
- Adds bundled `foch lsp` subcommand startup with cargo fallback for development.
- Adds builtin trigger/effect completion and workspace symbol completion.
- Adds CWT schema hover/completion diagnostics, goto-definition, find-references, document/workspace symbols, and a focused missing-localisation quick fix.
- Avoids starting the language server in unrelated workspaces without configured or detected mod roots.
- Prompts for a window reload after `fochLsp.*` settings change so target roots and server launch settings are rebuilt explicitly.
- Hides problems the game's own files already have unless `fochLsp.showGameDiagnostics` is on.
- Labels each problem with what decided it: `cwtools` for a vendored CWTools rule, `foch-cwtools` for foch's supplementary rules or its refinement of a rule, `foch-detect` for foch's own reading of how the game loads a file, and `foch` for parse errors and cross-mod findings.
- Adds a release smoke check for package metadata, bundle layout, and the bundled server's LSP initialize/shutdown handshake.
