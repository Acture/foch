# Foch Documentation

Documentation for using, building and contributing to Foch. Current product
contracts live here. Linear owns active work and dependencies.

## Users

- [Project manifest](foch-project-manifest.md) — compose ordered inputs in `foch.toml`.
- [Resolution reference](foch-toml-resolutions.md) — review and resolve conflicts.
- [VS Code/LSP preview](lsp-0.1-preview.md) — editor setup and supported behavior.
- [Inline EU4 runtime tests](foch-runtime-tests.md) — annotations, commands and the runner contract.
- [DLL plugin launch](../src/packages/foch-eu4-host/README.md) — Windows x64 packages, isolated deployment and actual status.
- [Known issues](known-issues.md) — public limitations.

## Contributors

- [Architecture](architecture.md) — source layout and dependency boundaries.
- [Merge contract](merge-design.md) — analyze, review and commit behavior.
- [Cache architecture](cache-architecture.md) — identities and lifecycle.
- [Mod authoring product design](foch-authoring-design.md) — current decisions and future capabilities.
- [Test framework API design](foch-test-framework-design.md) — pytest/Rust-inspired marks, selection, sessions and reporting; partly implemented.
- [Development commands](../README.md#development) — build and quality checks.
- [Release checklist](RELEASE_CHECKLIST.md) — packaging and release gates.

## Status and acceptance

- [Project status](project-status.md) — current verified state and its limits.
- [Merge-quality acceptance](merge-quality-dataset.md) — inputs, scoring and evidence contracts.

The append-only Workshop measurement streams and raw probe evidence stay beside
their runner in `src/apps/foch-cli/tests/merge_quality/data/`. Git history and
linked Linear issues preserve past decisions and checkpoints. This directory
contains current documentation, not an archive.
