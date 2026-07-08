# LSP 0.1 Preview

Last updated: 2026-07-08

This document defines the first public VS Code/LSP preview surface. It is intentionally narrower than the merge engine roadmap: the extension can be useful and publishable before automatic merge is reliable for arbitrary modlists.

## Positioning

Foch LSP should not compete on validator count. It should compete on a better EU4 editing experience and on Foch-native semantics: modlist-aware symbol graphs, contributor/provenance context, merge-risk signals, and eventually code actions that connect editor findings to merge/corpus workflows.

Foch uses CWT schema data, including `cwtools-eu4-config`, as one input. It does not embed the CWTools VS Code extension or CWTools' validator engine. The current implementation parses `.cwt` files into Foch's own `CwtSchemaGraph`, binds schema context inside the Rust LSP, and emits a deliberately small set of editor diagnostics and completions. That distinction matters: CWT is the schema language/data source; CWTools is another product built on that ecosystem.

CWTools' public README and Marketplace page list multi-game support, syntax errors, autocomplete, hover/tooltips, validators, go-to-definition, find-references, and missing-localisation code actions. Treat that as a feature-breadth benchmark, not as the target user experience: [cwtools-vscode](https://github.com/cwtools/cwtools-vscode), [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=tboby.cwtools-vscode).

| Axis | CWTools | Foch 0.1 LSP |
| --- | --- | --- |
| Primary job | Broad Paradox script editing and validation | Fast, curated EU4 editor entrypoint into Foch's modlist-aware analyzer |
| Game coverage | Multi-game: Stellaris, HOI4, EU4, and partial/in-progress support for others | EU4-only |
| Analysis unit | Open mod/workspace plus vanilla context | Game root plus multiple mod roots, matching Foch's check/merge semantic index |
| Schema source | CWT configs plus CWTools implementation | CWT configs plus Foch's own Rust binding/diagnostic layer |
| Strongest value | Validator breadth and existing ecosystem coverage | Lower-noise UX, cross-mod semantic index, provenance-ready definitions/references, graph/merge/corpus alignment |
| 0.1 limitation | User experience can be noisy or opaque for large real mod workspaces | Does not yet match CWTools' breadth of validators or multi-game coverage |

The public claim should be:

> Foch uses CWT facts where they are useful, but the product goal is not "more validators". The goal is a calmer EU4 editing loop that understands the same modlist, semantic graph, conflicts, and eventual merge artifacts as the CLI.

## Why Build An LSP

The LSP is the fastest path to make Foch useful before merge is fully trusted.

- It gives users value while they edit, not only after a long merge/check run.
- It turns the semantic index into interactive navigation: definitions, references, symbols, and missing-localisation stubs.
- It provides an ergonomic test surface for analyzer quality: a noisy LSP diagnostic is a concrete analyzer bug, not an abstract corpus metric.
- It lets Foch reuse CWT schema facts with stricter UX discipline: fewer diagnostics, clearer codes, bounded output, and quick fixes that are safe inside the current mod root.
- It creates the front door for future merge-aware actions: "why is this symbol conflicted?", "which mod wins this definition?", "show the merge plan for this file", and "apply this narrow resolution".
- It gives agents and MCP tools a stable language surface to query without reverse-engineering VS Code state.

## 0.1 Acceptance Surface

The `0.1.0` VS Code extension preview is acceptable when the repository can prove all of these:

- The extension package version is `0.1.0` and `preview = true`.
- The VSIX bundles a platform-specific `foch` binary and launches it as `foch lsp`.
- The smoke gate performs a real `initialize` / `shutdown` LSP handshake against the bundled binary.
- The server advertises:
  - text document sync
  - completion
  - hover
  - goto-definition
  - find-references
  - document symbols
  - workspace symbols
  - quickfix code actions
- The client exposes:
  - `EU4 Script` language mode
  - TextMate highlighting for supported EU4 paths
  - configured game root and mod roots
  - descriptor-based mod-root auto-detection
  - idle behavior in unrelated workspaces with no configured or detected mod roots
  - bundled-server, configured-server, and development cargo fallback launch modes
- The shared analyzer provides:
  - current-file parse diagnostics
  - workspace semantic findings
  - CWT schema unknown-key/cardinality diagnostics where schema context binds
  - schema-aware hover/completion in supported contexts
  - navigation for scripted effects/triggers, event ids, flag values, and localisation keys
  - missing-localisation quick fix that creates or appends an English stub in the current mod root

Current gate commands:

```fish
cargo test -p foch-cli lsp
cargo test --workspace
bun run --cwd packages/vscode-foch test
bun run --cwd packages/vscode-foch package:vsix
```

## Explicit Non-Goals

Do not claim these for `0.1.0`:

- rename support
- formatter / pretty printer
- semantic tokens
- broad code actions beyond missing-localisation stubs
- non-EU4 game profiles
- automatic merge reliability for arbitrary modlists
- feature parity with CWTools validators

## Multi-Game Path

Multi-game support should be layered instead of bolted onto EU4 names.

1. Split the LSP surface into `ClausewitzLanguageProfile` and `GameSemanticProfile`.
2. Keep parse diagnostics, syntax highlighting, and structural document symbols in the generic profile.
3. Move EU4 builtins, CWT schema pack lookup, localisation conventions, root classifiers, and completion tables behind the EU4 semantic profile.
4. Add a game-profile registry that maps workspace settings and file roots to a profile id.
5. Ship future non-EU4 profiles as profile-lite first: parse, schema hover/completion, and diagnostics before merge-aware semantics.
6. Only promote a game profile beyond lite after corpus fixtures prove definitions, references, localisation, and cross-file symbols are stable.

The near-term second-game milestone should not be "all Paradox games". It should be "one profile-lite game proves the abstraction without weakening EU4".

## Agent Skill Design

Do not commit agent configuration files to this repository. The repo should carry only the product-facing design and command contracts; a local/global skill can be generated from this section.

Recommended skill name: `foch-lsp-preview`.

Trigger contexts:

- Inspecting or improving Foch's VS Code extension.
- Debugging `foch lsp`, LSP diagnostics, completion, hover, navigation, or code actions.
- Preparing a `0.1.0` VS Code pre-release.
- Comparing Foch LSP with CWTools or deciding whether a feature belongs in 0.1.

Core workflow:

1. Read `docs/lsp-0.1-preview.md`, `packages/vscode-foch/README.md`, and `docs/project-status.md`.
2. Check `git status --short --branch`; never stage generated VSIX/binary/dist artifacts.
3. For server changes, run `cargo test -p foch-cli lsp` before broader gates.
4. For extension changes, run `bun run --cwd packages/vscode-foch test`.
5. Before release claims, run `bun run --cwd packages/vscode-foch package:vsix`.
6. Keep the outward claim EU4-only unless a game profile has fixture coverage.

Useful references for the skill body:

- `crates/foch-cli/src/lsp.rs` for server capabilities and LSP tests.
- `packages/vscode-foch/extension.js` for client wiring and commands.
- `packages/vscode-foch/scripts/smoke-test.js` for release-surface smoke coverage.
- `packages/vscode-foch/package.json` for marketplace metadata and settings.

## MCP Design

The first Foch MCP should be local, read-only by default, and workspace-root constrained. Its job is to let agents inspect Foch language/semantic state without scraping CLI text or opening VS Code.

Recommended package: `packages/foch-mcp`, implemented in TypeScript with stdio transport. The server should call stable Foch CLI/library surfaces and return bounded structured data. It should not expose arbitrary shell execution.

Initial read-only tools:

| Tool | Purpose | Notes |
| --- | --- | --- |
| `foch_lsp_capabilities` | Launch the configured/bundled server and return advertised capabilities from `initialize` | Mirrors the VS Code smoke gate |
| `foch_workspace_targets` | Resolve configured game/mod roots and descriptor-detected mod roots | Include role, path, and source |
| `foch_document_diagnostics` | Return parse/schema/semantic diagnostics for one document | Bound output and include diagnostic code/range/severity |
| `foch_workspace_symbols` | Search workspace symbols by query | Return kind, name, location, mod id |
| `foch_definition_at` | Resolve definition at document position | Return zero or more locations with symbol metadata |
| `foch_references_at` | Resolve references at document position | Return bounded locations grouped by symbol |
| `foch_missing_localisation_fixes` | Return proposed missing-localisation stubs without writing files | This mirrors the quick fix but remains read-only |

Future write tools must be opt-in and narrow:

- `foch_apply_localisation_stub`
- `foch_apply_resolution_suggestion`
- `foch_write_graph_artifacts`

Each write tool must declare whether it is destructive, idempotent, and workspace-scoped, and it must return the exact files it changed.

MCP evaluation questions should be stable and read-only, for example:

- Given a fixture mod root, which missing-localisation keys would Foch propose stubs for?
- At a scripted-effect callsite, what definition locations does Foch resolve?
- Which workspace symbols match a query, and which mod root contributed each one?
- Does the bundled LSP server advertise quickfix code actions?

## Release Claim Checklist

Before calling the LSP preview `0.1.0`-ready:

- `packages/vscode-foch/package.json` is `0.1.0` and preview.
- `packages/vscode-foch/CHANGELOG.md` describes the actual shipped surface.
- `README.md` and `packages/vscode-foch/README.md` state EU4-only preview scope.
- `docs/project-status.md` matches this document.
- The four gate commands above pass on the release host.
- Generated `bin/`, `dist/`, and `.vsix` artifacts remain ignored unless intentionally packaged outside git.
