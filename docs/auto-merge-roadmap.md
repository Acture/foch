# Auto-Merge Roadmap (Historical)

Original checkpoint: 2026-03-25

Archived as active planning: 2026-08-17

> This page records the first merge vertical slice. It is not a current
> roadmap, backlog, release claim, or source of next steps. Use
> [project-status.md](./project-status.md) for the verified repository state and
> Notion page **foch — Merge Corpus & Game Semantics** for live work.

## Historical Scope

The March checkpoint described the transition from analysis-only behavior to a
merge-capable EU4 tool. The planned sequence was:

1. deterministic merge planning;
2. an intermediate representation for supported structural content;
3. deterministic Clausewitz emission;
4. materialization of a merged mod and its audit sidecars; and
5. post-merge validation with explicit failure status.

At that checkpoint, the intended boundaries were deliberately narrow:

- EU4 only;
- structural merging only for roots supported by semantic indexing;
- no structural rewrite of unknown or binary formats;
- conservative treatment of UI files; and
- localisation compatibility tracked separately from the core merge sequence.

## Recorded Outcome

The repository subsequently gained deterministic `merge-plan`, supported
structural merge and emission, merged-output materialization, post-merge
revalidation, and the `graph` and `simplify` commands. Detailed command and
artifact contracts are documented in [merge-design.md](./merge-design.md).

Those milestones explain how the current implementation was reached. They do
not establish present release readiness or assign follow-up work.
