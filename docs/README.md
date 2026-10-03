# Foch Documentation

This directory holds Foch's public usage and acceptance contracts, measured
project state, numerical records, and raw evidence. Research and design documents
are maintained in the [project notes](../notes/foch/首页.md) submodule.

If you are new to the project, start with the current handoff below. It is
self-contained. Linear owns active execution and dependencies. Migrated research
and design live in `notes/foch/`; Notion-only material remains at its original
page until separately migrated. See the [notes workflow](../README.md#research-and-design-notes)
for initialization, updates, and committing notes before the parent gitlink.

## Start Here

- [project-status.md](./project-status.md) — verified state, accepted evidence,
  dirty-worktree warning, and fresh-agent runbook
- [architecture.md](../notes/foch/docs/architecture.md) — package and execution boundaries,
  including the analyze/review/commit flow
- [merge-design.md](../notes/foch/docs/merge-design.md) — review units, conflict policy, and
  commit contract
- [merge-quality-dataset.md](./merge-quality-dataset.md) — fixed 14-case product
  acceptance, input identity, evidence, and scoring contract

## User and Contributor Reference

- [foch-project-manifest.md](./foch-project-manifest.md) — declarative project
  input composition
- [foch-toml-resolutions.md](./foch-toml-resolutions.md) — reviewed conflict
  resolutions and safety rules
- [cache-architecture.md](../notes/foch/docs/cache-architecture.md) — cache layers, identity, and
  trust boundaries
- [lsp-0.1-preview.md](./lsp-0.1-preview.md) — independently versioned VS
  Code/LSP preview and editor scope
- [RELEASE_CHECKLIST.md](./RELEASE_CHECKLIST.md) — release gates

## Historical and Auxiliary Evidence

These documents explain how earlier decisions were reached. They are not the
active task queue and do not replace the current product acceptance gate.

- [structured-merge-shadow.md](../notes/foch/docs/structured-merge-shadow.md) — historical
  GumTree/PCS rollout and Legacy/Structured comparison
- [common-applicability-probe.md](./common-applicability-probe.md) — auxiliary
  `common/<folder>` analysis, not product acceptance

Historical reviews, research explanations, specs and plans now live under
`notes/foch/docs/` and `notes/foch/plan/`, with their history preserved. Their old
crate names and commands are not current architecture. Raw JSON evidence below
`docs/evidence/` and `docs/research/evidence/` remains in this repository.
