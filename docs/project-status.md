# Project Status

Last updated: 2026-03-23

## Summary

`foch` is currently a Paradox mod static analysis toolkit with:

- a working CLI analyzer
- a semantic indexing layer
- a preview-grade LSP and VS Code extension

It is not yet an automatic mod merger that generates a merged mod artifact on disk.

That distinction matters because the repository looks substantially more complete when judged as an analyzer than when judged against the original end goal.

## Deferred Workstreams

The repository also has a separate localisation compatibility track.

- It is not part of the mainline parser cleanup path.
- It is not a blocker for the current analyzer-to-merge work.
- It should be tracked and reported separately, especially for double-byte / EU4DLL-related handling and other compatibility gaps that do not change the core semantic analysis contract.

## Repository Shape Today

### Top-level user-facing commands

The current CLI surface is analyzer-oriented:

- `foch check`
- `foch config`

There is no merge-oriented command in the current command set.

### Implemented analyzer behavior

The repository can currently:

- load a playset
- locate mod roots and selected game roots
- parse supported Paradox script files
- build semantic indexes across base game and mods
- detect rule violations and semantic findings
- emit text and JSON reports
- export a semantic graph

The existing rule and analysis system covers, at minimum:

- playset integrity
- descriptor and dependency issues
- file conflicts
- scripted effect definition/reference issues
- semantic visibility and missing localisation diagnostics

Those localisation diagnostics are useful signals, but they should remain a distinct compatibility workstream rather than being folded into the main analyzer or merge milestone framing.

### Editor-side behavior

The repository also includes:

- `foch_lsp`
- a VS Code extension under `vscode-foch/`

The implemented editor feature set is:

- syntax highlighting
- completion
- goto definition
- diagnostics

The repository README still lists these editor features as not implemented:

- `hover`
- `find references`
- `rename`
- code actions

## Verified Checks

The following were verified locally during assessment:

- `cargo test --all-targets --all-features`: passed
- `cargo clippy --all-targets --all-features -- -D warnings`: passed
- `npm test` in `vscode-foch/`: passed

One quality gate is not currently green:

- `cargo fmt --check`: fails because of existing formatting drift in tracked Rust files

There is also an existing unrelated modification in the `tree-sitter-paradox` submodule worktree.

## Completion Estimates

### If the goal is "static analyzer + language tooling"

- overall: about `75%-80%`
- CLI analyzer core: about `85%-90%`
- LSP / editor experience: about `65%-70%`

### If the goal is "automatic Paradox mod merger using static analysis"

- overall: about `25%-35%`
- analysis prerequisites: about `60%-70%`
- actual merge planning and merge artifact generation: about `5%-10%`

The low second estimate is intentional. The current repository can already detect merge-relevant facts, but it does not yet materialize a merged mod.

## Why It Is Not Yet a Generated Merge Tool

The repository is still missing the product behavior that defines an automatic merger:

- no `merge-plan` command
- no `merge` command
- no merged output directory generation
- no generated `descriptor.mod`
- no merge manifest for produced artifacts
- no rewrite or emission layer for Clausewitz script output

The current codebase does contain merge-adjacent groundwork:

- file conflict detection
- structural merge candidate hints for selected paths
- semantic index composition across overlays

That groundwork is useful, but it is still upstream of an actual generated merge workflow.

The next merge milestone has now been decomposed into contract freeze, merge IR, materialization, and revalidation slices so coordination can stay explicit instead of treating `merge` as one opaque task.

## Recommended Reading

Use the docs set as follows:

- read [auto-merge-roadmap.md](./auto-merge-roadmap.md) for milestone ordering
- read [merge-design.md](./merge-design.md) for the implementation contract of the first merge-capable version
