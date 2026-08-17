# Foch Documentation

This directory is the contributor map for Foch's current product state,
architecture, and implementation contracts.

If you are new to the project, start with the current handoff below. It is
self-contained. Notion page **foch — Merge Corpus & Game Semantics** supplements
it with live ownership when access is available.

## Start Here

- [project-status.md](./project-status.md) — current goal, accepted evidence,
  dirty-worktree warning, active tasks, and fresh-agent runbook
- [architecture.md](./architecture.md) — package and execution boundaries,
  including the current prepare/export gap
- [merge-design.md](./merge-design.md) — public merge artifacts, conflict policy,
  and output contract
- [merge-quality-dataset.md](./merge-quality-dataset.md) — fixed 14-case product
  acceptance, input identity, evidence, and scoring contract

## User and Contributor Reference

- [foch-workspace-manifest.md](./foch-workspace-manifest.md) — declarative
  workspace composition
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

- [auto-merge-roadmap.md](./auto-merge-roadmap.md) — superseded milestone plan
  for the first merge vertical slice
- [structured-merge-shadow.md](./structured-merge-shadow.md) — historical
  GumTree/PCS rollout and Legacy/Structured comparison
- [common-applicability-probe.md](./common-applicability-probe.md) — auxiliary
  `common/<folder>` analysis, not product acceptance
- [research/2026-07-22-common-applicability.md](./research/2026-07-22-common-applicability.md)
  — fixed research result and failure taxonomy
