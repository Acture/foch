# foch.toml `[[resolutions]]` DSL

This guide documents the `foch.toml` `[[resolutions]]` entries used by `foch merge` to resolve structural merge conflicts declaratively. The authoritative schema lives in `crates/foch-core/src/config.rs:79-110`; the lookup map, validation, match DSL parser, and conflict-id hash are in `crates/foch-core/src/config.rs:113-435`; the built-in handler registry is in `crates/foch-engine/src/merge/handler_registry.rs:32-100`.

## 1. Overview

When the patch merge engine reaches a structural conflict, it asks a conflict-handler chain for a decision. The merge materializer builds that chain as:

1. `LookupHandler` — checks the parsed `foch.toml` `ResolutionMap` for a matching `[[resolutions]]` entry.
2. `DepImpliesResolutionHandler` — if lookup defers, tries to pick the downstream mod when declared dependencies imply a single winner.
3. `DeferHandler` — if both previous stages defer, leaves the conflict unresolved for post-pass interactive prompting or the final manual-conflict report.

That order is constructed in `crates/foch-engine/src/merge/materialize.rs:1001-1021`. `ChainHandler` only invokes the next handler when the previous handler returns `ConflictDecision::Defer` (`crates/foch-engine/src/merge/conflict_handler.rs:659-687`). `LookupHandler` computes the per-conflict id and canonical leaf address, then dispatches `prefer_mod`, `use_file`, `keep_existing`, or named `handler` decisions (`crates/foch-engine/src/merge/conflict_handler.rs:322-352`).

`[[resolutions]]` is therefore a local policy layer for conflicts you already understand: it can pin one exact conflict by id, apply a whole-file policy, apply a path/address pattern, or adjust mod priority metadata. The end-to-end fixture uses:

```toml
[[resolutions]]
match = "history/**"
handler = "last_writer"
```

and verifies that `last_writer` resolves the two-mod EU4 history conflict without manual conflicts (`crates/foch-engine/tests/fixtures/playsets/eu4_two_mod_conflict_resolved/foch.toml:4-6`, `crates/foch-engine/tests/merge_e2e.rs:351-419`).

## 2. Selectors

Every `[[resolutions]]` entry must set exactly one selector: `file`, `conflict_id`, `mod`, or `match`. The validator rejects missing or multiple selectors with `exactly one selector (file, conflict_id, mod, match) must be set` (`crates/foch-core/src/config.rs:335-342`).

| Selector | Type | Scope | Typical actions | Notes |
| --- | --- | --- | --- | --- |
| `file` | TOML string parsed as `PathBuf` | Exact merge target path, for example `events/PirateEvents.txt` | `prefer_mod`, `use_file`, `keep_existing` | Stored in `ResolutionMap.by_file`; lookup checks it after `conflict_id` and before patterns (`crates/foch-core/src/config.rs:285-289`, `crates/foch-core/src/config.rs:304-332`). |
| `conflict_id` | String | One canonical conflict address | `prefer_mod`, `use_file` | Stored in `ResolutionMap.by_conflict_id`; highest lookup precedence (`crates/foch-core/src/config.rs:288-289`, `crates/foch-core/src/config.rs:322-324`). |
| `mod` | String mod id | A mod-scoped priority entry | `priority_boost` only | Stored in `ResolutionMap.mod_priority_boost`, not returned by per-conflict `lookup` (`crates/foch-core/src/config.rs:276-282`). |
| `match` | String pattern DSL | File glob/regex, optionally plus address glob/regex | `prefer_mod`, `use_file`, `handler` | Compiled into ordered `pattern_rules`; handler actions are only valid with this selector (`crates/foch-core/src/config.rs:290-298`, `crates/foch-core/src/config.rs:370-374`). |

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
conflict_id = "ab12cd34"
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

Paths should be written as foch merge target paths, usually relative paths with `/` separators. Pattern matching normalizes `\` to `/` before matching (`crates/foch-core/src/config.rs:162-170`).

## 3. Actions

Every entry must set exactly one action. For non-`mod` selectors, the allowed action fields are `prefer_mod`, `use_file`, `keep_existing`, and `handler`; for the `mod` selector, the only action is `priority_boost`. Validation is centralized in `ResolutionEntry::validate` (`crates/foch-core/src/config.rs:335-388`), and the action is converted into `ResolutionDecision` in `crates/foch-core/src/config.rs:409-421`.

| Action | Type | Valid selectors | Runtime decision | Validation rules |
| --- | --- | --- | --- | --- |
| `prefer_mod` | String mod id | `file`, `conflict_id`, `match` | `PickMod(mod_id)` | Cannot combine with any other action (`crates/foch-core/src/config.rs:376-380`). If the mod is no longer a contributor at that conflict, the merge engine treats it as stale and defers (`crates/foch-engine/src/merge/patch_merge.rs:511-532`). |
| `use_file` | TOML string parsed as `PathBuf` | `file`, `conflict_id`, `match` | `UseFile(path)`; bytes are copied from that external path during materialization | Cannot combine with any other action. Materialization reads the source path and writes it to the target (`crates/foch-engine/src/merge/materialize.rs:1264-1288`). |
| `keep_existing` | Boolean | `file` only | `KeepExisting`; preserve an existing output-dir file | Must be `true` when present, and requires `file` selector (`crates/foch-core/src/config.rs:343-347`, `crates/foch-core/src/config.rs:382-386`). If the output file does not exist, foch warns and falls through to normal output (`crates/foch-engine/src/merge/materialize.rs:1244-1262`). |
| `priority_boost` | Integer (`i32`) | `mod` only | Stored in `ResolutionMap.mod_priority_boost` | `mod` requires `priority_boost`; `priority_boost` cannot combine with `prefer_mod`, `use_file`, `keep_existing`, or `handler`; `priority_boost` without `mod` is invalid (`crates/foch-core/src/config.rs:349-368`). |
| `handler` | String handler name | `match` only | Registry dispatches to a built-in handler | Requires `match` selector and cannot combine with any other action (`crates/foch-core/src/config.rs:370-380`). Unknown names parse successfully but log a runtime warning and defer (`crates/foch-engine/src/merge/handler_registry.rs:42-49`). |

Examples:

```toml
# prefer_mod: choose a contributor by mod id.
[[resolutions]]
conflict_id = "ab12cd34"
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

## 4. Match DSL syntax

The `match` selector uses this DSL:

```text
match-dsl  = file-side [ "::" [ address-side ] ]
file-side  = glob-side | regex-side
address-side = glob-side | regex-side
glob-side  = non-empty globset pattern, for example "common/ideas/**" or "**"
regex-side = "re:" non-empty Rust regex, for example "re:^events/.*\\.txt$"
```

`parse_match_dsl` trims the whole input, splits once on `::`, requires a non-empty file side, treats an omitted or empty address side as no address constraint, and compiles each side independently (`crates/foch-core/src/config.rs:191-218`). A side without `re:` is a glob compiled by `globset`; a side with `re:` is a `regex::Regex` (`crates/foch-core/src/config.rs:220-235`).

Important semantics:

- **File-only match**: no `::`, or a trailing empty address side, matches every conflict leaf in matching files.
- **Address-constrained match**: `file::address` only matches when both sides match. If the caller has no leaf address, address-constrained rules do not match (`crates/foch-core/src/config.rs:157-170`, `crates/foch-core/src/config.rs:311-315`).
- **Canonical address shape**: `LookupHandler` builds the leaf address as `address.path.join("/") + "/" + address.key`, or just `address.key` when the path is empty (`crates/foch-engine/src/merge/conflict_handler.rs:329-335`).
- **Use `**` for global file scope**: `::xx_*` is invalid because the file side is empty; write `**::xx_*` instead (`crates/foch-core/src/config.rs:207-210`).
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

A `handler` action names a built-in handler from the registry. Dispatch is case-insensitive because names are lowercased before matching (`crates/foch-engine/src/merge/handler_registry.rs:32-41`).

| Handler | Decision | Behavior | Source |
| --- | --- | --- | --- |
| `last_writer` | `PickModWithRecord` | Chooses the patch with the largest `(precedence, mod_id)` pair; `mod_id` breaks equal-precedence ties lexicographically for deterministic output. If there are no patches, it defers. | `crates/foch-engine/src/merge/handler_registry.rs:62-100` |
| `defer` | `Defer` | Passes the conflict to the next handler in the chain. In the default chain, dependency-implied resolution gets a chance next; if it also defers, the conflict can reach the interactive prompt/manual report. | `crates/foch-engine/src/merge/handler_registry.rs:38-55` |
| `keep_existing` | `KeepExisting` | Marks matching paths to keep the current output-dir file. If the target file exists, materialization records `kept_existing`; if it does not, foch warns and writes normal output. | `crates/foch-engine/src/merge/handler_registry.rs:38-60`, `crates/foch-engine/src/merge/materialize.rs:1244-1262` |

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

`conflict_id` is an eight-hex-character prefix of a BLAKE3 hash (`crates/foch-core/src/config.rs:424-435`). The hash input is:

1. the merge target file path, normalized by replacing `\` with `/`;
2. a NUL byte separator;
3. the canonical address path string;
4. a NUL byte separator;
5. the address key string.

The same file path, address path, and address key produce the same id across runs. The id changes when any of those inputs changes: moving/renaming the file, changing path normalization, changing how the patch engine computes `PatchAddress.path`, or changing the merge key/address key for the leaf. The stability tests verify that the id is eight hex characters, stable for identical inputs, and input-sensitive for file/path/key changes (`crates/foch-core/src/config.rs:811-831`).

`LookupHandler` computes the id from `current_file`, `address.path.join("/")`, and `address.key` just before lookup (`crates/foch-engine/src/merge/conflict_handler.rs:329-338`). Interactive choices are persisted as `conflict_id` entries for `prefer_mod` and `use_file`; `keep_existing` persists as a file-scoped entry because the action requires `file` (`crates/foch-engine/src/merge/conflict_handler.rs:689-733`).

To find a conflict id:

- In the simple interactive prompt, foch prints a conflict summary containing `conflict_id: ...` before the choice prompt (`crates/foch-engine/src/merge/conflict_handler.rs:480-497`).
- If you choose a candidate or external file interactively, foch appends a `[[resolutions]]` entry containing that id to the configured `foch.toml` (`crates/foch-engine/src/merge/conflict_handler.rs:566-585`, `crates/foch-engine/src/merge/conflict_handler.rs:689-720`).
- The generated `.foch/foch-merge-report.json` currently records counts plus conflict/handler resolution records, but its public `MergeReport` fields do not include per-leaf `conflict_id` (`crates/foch-core/src/model/merge.rs:190-211`). Use the prompt output, the persisted `foch.toml`, or recompute from file path + address when you need an exact id from a non-interactive run.

## 7. Lookup precedence

`ResolutionMap::lookup(file, conflict_id, leaf_address)` uses this precedence order (`crates/foch-core/src/config.rs:304-332`):

1. Exact `conflict_id` match in `by_conflict_id`.
2. Exact `file` match in `by_file`.
3. First matching `pattern_rules` entry in declaration order.

Worked example:

```toml
# Layer 3, declaration order position 1: global fallback policy.
[[resolutions]]
match = "**"
handler = "last_writer"

# Layer 3, declaration order position 2: never reached for common/ideas/**
# if the previous ** rule already matched.
[[resolutions]]
match = "common/ideas/**"
handler = "defer"

# Layer 2: exact file beats both pattern rules.
[[resolutions]]
file = "common/ideas/national_ideas.txt"
prefer_mod = "file_policy_mod"

# Layer 1: exact conflict id beats the file policy and all patterns.
[[resolutions]]
conflict_id = "abc12345"
prefer_mod = "specific_conflict_mod"
```

For a conflict in `common/ideas/national_ideas.txt` with id `abc12345`, foch picks `specific_conflict_mod` because `conflict_id` wins. For another conflict in the same file with a different id, foch picks `file_policy_mod` because `file` wins over patterns. For a conflict in `events/foo.txt`, foch uses the first pattern rule (`match = "**"`) because no id or file entry matches. If two pattern rules can both match, the earlier declaration wins; tests cover that first-match behavior (`crates/foch-core/src/config.rs:1128-1160`).

Avoid duplicate exact `file` or `conflict_id` keys. The current map uses `HashMap::insert`, so a later duplicate exact key overwrites the earlier one during map construction (`crates/foch-core/src/config.rs:285-289`). Pattern rules, by contrast, keep declaration order.

## 8. Common templates

### Global last writer

Use this only when you have decided that load-order semantics are acceptable for every structural conflict that reaches lookup. It is intentionally broad.

```toml
[[resolutions]]
match = "**"
handler = "last_writer"
```

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
conflict_id = "ab12cd34"
prefer_mod = "1234567890"
```

## 9. TUI integration

If no `foch.toml` entry resolves a conflict and dependency implication also defers, the conflict can survive into the post-pass interactive resolver. `foch merge` enables interactive mode when stdin is a TTY and `--non-interactive`/`--no-interactive` is not set; `--cli-prompt` forces the simple stdin/stderr prompt instead of the ratatui UI (`crates/foch-cli/src/cli/handler/merge.rs:28-47`, `crates/foch-cli/src/cli/arg.rs:149-155`). In auto mode, the post-pass selects ratatui when stdin/stdout are TTYs; otherwise it falls back to the simple prompt path (`crates/foch-engine/src/merge/conflict_handler.rs:777-821`).

The ratatui resolver is implemented in `crates/foch-engine/src/merge/tui_conflict_handler.rs`. It refuses to start when stdin/stdout are not TTYs and downgrades to `Defer` (`crates/foch-engine/src/merge/tui_conflict_handler.rs:341-357`). Its keybindings are:

| Key | Action | Source |
| --- | --- | --- |
| `↑` / `↓` | Move selection | `crates/foch-engine/src/merge/tui_conflict_handler.rs:132-142` |
| `Home` / `End` | Jump to first/last item | `crates/foch-engine/src/merge/tui_conflict_handler.rs:143-149` |
| `Enter` | Confirm selected item | `crates/foch-engine/src/merge/tui_conflict_handler.rs:151` |
| `Esc`, `d`, `D` | Defer | `crates/foch-engine/src/merge/tui_conflict_handler.rs:151-162` |
| `q`, `Q` | Abort merge | `crates/foch-engine/src/merge/tui_conflict_handler.rs:158-161` |
| `s`, `S` | Use an external file path; the dialog accepts `Enter` to confirm and `Esc` to cancel | `crates/foch-engine/src/merge/tui_conflict_handler.rs:158-163`, `crates/foch-engine/src/merge/tui_conflict_handler.rs:451-514` |
| `k`, `K` | Keep existing output file | `crates/foch-engine/src/merge/tui_conflict_handler.rs:158-164` |
| `1` ... `9` | Pick that candidate by visible candidate number | `crates/foch-engine/src/merge/tui_conflict_handler.rs:164-167` |

The footer displayed by the TUI summarizes the main bindings as `↑↓ select  Enter confirm  Esc/d defer  Q abort  S file  K keep` (`crates/foch-engine/src/merge/tui_conflict_handler.rs:281-287`). Picked decisions are persisted back to `foch.toml` through the same `resolution_entry_for_decision` path used by the simple prompt (`crates/foch-engine/src/merge/tui_conflict_handler.rs:323-339`).

## 10. Troubleshooting

Most schema errors surface while loading `foch.toml`: `FochConfig` validates `ResolutionMap::from_entries` during deserialize (`crates/foch-core/src/config.rs:40-52`), and load failures are reported as `failed to parse foch config <path>: ...` (`crates/foch-core/src/config.rs:489-577`).

| Error or symptom | Cause | Fix |
| --- | --- | --- |
| `invalid [[resolutions]] entry N: exactly one selector ... must be set` | Missing selector, or more than one of `file`, `conflict_id`, `mod`, `match`. | Split the policy into separate entries or keep only the intended selector (`crates/foch-core/src/config.rs:335-342`). |
| `invalid [[resolutions]] entry N: exactly one action ... must be set` | Missing action, or multiple actions such as `prefer_mod` plus `use_file`. | Keep exactly one action. For `mod`, the action must be `priority_boost` (`crates/foch-core/src/config.rs:349-380`). |
| `handler action requires match selector` | `handler` was used with `file` or `conflict_id`. | Replace the selector with `match = "..."`, or use `prefer_mod`/`use_file`/`keep_existing` for exact selectors (`crates/foch-core/src/config.rs:370-374`). |
| `keep_existing action requires file selector` | `keep_existing = true` was used with `conflict_id` or `match`. | Use `file = "..."` with `keep_existing = true`, or use `handler = "keep_existing"` with a `match` selector for pattern-scoped behavior (`crates/foch-core/src/config.rs:382-386`). |
| `keep_existing must be true when set` | TOML set `keep_existing = false`. | Remove the action or set `keep_existing = true` (`crates/foch-core/src/config.rs:343-347`). |
| `mod selector requires priority_boost action` | `mod = "..."` was paired with `prefer_mod`/`use_file`/`handler`, or no action. | Use `priority_boost = <integer>` and no other action (`crates/foch-core/src/config.rs:349-361`). |
| `priority_boost requires mod selector` | `priority_boost` was used with `file`, `conflict_id`, or `match`. | Change the selector to `mod`, or use a conflict action appropriate for the selector (`crates/foch-core/src/config.rs:364-368`). |
| `match pattern cannot be empty` or `match pattern file side cannot be empty` | `match = ""`, whitespace-only match, or an address-only pattern like `::xx_*`. | Use a non-empty file side; for global file scope, write `**` or `**::xx_*` (`crates/foch-core/src/config.rs:195-210`). |
| `regex pattern side cannot be empty after re: prefix` | A side was just `re:`. | Add a regex after the prefix or remove `re:` to use a glob (`crates/foch-core/src/config.rs:220-227`). |
| `invalid regex ...` | Regex compilation failed, often because of an unterminated character class or TOML escaping mistake. | Validate the regex and remember to double backslashes in TOML basic strings, or use TOML literal strings (`crates/foch-core/src/config.rs:228-230`). |
| Runtime warning: `unknown merge handler ...; deferring conflict` | `handler` name parsed but did not match `last_writer`, `defer`, or `keep_existing`. | Fix the handler spelling; handler names are matched case-insensitively but are not schema-validated at load time (`crates/foch-engine/src/merge/handler_registry.rs:32-49`). |
| Runtime warning: `stale pick ... mod ... is no longer a contributor; deferring` | A `prefer_mod` entry matched a conflict, but that mod no longer contributes at the conflict address. | Re-run interactively, inspect the current candidates, and update/remove the stale `conflict_id` entry (`crates/foch-engine/src/merge/patch_merge.rs:511-532`). |
| `keep_existing_failed: file does not exist at output dir` | A keep-existing action/handler matched, but the target file was absent in the output directory. | Seed the output file first, choose `use_file`, or remove the keep-existing rule (`crates/foch-engine/src/merge/materialize.rs:1244-1262`). |
