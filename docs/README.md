# Foch Documentation

Public documentation for using, building and contributing to Foch. No private
notes access is required. Current product contracts live here; private research,
experimental interpretation and historical design discussions live in the
separate notes repository. Linear owns active work and dependencies.

## Users

- [Project manifest](foch-project-manifest.md) — compose ordered inputs in `foch.toml`.
- [Resolution reference](foch-toml-resolutions.md) — review and resolve conflicts.
- [VS Code/LSP preview](lsp-0.1-preview.md) — editor setup and supported behavior.
- [Known issues](../KNOWN_ISSUES.md) — public limitations.

## Contributors

- [Architecture](architecture.md) — source layout and dependency boundaries.
- [Merge contract](merge-design.md) — analyze, review and commit behavior.
- [Cache architecture](cache-architecture.md) — identities and lifecycle.
- [Development commands](../README.md#development) — build and quality checks.
- [Release checklist](RELEASE_CHECKLIST.md) — packaging and release gates.

## Status and evidence

- [Project status](project-status.md) — measured checkpoints and their limits.
- [Merge-quality acceptance](merge-quality-dataset.md) — inputs, scoring and evidence contracts.
- [Static-modifier verification](static-modifiers-verification.md) — focused product evidence.
- [Historical AST-diff results](merge-quality-ast-diff.md) — retained historical measurements.
- [Raw evidence](evidence/) — recorded artifacts, including historical applicability observations.

The append-only Workshop measurement streams stay beside their runner in
`src/apps/foch-cli/tests/merge_quality/data/`. Research notes reference these
records; moving directories must not change their bytes or acceptance scope.
