# P-830 implementation plan

**Goal:** Show recorded adopted Mod sources beside schema help through shared
EU4 editor behavior and `foch lsp`. Enrich the opt-in merge sidecar with exact
byte fingerprints and names, and append sources to supported game GUI tooltips.

**Architecture:** A shared source-position helper converts parser byte offsets
and UTF-16 positions. A shared hover module reads bounded sidecar metadata,
matches exact definitions and composes schema help. LSP owns document lifecycle.

**Tech stack:** Rust, existing parser/content-family APIs, serde_json, tower-lsp.

- [x] Add focused failing shared-editor regressions for provenance lookup and
  schema composition; run `cargo test -p foch --lib provenance_hover`.
- [x] Implement `src/game/eu4/editor/hover.rs`, source positions and the small
  schema-hover adapter changes; verify the mapping/fallback matrix.
- [x] Add failing real-handler LSP regressions for sources plus schema, UTF-16
  incremental edits, dirty/saved documents and close. Implement the thin adapter
  and document updates in `apps/foch-cli/src/lsp.rs`.
- [x] Verify a real merge-generated sidecar and actual LSP request flow, including
  that hover does not alter the script or metadata.
- [x] Implement the user's added GUI scope: source-backed widget allowlist,
  append original tooltip content, project actual widget lineage, and reuse
  surviving-script localisation emission. Cover this via a real merge fixture.
- [x] Preserve historical GUI fixtures and document installed field evidence,
  supported behavior, and remaining manual runtime verification.
- [x] Run focused gates, root/CLI package tests, formatting and strict workspace
  Clippy where the environment supports them. Review requirements and code,
  resolve findings, and update `docs/project-status.md` with exact evidence.

Do not run the full Workshop cohort; hand off `cargo acceptance` to the maintainer.

Final results and environment-limited gates are recorded in
[`docs/project-status.md`](../../project-status.md#editor-and-gui-merge-provenance-2026-10-01).
