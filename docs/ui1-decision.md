# UI1 TUI resolver decision

## Decision

CASE B: block `ui1-tui-resolver` for alpha.

## Rationale

The `ui-tui` worktree at commit `8ef87b9` contains substantial ratatui scaffolding, but it is a read-only report viewer built around optional future fields (`conflicts`, `findings`, `mods`) rather than the current slim `MergeReport` payload (`conflict_resolutions`, `leaf_conflicts`, `handler_resolutions`). It also does not implement the todo's write path for reviewed picks into `foch.toml`.

Alpha conflict resolution is already covered by the in-merge ratatui resolver in `crates/foch-engine/src/merge/tui_conflict_handler.rs`, introduced by `2434842` and refined through `0456590`. That flow prompts only surviving conflicts during `foch merge`, supports candidate picks with `1`-`9`, and persists decisions through the same `[[resolutions]]` writer used by the CLI prompt.

Merging `ui-tui` wholesale would bring a stale branch that diverges heavily from current `master` and would duplicate the shipped alpha UX. A separate post-merge report browser is useful, but it is beta scope rather than an alpha blocker.

## Path forward

Revisit this in beta when the report-viewer requirements are explicitly scoped around the current report schema and any extra payload needed for safe post-run edits. A beta implementation should either reuse the existing conflict-resolution entry writer or expose it from the engine, render `conflict_resolutions` grouped by file, and write exact `conflict_id` `prefer_mod` entries to `foch.toml` from selected `leaf_conflicts`.
