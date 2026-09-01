# Foch Desktop

Player-facing Tauri application for Foch's EU4 inspect/analyze/review workflow.

The Rust backend links the root `foch` library directly. It must not spawn or bundle the
CLI, read merge-quality JSONL as a product API, or add filesystem or shell capabilities
without a task-specific requirement.

The frontend already contains the input-readiness screen, analysis progress and
cancellation controls, disposition summary, paginated unit browser, and unit detail
pane. The APP-003 backend supplies their live data.

The APP-003 backend surface is limited to:

- `inspect_input`
- `start_merge_analysis`
- `cancel_merge_analysis`
- `get_merge_analysis_summary`
- `list_merge_units`
- `get_merge_unit`

Only one analysis may be queued or running. Terminal analyses are bounded, and unit
lists are filtered and paginated before crossing IPC. `AnalyzedMerge`, raw reports,
syntax trees, and generated artifact bytes stay in Rust.

There is no desktop commit/export command in this checkpoint. `MergeSession` design is
deferred.

## Commands

Run from the repository root:

```fish
bun install --frozen-lockfile
bun run --cwd apps/foch-desktop format:check
bun run --cwd apps/foch-desktop lint
bun run --cwd apps/foch-desktop typecheck
bun run --cwd apps/foch-desktop test
bun run --cwd apps/foch-desktop build
```

Start the development application with:

```fish
bun run --cwd apps/foch-desktop tauri dev
```
