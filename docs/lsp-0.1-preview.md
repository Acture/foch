# LSP 0.1 Preview

Last updated: 2026-08-25

This page defines the independently versioned VS Code/LSP preview. The editor
can be useful before Foch's automatic merge quality is proven across arbitrary
modlists, but it must not imply broader game or merge support.

## Positioning

Foch LSP is a focused EU4 editing surface over the same parser, concrete game
semantics, and cross-mod index used by check and merge analysis. Its value is a
calm editing loop: bounded diagnostics, schema-aware assistance, navigation,
and contributor context.

Foch consumes CWT data, including `cwtools-eu4-config`, but does not embed the
CWTools extension or validator engine. `src/game/schema` parses and compiles the
reusable schema language; `src/game/eu4/editor` interprets it for EU4. CWT
coverage alone does not establish runtime or merge semantics.

| Axis | Foch 0.1 LSP |
| --- | --- |
| Game | EU4 only |
| Input | Configured game root plus one or more mod roots |
| Schema | Vendored CWT compiled by Foch's Rust schema layer |
| Semantic context | Cross-mod definitions/references aligned with `foch check` |
| Primary limitation | Narrower validator breadth and no claim of automatic-merge reliability |

## Acceptance surface

The `0.1.0` preview is acceptable when the repository proves:

- extension version `0.1.0` with preview metadata;
- a platform-specific bundled `foch` binary launched as `foch lsp`;
- a real `initialize` / `shutdown` smoke handshake;
- text sync, completion, hover, definition, references, document symbols,
  LSP workspace symbols, quick-fix code actions, and bounded diagnostics;
- EU4 Script language mode and TextMate highlighting;
- configured game/mod roots and descriptor-based mod-root detection;
- idle behavior when an editor workspace has no EU4 input;
- reload on `fochLsp.*` setting changes;
- bundled, explicitly configured, and development server launch modes;
- current-file parse diagnostics and cross-mod semantic findings;
- CWT unknown-key/cardinality diagnostics only where schema context binds;
- schema-aware completion/hover for supported contexts;
- navigation for scripted effects/triggers, event IDs, flag values, and
  localisation keys; and
- a missing-localisation quick fix limited to the current mod root.

Current gates:

```fish
cargo test -p foch-cli lsp
cargo test --workspace
bun run --cwd packages/vscode-foch test
bun run --cwd packages/vscode-foch package:vsix
```

## Non-goals for 0.1

- rename or formatting
- semantic tokens
- broad write actions beyond the localisation stub
- non-EU4 game support
- automatic-merge reliability for arbitrary modlists
- feature-count parity with another Paradox editor extension
- desktop merge review or commit/export behavior

## Merge-aware evolution

Future editor actions may explain which mod contributes a definition, why a
review unit is withheld, or which narrow resolution would apply. They should
consume stable root-library analysis/review data rather than scrape CLI text or
invent an editor-only precedence model.

The old phrase “show the merge plan for this file” now means “show the analyzed
review outcome for this file.” There is no separate merge-plan command in the
current product flow.

## Future games

Reusable CWT machinery already lives in `src/game/schema`. A second game must
still add a concrete `src/game/<game>` implementation with fixtures for:

- root discovery and loader order;
- content families and definition-module boundaries;
- builtins, localisation, and semantic indexing;
- base-data identity and coverage; and
- any merge behavior it claims.

Until those gates exist, the extension and CLI remain EU4-only. A schema pack
or parse-only demo must not be presented as supported game behavior.

## Contributor workflow

1. Read this page, `packages/vscode-foch/README.md`, and
   `docs/project-status.md`.
2. Check `git status --short --branch`; do not stage generated VSIX, binary, or
   `dist/` artifacts.
3. For server changes, start with `cargo test -p foch-cli lsp`.
4. For extension changes, run the package test before the package smoke.
5. Keep outward claims EU4-only.

Useful source references:

- `apps/foch-cli/src/lsp.rs` — server capabilities and tests
- `src/game/schema` — reusable CWT syntax/query machinery
- `src/game/eu4/editor` — concrete EU4 schema interpretation
- `packages/vscode-foch/extension.js` — client wiring and commands
- `packages/vscode-foch/scripts/smoke-test.js` — release-surface smoke
- `packages/vscode-foch/package.json` — Marketplace metadata and settings

## Read-only automation boundary

Any future agent/MCP adapter should use stable library/LSP data, constrain
paths to one configured editor root, and return bounded structured results. A
minimal read-only surface could expose capabilities, input targets, document
diagnostics, symbol search, definition, references, and proposed localisation
stubs.

Arbitrary shell execution is out of scope. Write operations must be separately
opt-in, narrow, declare whether they are destructive/idempotent, and return the
exact files changed.

## Release claim checklist

- package metadata and changelog match the shipped surface;
- README files state EU4-only preview scope;
- `docs/project-status.md` matches the source checkpoint;
- all four release gates pass on the release host; and
- generated `bin/`, `dist/`, and `.vsix` artifacts remain ignored unless
  intentionally packaged outside Git.
