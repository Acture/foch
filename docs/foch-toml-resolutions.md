# foch.toml `[[resolutions]]` DSL

This guide documents the `foch.toml` `[[resolutions]]` entries used by `foch merge` to resolve structural merge conflicts declaratively. The authoritative schema, lookup map, validation, match DSL parser, and legacy address conflict-id hash live in `crates/foch-core/src/config.rs`; the built-in handler registry is in `crates/foch-engine/src/merge/resolution/handler_registry.rs`.

## 1. Overview

When the patch merge engine reaches a structural conflict, it asks a conflict-handler chain for a decision. The merge materializer builds that chain as:

1. `LookupHandler` — checks the parsed `foch.toml` `ResolutionMap` for a matching `[[resolutions]]` entry.
2. `PriorityBoostResolutionHandler` — if lookup defers, applies mod-scoped priority boosts when they produce one unique highest-precedence candidate.
3. `DepImpliesResolutionHandler` — if priority resolution also defers, tries to pick the downstream mod when declared dependencies imply a single winner.
4. `DeferHandler` — if all previous stages defer, leaves the conflict unresolved for post-pass interactive prompting or the final manual-conflict report.

That order is constructed in `crates/foch-engine/src/merge/output/materialize/structural.rs`. The handlers and `ChainHandler` are implemented in `crates/foch-engine/src/merge/resolution/conflict_handler.rs`; the chain advances only for an unrecorded `ConflictDecision::Defer`. `LookupHandler` reads the view's exact conflict id and canonical leaf address, then dispatches `prefer_mod`, `prefer_candidate`, `use_file`, `keep_existing`, or named `handler` decisions.

`[[resolutions]]` is therefore a local policy layer for conflicts you already understand: it can pin one exact conflict by id, apply a whole-file policy, apply a path/address pattern, adjust mod priority metadata, or opt a structural root into `policy = "cwt_suggested"` merge-key discovery. The end-to-end fixture uses:

```toml
[[resolutions]]
match = "history/**"
handler = "last_writer"
```

and verifies that `last_writer` resolves the two-mod EU4 history conflict without manual conflicts (`crates/foch-engine/tests/fixtures/playsets/eu4_two_mod_conflict_resolved/foch.toml`, `crates/foch-engine/tests/merge_e2e.rs`).

## 2. Selectors

Every `[[resolutions]]` entry must set exactly one selector: `file`, `conflict_id`, `mod`, or `match`. The validator rejects missing or multiple selectors with `exactly one selector (file, conflict_id, mod, match) must be set` (`crates/foch-core/src/config.rs`).

| Selector | Type | Scope | Typical actions | Notes |
| --- | --- | --- | --- | --- |
| `file` | TOML string parsed as `PathBuf` | Exact merge target path, for example `events/PirateEvents.txt` | `prefer_mod`, `use_file`, `keep_existing`, `policy` | Stored in `ResolutionMap.by_file`; lookup checks it after `conflict_id` and before patterns. File-scoped `policy = "cwt_suggested"` entries are stored separately in `ResolutionMap.policy_by_file`. |
| `conflict_id` | String | One exact conflict identity | `prefer_mod`, `prefer_candidate`, `use_file` | Stored in `ResolutionMap.by_conflict_id`; highest lookup precedence. Semantic-tree ids are file-scoped 64-hex full ids; address-patch ids are 8 hex. `policy` is not valid with this selector because merge-key overrides apply before per-conflict dispatch. |
| `mod` | String mod id | A mod-scoped priority entry | `priority_boost` only | Stored in `ResolutionMap.mod_priority_boost`, not returned by per-conflict `lookup`. |
| `match` | String pattern DSL | File glob/regex, optionally plus address glob/regex | `prefer_mod`, `use_file`, `handler`, `policy` | Decision rules compile into ordered `pattern_rules`. `policy = "cwt_suggested"` is only valid for file-only `match` patterns (no address side) because it overrides the file-level merge key before conflicts are computed. |

Examples:

```toml
# Exact file selector: every conflict in this target file prefers one mod.
[[resolutions]]
file = "events/PirateEvents.txt"
prefer_mod = "1234567890"
```

```toml
# Exact conflict selector: one leaf conflict, independent of broader path rules.
[[resolutions]]
conflict_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
prefer_mod = "9876543210"
```

```toml
# Mod selector: mod-scoped priority metadata.
[[resolutions]]
mod = "balance_patch"
priority_boost = 50
```

```toml
# Match selector: all conflicts under history/ route through a named handler.
[[resolutions]]
match = "history/**"
handler = "last_writer"
```

Paths should be written as foch merge target paths, usually relative paths with `/` separators. Pattern matching normalizes `\` to `/` before matching (`crates/foch-core/src/config.rs`).

## 3. Actions

Every entry must set exactly one action. For non-`mod` selectors, the allowed action fields are `prefer_mod`, `prefer_candidate`, `use_file`, `keep_existing`, `handler`, and `policy`; for the `mod` selector, the only action is `priority_boost`. Validation is centralized in `ResolutionEntry::validate`, and conflict actions are converted into `ResolutionDecision` while merge-key overrides become `ResolutionPolicy` entries.

| Action | Type | Valid selectors | Runtime decision | Validation rules |
| --- | --- | --- | --- | --- |
| `prefer_mod` | String mod id | `file`, `conflict_id`, `match` | `ResolutionDecision::PreferMod`; lookup selects the unique matching candidate | Cannot combine with any other action (`crates/foch-core/src/config.rs`). If the mod is no longer a unique contributor at that conflict, lookup defers (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`). |
| `prefer_candidate` | Positive one-based integer | `conflict_id` only | Picks the corresponding zero-based candidate internally | Use only with the complete exact id shown by foch. The exact semantic id binds the candidate sequence; stale or out-of-range choices defer. |
| `use_file` | TOML string parsed as `PathBuf` | `file`, `conflict_id`, `match` | Configured bytes are frozen during preparation and dispatched as `UseFrozenFile` | Cannot combine with any other action. Preparation reads the source before export (`crates/foch-engine/src/merge/execute.rs`); materialization writes those frozen bytes rather than rereading the path (`crates/foch-engine/src/merge/output/materialize/io.rs`). |
| `keep_existing` | Boolean | `file` only | `KeepExisting`; preserve an existing output-dir file | Must be `true` when present, and requires `file` selector (`crates/foch-core/src/config.rs`). If the output file does not exist, foch warns and falls through to normal output (`crates/foch-engine/src/merge/output/materialize/io.rs`). |
| `priority_boost` | Integer (`i32`) | `mod` only | Stored in `ResolutionMap.mod_priority_boost` | `mod` requires `priority_boost`; `priority_boost` cannot combine with `prefer_mod`, `use_file`, `keep_existing`, `handler`, or `policy`; `priority_boost` without `mod` is invalid. |
| `handler` | String handler name | `match` only | Registry dispatches to a built-in handler | Requires `match` selector and cannot combine with any other action. Unknown names parse successfully but log a runtime warning and defer. |
| `policy` | String enum | `file`, file-only `match` | Overrides pre-conflict merge-key discovery | `policy = "cwt_suggested"` asks materialization to derive the file's `MergeKeySource` from CWTools schema metadata. Missing or ambiguous schema hints are hard validation errors; address-constrained `match` patterns are rejected because the override applies before per-leaf conflict dispatch. |

Interactive external-file choices are deliberately different: the choice made
after export starts becomes `UseLiveFile` for that run and is read at
materialization time. If persisted to `foch.toml`, it is a configured `use_file`
on the next run and its bytes are then frozen during preparation.

Examples:

```toml
# prefer_mod: choose a contributor by mod id.
[[resolutions]]
conflict_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
prefer_mod = "1234567890"
```

```toml
# use_file: replace the merge target with a maintained resolution file.
[[resolutions]]
file = "events/PirateEvents.txt"
use_file = "resolutions/PirateEvents.txt"
```

```toml
# keep_existing action: only valid with an exact file selector.
[[resolutions]]
file = "common/defines/00_graphics.txt"
keep_existing = true
```

```toml
# priority_boost: only valid with a mod selector.
[[resolutions]]
mod = "late_patch_mod"
priority_boost = 100
```

```toml
# handler: only valid with a match selector.
[[resolutions]]
match = "common/ideas/**::xx_idea_*"
handler = "last_writer"
```

```toml
# policy: opt a file into CWT-derived merge-key discovery.
[[resolutions]]
file = "common/estates_preload/test_modifiers.txt"
policy = "cwt_suggested"
```

> **Note:** The `cwt_suggested` policy is parsed and validated but merge-time application is not yet
> implemented (tracked in [#42](https://github.com/Acture/foch/issues/42)). Adding this entry today
> will pass validation but have no effect on merge output until #42 is resolved.

## 4. Match DSL syntax

The `match` selector uses this DSL:

```text
match-dsl  = file-side [ "::" [ address-side ] ]
file-side  = glob-side | regex-side
address-side = glob-side | regex-side
glob-side  = non-empty globset pattern, for example "common/ideas/**" or "**"
regex-side = "re:" non-empty Rust regex, for example "re:^events/.*\\.txt$"
```

`parse_match_dsl` trims the whole input, splits once on `::`, requires a non-empty file side, treats an omitted or empty address side as no address constraint, and compiles each side independently (`crates/foch-core/src/config.rs`). A side without `re:` is a glob compiled by `globset`; a side with `re:` is a `regex::Regex`.

Important semantics:

- **File-only match**: no `::`, or a trailing empty address side, matches every conflict leaf in matching files.
- **Address-constrained match**: `file::address` only matches when both sides match. If the caller has no leaf address, address-constrained rules do not match (`crates/foch-core/src/config.rs`).
- **Canonical address shape**: `LookupHandler` builds the leaf address as `address.path.join("/") + "/" + address.key`, or just `address.key` when the path is empty (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`).
- **Use `**` for global file scope**: `::xx_*` is invalid because the file side is empty; write `**::xx_*` instead (`crates/foch-core/src/config.rs`).
- **TOML escaping**: in TOML basic strings, regex backslashes must be doubled (`"re:^events/.*\\.txt$"`). TOML literal strings can keep regexes closer to source form (`'re:^events/.*\.txt$'`).

Glob examples:

```toml
# Every leaf in files under common/ideas/.
[[resolutions]]
match = "common/ideas/**"
handler = "last_writer"

# Every file and every leaf.
[[resolutions]]
match = "**"
handler = "defer"

# Any file, but only address leaves beginning with xx_idea_.
[[resolutions]]
match = "**::xx_idea_*"
prefer_mod = "ideas_mod"
```

Regex examples:

```toml
# Regex file side, no address constraint.
[[resolutions]]
match = 're:^events/.*\.txt$'
handler = "last_writer"

# Regex on both sides.
[[resolutions]]
match = 're:^events/.*\.txt$::re:^flavor_[a-z]+\.[0-9]+/option/.+'
handler = "last_writer"
```

Mixed-side examples:

```toml
# Glob file side, regex address side.
[[resolutions]]
match = 'common/**::re:^test\..*'
handler = "defer"

# Regex file side, glob address side.
[[resolutions]]
match = 're:^history/.*\.txt$::*religion*'
prefer_mod = "history_patch"

# Empty address side means the same as file-only.
[[resolutions]]
match = "events/**::"
handler = "last_writer"
```

## 5. Built-in handlers

A `handler` action names a built-in handler from the registry. Dispatch is case-insensitive (`crates/foch-engine/src/merge/resolution/handler_registry.rs`).

| Handler | Decision | Behavior | Source |
| --- | --- | --- | --- |
| `last_writer` | `PickCandidate { record: Some(...) }` | Chooses the candidate with the largest `(precedence, mod_id, candidate index)` tuple for deterministic output. If there are no candidates, it defers. | `crates/foch-engine/src/merge/resolution/handler_registry.rs` |
| `defer` | `Defer { record: Some(...) }` | Records a handler-attributed manual conflict and stops the handler chain; it does not fall through to `PriorityBoost` or `DepImplies`. | `crates/foch-engine/src/merge/resolution/handler_registry.rs` |
| `keep_existing` | `KeepExisting` | Marks matching paths to keep the current output-dir file. If the target file exists, materialization records `kept_existing`; if it does not, foch warns and writes normal output. | `crates/foch-engine/src/merge/resolution/handler_registry.rs`, `crates/foch-engine/src/merge/output/materialize/io.rs` |

Examples:

```toml
# last_writer: broad policy for a known safe root.
[[resolutions]]
match = "history/**"
handler = "last_writer"
```

```toml
# defer: deliberately leave this path to dependency logic or the interactive resolver.
[[resolutions]]
match = "events/experimental/**"
handler = "defer"
```

```toml
# keep_existing handler: pattern-scoped keep-existing behavior.
[[resolutions]]
match = "gfx/**"
handler = "keep_existing"
```

## 6. Conflict ID stability

Foch exposes two conflict-id forms, one per merge kernel:

- **Semantic-tree conflicts (production):** a 64-hex full BLAKE3 digest. The digest binds a domain tag, the merge target path normalized from `\` to `/`, a NUL separator, and the raw 32-byte kernel `ConflictNodeId`. The raw kernel id captures the structural conflict and complete candidate sequence but intentionally excludes the file path. Binding both values makes the public id unique across target files while keeping two different candidate sequences at the same file/address distinct. Raw `ConflictNodeId` values remain internal keys when replaying kernel resolutions.
- **Address-patch conflicts (reference kernel):** the legacy 8-hex prefix returned by `compute_conflict_id`. Its inputs are the slash-normalized target path, a NUL separator, the canonical address path, another NUL separator, and the address key.

Moving or renaming a target file changes either public id. For semantic conflicts, changing the structural conflict or candidate sequence also changes the id even when the displayed leaf address stays the same. Do not shorten a semantic id to eight characters: persist the complete id exactly as shown by foch.

`ConflictView.conflict_id`, semantic `resolved_conflict_ids`, persisted interactive choices, and report leaf details all use the same file-scoped semantic id. `LookupHandler` only retargets an 8-hex address id when the view itself is verified as address-derived; an address-era entry is never applied to a semantic candidate sequence.

To find a conflict id:

- In the simple interactive prompt, foch prints a conflict summary containing `conflict_id: ...` before the choice prompt (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`).
- If you choose a candidate or external file interactively, foch appends a `[[resolutions]]` entry containing that id to the configured `foch.toml` (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`).
- The generated `.foch/foch-merge-report.json` exposes unresolved ids at `conflict_resolutions[].leaf_conflicts[].conflict_id`. Copy the complete value from the report or prompt into `foch.toml`; semantic ids cannot be recomputed from the displayed address alone.

## 7. Lookup precedence

`LookupHandler` uses this precedence order:

1. Exact `ConflictView.conflict_id` match in `by_conflict_id`.
2. For a verified address-patch view only, the 8-hex address id recomputed against the real merge target. This preserves reference-kernel calls whose view initially carried a synthetic address path instead of the target file.
3. Exact `file` match in `by_file`.
4. First matching `pattern_rules` entry in declaration order.

The address fallback is deliberately gated. A missing 64-hex semantic id proceeds directly to file and pattern lookup; it cannot match an 8-hex address entry. This prevents an old `prefer_candidate` choice from being replayed against a different semantic candidate sequence.

Worked example:

```toml
# Layer 4, declaration order position 1: global resolution policy.
[[resolutions]]
match = "**"
handler = "last_writer"

# Layer 4, declaration order position 2: never reached for common/ideas/**
# if the previous ** rule already matched.
[[resolutions]]
match = "common/ideas/**"
handler = "defer"

# Layer 3: exact file beats both pattern rules.
[[resolutions]]
file = "common/ideas/national_ideas.txt"
prefer_mod = "file_policy_mod"

# Layer 1: exact view conflict id beats address, file, and pattern rules.
[[resolutions]]
conflict_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
prefer_mod = "specific_conflict_mod"
```

For a conflict in `common/ideas/national_ideas.txt` with id `0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef`, foch picks `specific_conflict_mod` because `conflict_id` wins. For another conflict in the same file with a different id, foch picks `file_policy_mod` because `file` wins over patterns. For a conflict in `events/foo.txt`, foch uses the first pattern rule (`match = "**"`) because no id or file entry matches. If two pattern rules can both match, the earlier declaration wins; tests cover that first-match behavior (`crates/foch-core/src/config.rs`).

Avoid duplicate exact `file` or `conflict_id` keys. The map uses
`BTreeMap::insert` for deterministic key iteration; a later duplicate exact key
still overwrites the earlier one during construction (`crates/foch-core/src/config.rs`).
Pattern rules, by contrast, keep declaration order.

## 8. Common templates

### Global last writer

Use this only when you have decided that load-order semantics are acceptable for every structural conflict that reaches lookup. It is intentionally broad — and on a real EU4 playset it routes ~1700 handler decisions through `last_writer`, overriding `downstream_override` on the ~1600 conflicts the engine could resolve from dependency order alone. Prefer the narrow per-path template below in `examples/eu4-default-foch.toml` unless you genuinely want this behavior.

```toml
[[resolutions]]
match = "**"
handler = "last_writer"
```

### Historical narrow-rule example

`examples/eu4-default-foch.toml` preserves nine narrow `last_writer` rules from
one historical playset probe. It is a DSL example, not a current default or a
claim that those files conflict in another playset. Review every path against
your own merge report before copying a rule.

### Per-path policy

Combine exact file rules with narrower pattern rules. The exact file rule wins for `events/PirateEvents.txt`; the idea pattern applies elsewhere under `common/ideas/**`.

```toml
[[resolutions]]
file = "events/PirateEvents.txt"
use_file = "resolutions/PirateEvents.txt"

[[resolutions]]
match = "common/ideas/**::xx_idea_*"
prefer_mod = "ideas_expansion"

[[resolutions]]
match = "history/**"
handler = "last_writer"
```

### Specific conflict resolved by mod pick

Use this for a reviewed conflict where only one leaf should be pinned.

```toml
[[resolutions]]
conflict_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
prefer_mod = "1234567890"
```

## 9. TUI integration

If no `foch.toml` entry, priority boost, or dependency implication resolves a conflict, it can survive into the post-pass interactive resolver. In auto mode, `foch merge` installs the ratatui handler only when stdin and stdout are TTYs and `--non-interactive`/`--no-interactive` is not set. Without a suitable TTY it installs no interactive handler, so the conflict remains deferred; it does not switch to the simple prompt. `--cli-prompt` explicitly selects the simple stdin/stderr handler (`crates/foch-cli/src/cli/handler/merge.rs`, `crates/foch-cli/src/cli/arg.rs`).

The ratatui resolver is implemented in `crates/foch-cli/src/tui/conflict_handler.rs`. Its defensive TTY check returns `Defer` if it is invoked without TTY stdin/stdout. Its keybindings are:

| Key | Action | Source |
| --- | --- | --- |
| `↑` / `↓` | Move selection | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `Home` / `End` | Jump to first/last item | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `Enter` | Confirm selected item | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `Esc`, `d`, `D` | Defer | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `q`, `Q` | Abort merge | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `s`, `S` | Use an external file path; the dialog accepts `Enter` to confirm and `Esc` to cancel | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `k`, `K` | Keep existing output file | `crates/foch-cli/src/tui/conflict_handler.rs` |
| `1` ... `9` | Pick that candidate by visible candidate number | `crates/foch-cli/src/tui/conflict_handler.rs` |

The footer displayed by the TUI summarizes the main bindings as `↑↓ select  Enter confirm  Esc/d defer  Q abort  S file  K keep`. Picked decisions are persisted back to `foch.toml` through the same `resolution_entry_for_decision` path used by the simple prompt (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`).

## 10. Troubleshooting

Most schema errors surface while loading `foch.toml`: `FochConfig` validates `ResolutionMap::from_entries` during deserialize, and load failures are reported as `failed to parse foch config <path>: ...` (`crates/foch-core/src/config.rs`).

| Error or symptom | Cause | Fix |
| --- | --- | --- |
| `invalid [[resolutions]] entry N: exactly one selector ... must be set` | Missing selector, or more than one of `file`, `conflict_id`, `mod`, `match`. | Split the policy into separate entries or keep only the intended selector (`crates/foch-core/src/config.rs`). |
| `invalid [[resolutions]] entry N: exactly one action ... must be set` | Missing action, or multiple actions such as `prefer_mod` plus `use_file`. | Keep exactly one action. For `mod`, the action must be `priority_boost` (`crates/foch-core/src/config.rs`). |
| `handler action requires match selector` | `handler` was used with `file` or `conflict_id`. | Replace the selector with `match = "..."`, or use `prefer_mod`/`use_file`/`keep_existing` for exact selectors (`crates/foch-core/src/config.rs`). |
| `keep_existing action requires file selector` | `keep_existing = true` was used with `conflict_id` or `match`. | Use `file = "..."` with `keep_existing = true`, or use `handler = "keep_existing"` with a `match` selector for pattern-scoped behavior (`crates/foch-core/src/config.rs`). |
| `keep_existing must be true when set` | TOML set `keep_existing = false`. | Remove the action or set `keep_existing = true` (`crates/foch-core/src/config.rs`). |
| `mod selector requires priority_boost action` | `mod = "..."` was paired with `prefer_mod`/`use_file`/`handler`, or no action. | Use `priority_boost = <integer>` and no other action (`crates/foch-core/src/config.rs`). |
| `priority_boost requires mod selector` | `priority_boost` was used with `file`, `conflict_id`, or `match`. | Change the selector to `mod`, or use a conflict action appropriate for the selector (`crates/foch-core/src/config.rs`). |
| `match pattern cannot be empty` or `match pattern file side cannot be empty` | `match = ""`, whitespace-only match, or an address-only pattern like `::xx_*`. | Use a non-empty file side; for global file scope, write `**` or `**::xx_*` (`crates/foch-core/src/config.rs`). |
| `regex pattern side cannot be empty after re: prefix` | A side was just `re:`. | Add a regex after the prefix or remove `re:` to use a glob (`crates/foch-core/src/config.rs`). |
| `invalid regex ...` | Regex compilation failed, often because of an unterminated character class or TOML escaping mistake. | Validate the regex and remember to double backslashes in TOML basic strings, or use TOML literal strings (`crates/foch-core/src/config.rs`). |
| Runtime warning: `unknown merge handler ...; deferring conflict` | `handler` name parsed but did not match `last_writer`, `defer`, or `keep_existing`. | Fix the handler spelling; handler names are matched case-insensitively but are not schema-validated at load time (`crates/foch-engine/src/merge/resolution/handler_registry.rs`). |
| A `prefer_mod` or `prefer_candidate` entry no longer resolves | The selected mod is not a unique current candidate, or the saved candidate index is stale. | Re-run interactively, inspect the current candidates, and update or remove the stale `conflict_id` entry (`crates/foch-engine/src/merge/resolution/conflict_handler.rs`). |
| `keep_existing_failed: file does not exist in prior output` | A keep-existing action/handler matched, but the target file was absent in the prior output directory. | Seed the output file first, choose `use_file`, or remove the keep-existing rule (`crates/foch-engine/src/merge/output/materialize/io.rs`). |
