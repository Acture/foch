# Foch Documentation

This directory is the contributor map for Foch's current product state,
architecture, and implementation contracts.

If you are new to the project, start with the current handoff below. It is
self-contained. Linear owns active execution and dependencies; Notion owns the
project narrative and research record. Neither is duplicated here.

## Start Here

- [project-status.md](./project-status.md) — verified state, accepted evidence,
  dirty-worktree warning, and fresh-agent runbook
- [architecture.md](./architecture.md) — package and execution boundaries,
  including the analyze/review/commit flow
- [merge-design.md](./merge-design.md) — review units, conflict policy, and
  commit contract
- [merge-quality-dataset.md](./merge-quality-dataset.md) — fixed 14-case product
  acceptance, input identity, evidence, and scoring contract

## User and Contributor Reference

- [foch-project-manifest.md](./foch-project-manifest.md) — declarative project
  input composition
- [foch-toml-resolutions.md](./foch-toml-resolutions.md) — reviewed conflict
  resolutions and safety rules
- [cache-architecture.md](./cache-architecture.md) — cache layers, identity, and
  trust boundaries
- [lsp-0.1-preview.md](./lsp-0.1-preview.md) — independently versioned VS
  Code/LSP preview and editor scope
- [RELEASE_CHECKLIST.md](./RELEASE_CHECKLIST.md) — release gates

## Historical and Auxiliary Evidence

These documents explain how earlier decisions were reached. They are not the
active task queue and do not replace the current product acceptance gate.

- [structured-merge-shadow.md](./structured-merge-shadow.md) — historical
  GumTree/PCS rollout and Legacy/Structured comparison
- [common-applicability-probe.md](./common-applicability-probe.md) — auxiliary
  `common/<folder>` analysis, not product acceptance

Files below `reviews/`, `research/`, and `superpowers/specs/` are retained
historical evidence. Their old crate names and commands are not current
architecture.
