# Foch Desktop

Player-facing Tauri application for Foch.

APP-001 contains only the shared desktop shell and build/test boundary. Steam, EU4,
Launcher, base-data, and current-playset discovery belong to APP-002. Analysis commands
and merge state belong to APP-003 after the engine progress and complete-preview
contracts exist.

The Rust backend links `foch-engine` and `foch-core` directly. It must not spawn or
bundle the `foch` CLI, read merge-quality JSONL as a product API, or add filesystem and
shell capabilities without a task-specific requirement.

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
