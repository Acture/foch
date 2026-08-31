# `foch.toml` `[[resolutions]]` DSL

`[[resolutions]]` records reviewed structural-conflict policy for `foch merge`.
The authoritative schema, validation, lookup map, and match parser live in
`src/project/mod.rs`; runtime handlers live under `src/merge/resolution/`.

Use narrow rules for conflicts you understand. A broad rule can deliberately
turn large parts of a playset into load-order behavior.

## Resolution chain

For each structural conflict, analysis tries:

1. an exact or pattern `[[resolutions]]` lookup;
2. a unique winner produced by mod `priority_boost` metadata;
3. a unique downstream contributor implied by declared dependencies; and
4. defer for interactive review or the final unresolved report.

The chain advances only for an unrecorded defer. An explicit `handler =
"defer"` records the choice and stops the chain.

## Selectors

Every entry sets exactly one selector.

| Selector | Scope | Valid actions |
| --- | --- | --- |
| `file` | Exact normalized merge target path | `prefer_mod`, `use_file`, `keep_existing`, `policy` |
| `conflict_id` | One exact conflict and candidate sequence | `prefer_mod`, `prefer_candidate`, `use_file` |
| `mod` | Contributor-wide priority metadata | `priority_boost` |
| `match` | File glob/regex and optional leaf-address glob/regex | `prefer_mod`, `use_file`, `handler`, file-only `policy` |

Paths are output-relative and normally use `/`. Matching normalizes `\` to `/`.

```toml
[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "1234567890"

[[resolutions]]
conflict_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
prefer_candidate = 2

[[resolutions]]
mod = "balance_patch"
priority_boost = 50

[[resolutions]]
match = "history/**"
handler = "last_writer"
```

## Actions

Every entry also sets exactly one action.

| Action | Contract |
| --- | --- |
| `prefer_mod = "id"` | Select that mod only when it is one unique current candidate. |
| `prefer_candidate = N` | Select the one-based candidate bound to an exact full `conflict_id`; stale/out-of-range choices defer. |
| `use_file = "path"` | Read the external file during analysis and freeze its bytes into the reviewed artifact tree. |
| `keep_existing = true` | For an exact file selector, retain bytes from the current output target and guard them against drift before commit. |
| `priority_boost = N` | Adjust one contributor's priority; valid only with `mod`. |
| `handler = "name"` | Dispatch a named built-in handler; valid only with `match`. |
| `policy = "cwt_suggested"` | Ask for CWT-derived merge-key discovery before conflicts are computed; valid only for `file` or a file-only `match`. |

Actions cannot be combined. `keep_existing = false` is invalid. Unknown handler
names load but warn and defer during analysis.

Examples:

```toml
[[resolutions]]
file = "events/PirateEvents.txt"
use_file = "resolutions/PirateEvents.txt"

[[resolutions]]
file = "common/defines/00_graphics.txt"
keep_existing = true

[[resolutions]]
match = "common/ideas/**::xx_idea_*"
handler = "last_writer"

[[resolutions]]
file = "common/estates_preload/test_modifiers.txt"
policy = "cwt_suggested"
```

An external file selected interactively is read during that analysis. If the
choice is persisted, the next run treats it as configured `use_file`. Commit
never rereads the external path; it installs the bytes already reviewed.

> `cwt_suggested` is parsed and validated, but its merge-time application is
> still tracked separately. Do not assume the rule changes output until the
> owning implementation and tests say so.

## Match syntax

```text
match-dsl   = file-side [ "::" [ address-side ] ]
file-side   = glob | "re:" regex
address-side = glob | "re:" regex
```

- no `::`, or a trailing empty address side, matches every conflict leaf in a
  matching file;
- `file::address` requires both sides to match;
- the canonical address is the nested path plus key joined with `/`;
- the file side may not be empty: use `**::xx_*`, not `::xx_*`;
- TOML basic strings double regex backslashes, while literal strings do not;
- pattern rules use declaration order and stop at the first match.

```toml
[[resolutions]]
match = "common/ideas/**"
handler = "last_writer"

[[resolutions]]
match = "**::xx_idea_*"
prefer_mod = "ideas_mod"

[[resolutions]]
match = 're:^events/.*\.txt$::re:^flavor_[a-z]+/[0-9]+'
handler = "defer"
```

## Built-in handlers

Handler names are case-insensitive.

| Handler | Behavior |
| --- | --- |
| `last_writer` | Pick the largest `(precedence, mod_id, candidate index)` tuple; defer if there are no candidates. |
| `defer` | Record an explicit defer and stop the chain. The review disposition is `deferred`. |
| `keep_existing` | Keep matching prior-output bytes when present; otherwise warn and continue with normal output. |

Use `last_writer` only where verified load-order semantics are acceptable. A
global rule is intentionally broad:

```toml
[[resolutions]]
match = "**"
handler = "last_writer"
```

`examples/eu4-default-foch.toml` contains historical narrow examples from one
playset. It is not a universal default; review every rule against current input.

## Conflict IDs

Production semantic-tree conflicts use a full 64-hex BLAKE3 identity that binds
the normalized target path, structural conflict, and complete candidate
sequence. Moving a file or changing candidates changes the ID. Persist the
complete value exactly as shown; never shorten it to eight characters.

The experimental address-patch backend retains an 8-hex address identity for
comparative tests. Lookup only applies that legacy fallback to a verified
address-derived view; it cannot replay an old candidate choice against a
semantic-tree conflict.

Find current IDs in the terminal conflict view or
`.foch/foch-merge-report.json` at
`conflict_resolutions[].leaf_conflicts[].conflict_id`.

## Lookup precedence

Conflict lookup order is:

1. exact current `conflict_id`;
2. verified address-patch fallback, only for that backend;
3. exact `file`;
4. first matching `match` rule in declaration order.

Avoid duplicate exact `file` or `conflict_id` keys. Exact maps are deterministic
but a later duplicate replaces the earlier entry; pattern rules retain their
declaration order.

## Terminal review

When static policy, priority, and dependency implication all defer, `foch
merge` may install the ratatui handler only when stdin/stdout are TTYs and
`--non-interactive` is absent. `--cli-prompt` selects the simple prompt.
Without a suitable TTY, the conflict remains a `needs_user_choice` review unit.

The TUI adapter is `apps/foch-cli/src/tui/conflict_handler.rs`.

| Key | Action |
| --- | --- |
| `↑` / `↓`, `Home` / `End` | Move selection |
| `Enter` | Confirm selected candidate |
| `Esc`, `d`, `D` | Defer |
| `q`, `Q` | Abort analysis |
| `s`, `S` | Select an external file |
| `k`, `K` | Keep existing output file |
| `1` … `9` | Pick visible candidate number |

Persisted terminal decisions use the same `[[resolutions]]` schema.

## Troubleshooting

| Error or symptom | Fix |
| --- | --- |
| `exactly one selector ... must be set` | Keep one of `file`, `conflict_id`, `mod`, or `match`. |
| `exactly one action ... must be set` | Keep one valid action; a `mod` selector requires only `priority_boost`. |
| `handler action requires match selector` | Use `match`, or replace the handler with an action valid for the exact selector. |
| `keep_existing action requires file selector` | Use exact `file`, or use pattern `handler = "keep_existing"`. |
| `keep_existing must be true when set` | Remove it or set `true`. |
| `priority_boost requires mod selector` | Change the selector to `mod`. |
| `match pattern file side cannot be empty` | Use a non-empty file side such as `**`. |
| `regex pattern side cannot be empty after re: prefix` | Supply a regex or remove `re:`. |
| `unknown merge handler` | Use `last_writer`, `defer`, or `keep_existing`. |
| A saved candidate no longer resolves | Inspect the current candidate sequence and replace/remove the stale exact rule. |
| `keep_existing_failed` | Ensure the target file exists, use `use_file`, or remove the keep-existing rule. |

Schema errors originate in `src/project/mod.rs`; runtime decision behavior is
implemented in `src/merge/resolution/`, and materialization behavior is in
`src/merge/output/materialize/`.
