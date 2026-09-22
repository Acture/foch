# Project Status

Latest worktree verification: 2026-09-22 on `614aab6` plus the P-695 numeric
equivalence change: strict workspace Clippy and formatting, and `cargo test
--workspace --no-fail-fast` at 1,579 passed and 3 failed across all targets,
the three being exactly the sandbox denials `AGENTS.md` records as environment
results. Two existing expectations moved and are adjudicated in the entry below.
Earlier worktree verification: 2026-09-22 on `02ebd37` plus the P-687 boolean
canonicalization fix: strict workspace Clippy and formatting, and `cargo test
--workspace` green apart from the known sandbox denials, with no existing
expectation changed by the fix.
Earlier worktree verification: 2026-09-21 on `417b2d5` plus the P-658 per-entry
no-op path fix: strict workspace Clippy and formatting, `cargo test --workspace`
green apart from the known sandbox denials, and a bounded vanilla probe of the
three families that enable per-entry no-op dedup.
Earlier worktree verification: 2026-09-20 on `e7439c5` plus the uncommitted
P-640 lineage fix: strict workspace Clippy and formatting, `cargo test
--workspace` at 1,521 passed and 3 failed (all three sandbox socket denials),
and a bounded recheck of the 167 retained Workshop paths that failed lineage
validation, at one and four workers.
Earlier committed-source verification: 2026-09-19 at `4b8444c` (P-609, parallel
merge units): workspace tests, strict workspace Clippy and formatting, plus a
bounded serial/parallel comparison on the retained 30-mod Workshop page.
Earlier committed-source verification: 2026-09-19 at `12a7f92`: full workspace
tests, strict workspace Clippy, formatting, and workspace/test compilation.
The automatic Workshop probe, missing-input fix, definition-module indexing,
and tree matching/lineage fixes are now committed.
Earlier committed-source verification: 2026-09-14 at `f0d348f`: workspace regressions and
isolated CLI smoke tests for cross-directory output and version matching.
Earlier source verification: 2026-09-10 for cross-directory database output
(P-580) and rule-version matching (P-591).
Earlier source verification: 2026-09-07 for database-rule planning (P-579).
Earlier focused verification: 2026-09-05, P-553/P-556 static-modifier product
fixtures and bounded Workshop observation. See
[the verification record](./static-modifiers-verification.md).

Earlier project-wide source verification: 2026-08-25 on branch `refactor/structure-reset` at
`30aa902` (`Update quality harness for merge reviews`).

This page is the repository handoff. Recheck Git and local inputs before using
any checkpoint fact. Linear owns live execution; Notion holds the project
narrative and research record.

## Numeric equivalence under the game's field coercion (2026-09-22)

P-695. EU4 reads a script number through a reader far coarser than byte
comparison, so two mods can write one value several ways and foch reported the
difference as a content conflict. Measured before the change, not assumed: on
`common/ideas/test.txt` with `land_morale`, `0.5` vs `0.50`, `1` vs `1.0` and
`0.1234` vs `0.1239` each produced a `Policy` conflict at
`["test_idea", "land_morale"]`, alongside the genuine `0.5` vs `0.6`.

The reader. `CToken::ReadValue(CFixedPoint&)` takes the integer part with
`sscanf("%i")`, copies at most three fraction digits into a `"000"` buffer and
scales the integer by 1000 — three decimals, truncated, never rounded.
`CToken::GetInt` is `atoi`. The finer `GetFloat64` at 1/32768 has two callers,
neither a script path. That is modelled in `src/game/eu4/coercion.rs`, with the
evidence restated in the module's own doc comment rather than cited, because
the P-687 findings directory lived under `target/` and is gone.

Where it acts, and why not where the issue said. The issue ruled out the
normalization layer on the grounds that `equivalent_subtrees` compares
`subtree_hash` before recursing, so a leaf-equal, hash-unequal pair would be
equal under an `Ordered` parent and unequal under a `Commutative` one. That
argument holds against making the *comparison* coercion-aware; it does not hold
against rewriting the *value*, because the hash is computed from the value and
follows it. The first implementation followed the issue and resolved
equivalence inside `resolve_nway_divergent_node`; the maintainer redirected it
to semantic normalization, and that is what shipped.

`canonicalize_numeric_values` (`src/merge/numeric.rs`) rewrites the AST before
the merge, beside `canonicalize_boolean_or_definitions` and ahead of it, since
that pass deduplicates disjuncts by exact scalar text. The AST is what every
identity is derived from — `assignment_anchor` and `value_fingerprint` read
`AstValue` directly, and the normalized tree hashes the values it carries — so
the matcher, the hashes, the anchors, `definition_provenance` and the
semantic-equivalence check all agree by construction instead of each learning
the rule. Nothing needs a decision label any more: the values are identical
before the merge looks at them, and both contributors stay in provenance, so
the trace no longer calls the losing spelling an override. The
`MergePolicyKind` variant, the `PolicyDecision` policy field, the
`MergeTraceEntry` count and the review note the first implementation added were
all removed with it.

Doing it in the AST also removed a class of guesswork. The first version had to
recover the field path from `policy_path`, whose components are anchor values —
either a bare key or `key:identity`, indistinguishable as text, and vanilla
writes `event_target:<name>` as an ordinary assignment key about 1,890 times.
That forced two readings and a unanimity rule. A top-down AST walk holds the
real keys, and `RuleContext` descends with the recursion: one binding per block
entered, cached by ancestor chain, rather than one per number.

Canonical form. `CFixedPoint` holds thousandths, so a `float` field is written
with exactly three decimals and an `int` field as the integer `atoi` reads.
This is the engine's representation, not a formatting preference.

Honest coverage and its price, measured through the shipped transform against
the installed 1.37.5 over `common/`, `events/`, `decisions/` and `missions/`
(1,941 files; the probe is `measure_coverage_against_vanilla`, `#[ignore]`d,
needs `EU4_ROOT`):

- **1,783** numbers are spelled redundantly — a trailing zero or a fourth
  decimal, the shape a sibling mod could write differently. **772 of them
  (43%)** sit on a field the schema types, and those are the false conflicts
  this removes.
- **26,559** numbers are rewritten in total, because every schema-typed number
  takes the canonical spelling whether or not anything ever disagreed about it.
  That is the cost side of writing the engine's representation: roughly 34
  numbers change spelling for each potential false conflict removed.

The 1,011 redundant numbers that are left alone are dominated by the modifier
keys the vendored CWT config records only in its `modifiers = { ... }` registry
and never declares as `alias[modifier:<key>]`: `land_morale`,
`global_tax_modifier`, `trade_efficiency`, `stability_cost_modifier`. That is a
CWT-coverage question, not a merge one. Where the schema is silent the text is
left exactly as written, which is the point — the tree asserts a value only
where the meaning is known.

Two supporting fixes came out of it and are worth keeping separate.
`SchemaScalarType` and its range parsing were private to
`src/game/eu4/editor/schema/interpret.rs`; they are CWT vocabulary rather than
EU4 interpretation and now live in `src/game/schema/query.rs`, shared with the
LSP. `SchemaScalarType::matches` stayed behind as
`schema_scalar_type_matches`, because schema conformance is deliberately
stricter than what the game reads and conflating them would fold `123abc` into
`123` on the strength of a check that rejects it. And
`CompiledBindFieldMatch::value()` now reads `alias.value` instead of the
wildcard's `alias_match_left[...]` marker: 682 of 1,428 `int`/`float`
declarations in the vendored config are alias-bound, and `rule_field_for_path`
had been reading the marker, so an alias-bound block field was suggested as
`Replace` rather than `Recursive`.

Deliberately out of scope. `123abc` and `123` are one value to `GetInt`, but
`script_int` abstains on text it has not modelled, because `atoi` over
arbitrary text makes every unparseable string `0` and therefore equal to every
other — an equivalence that would hide real divergence rather than explain it.
`bool` is excluded for the same kind of reason: `CToken::GetBool` compares
against the exact lowercase `yes`, so every other spelling reads false and they
would all collapse together, including the mis-spellings `V009` reports.

Scoring had to follow, or the harness would count its own tool's output as a
divergence. Four separate paths needed it — `canonical_ast`,
`semantic_atoms_for_path_with_ordering`, the layered module view, and the text
similarity behind `matches_human` — and adding it at each of them in turn
missed two: the module view, which `tiny_product_cli_to_pure_scorer_seam`
caught by regressing to `diverges_ast`, and the similarity path. That is a
design fault, not an attention one, so the four collapsed into one module,
`merge_quality::representation`. It is now the only thing in the harness that
reads a script file for comparison, and `score.rs` no longer imports a raw
parser. The single exception is the per-file read inside module composition,
which needs the library's own loader; the composed tree goes through the same
seam immediately after, and the call site says so.

The seam keys on the **game-relative** path, which is load-bearing and was
found by measurement rather than reading: root binding is a path-prefix match,
so the absolute scratch paths the harness holds bind nothing and canonicalize
nothing, silently. The same content at the same bytes now caches per relative
path for that reason.

The similarity path needed a different shape from the rest.
`canonicalize_numeric_text` replaces the numbers' byte ranges in the source
instead of re-emitting the parsed file, because re-emitting would also reformat
the comments and whitespace that this metric is measuring — the reformatting
would swamp the signal. Without it, `0.500` against a human patch's `0.50`
scored 0.667 on a three-line file, and a unit that was `matches_human` (AST
equal and similarity >= 0.92) would have degraded to `matches_ast` across the
cohort for no real reason.

Two existing expectations moved, both adjudicated rather than made green.
`event_merge_amalgamates_independent_ordered_insertions` now expects
`add_prestige = 1.000`, because `alias[effect:add_prestige]` is a schema
`float` — and that makes this test schema-dependent, one more in the cluster
`AGENTS.md` records for an uninitialized `vendor/cwtools-eu4-config`.
`tiny_product_cli_to_pure_scorer_seam` regressed to `diverges_ast` until the
module-view path was canonicalized too, which is how that third scorer path was
found.

Regressions: `text_similarity_ignores_how_a_number_is_spelled` pins the
scorer's text path. Fourteen unit tests in `src/game/eu4/coercion.rs`, including one
proving fixed-point equality is never coarser than integer equality — so a
wrong field type cannot turn a real difference into an equivalence — and one
proving the canonical spelling is idempotent, since the transform runs on
output it produced earlier. Twelve in `src/merge/numeric.rs` for the transform and
its abstain cases, two of them proving the text rewrite moves the numbers and
nothing else. Eleven in `merge::structured::tests::game_value_equivalence`
for the product verdict, including `how_a_value_is_spelled_changes_nothing_downstream`,
which pins the real property: under any policy, `0.5`/`0.50` produces the same
verdict and the same output as `0.5`/`0.5`. Two in `src/game/eu4/cwt/merge.rs`
for the alias fix. And `merge_reads_one_value_written_two_ways_as_one_value` in
the CLI integration suite, which is the end-to-end proof that the schema
binding reaches the real pipeline — the canonicalization keys off the
game-relative path, and only the production plumbing supplies it.

Validation: `cargo fmt --all --check` and strict workspace Clippy passed.
`cargo test --workspace --no-fail-fast` left only the three tests `AGENTS.md`
records as sandbox denials (`PermissionDenied` binding a Unix socket or a local
HTTP server).

Derived blocker, filed as **P-709**: the compiled CWT pack is not embedded in
the binary. `schema_candidates()` resolves `vendor/cwtools-eu4-config` through
`env!("CARGO_MANIFEST_DIR")`, the build machine's source path. Before this
change a missing schema cost only capability; now it decides output bytes, so
the same binary on another machine would produce a different merged mod.

Not established: any full-page or fixed-cohort merge with this change, and
whether a real Workshop mod pair hits the covered 43% rather than the
abstaining 57%. The vanilla measurement bounds the shape's frequency, not its
rate in mods.

## Boolean canonicalization split equivalent statements (2026-09-22)

P-687, found during the P-658 audit and fixed separately: it is the opposite
defect. P-658 was a judgement made outside its content family, which kept a
duplicate. The two defects recorded here instead make
`canonicalize_boolean_or_definitions` turn an **equal** pair into an unequal
one, and they sit on the production n-way path —
`merge_clausewitz_files_n_way` canonicalizes before `detach_trivia`, the same
order the equivalence check uses.

First, comments decided structure. `body_to_disjunct` wrapped a body in `AND`
whenever it held more than one statement, and `AstStatement::Comment` counted.
`detach_trivia` then removed the comment and left the `AND` behind, so a
comment-only difference outlived its trivia as a structural one: under
`common/scripted_triggers`' real policies, `t = { a = yes }` and the same
definition with one comment above `a` compared as different content.

Second, deduplication used a coarser relation than the judge. `unique_disjuncts`
deduplicated with `ast_statements_semantically_equal`, which equates `SWE` and
`"SWE"` — correct for patch convergence, where two mods writing one value
differently must converge — while the normalized tree gives the two spellings
different leaf kinds. Dedup keeps the first survivor, so
`OR = { tag = SWE tag = "SWE" }` and its reordering canonicalized to
`OR = { tag = SWE }` and `OR = { tag = "SWE" }`: equivalent before
canonicalization, not equivalent after.

Fix. Comments are lifted out of the disjunct walk and re-emitted with the body,
so every structural choice is made on content alone and the transform is blind
to trivia. Deduplication uses `ScalarEquality::Exact`, which is no coarser than
the tree that later judges the output; `patch.rs` now parameterizes one
comment-ignoring walk rather than growing a second copy, and every other caller
keeps the convergence relation it was written for.

Deduplication compares disjuncts with comments ignored at every depth, so two
that differ only in a nested comment collapse to one. What the discarded one
said is now lifted out and re-emitted with the body rather than going with it —
that loss predates this change, but the fix claims comment preservation, so it
had to hold there too.

Two deliberate behavior changes: a body holding only comments no longer becomes
`OR = {}` — it states no condition, so it keeps its comments and emits no
disjunction — and lifted comments are re-emitted ahead of the `OR` rather than
inside the `AND` their presence used to create. Both are pinned.

Not fixed here: the underlying asymmetry that produced the second defect. The
normalized tree distinguishes `SWE` from `"SWE"`; `scalar_values_semantically_equal`
does not. Which side matches EU4 was asserted without evidence when this fix
landed, and has since been checked against the installed 1.37.5: vanilla writes
81 distinct `key = value` pairs both ways, including identity lookups such as
`has_country_modifier`, and `counter_reformation` — defined once as a bare block
key — is referenced both bare and quoted in shipped missions and events, which
those triggers could not survive if the loader kept the spellings apart. The
rule behind that was then read out of the game binary, which ships unstripped
with full C++ symbols: `CTextLexer::GetTok` stores `{int type, char text[512],
bool wasQuoted}`, strips the delimiters, and assigns type 15 both to a quoted
string and to a bare word that misses `CLexer::_TokenTree` — so the two spellings
differ only in a flag kept for round-tripping. `CLexer::GetPrimitiveString` names
ids 12-15 `num`, `float`, `bool` and `string`, and the text lexer only ever emits
`num` and `string`: for text script every other scalar meaning is a
consumer-side coercion, which is why a value like `00_government_names` is a
`num`. They diverge only where the bare
form would lex as a number (leading `-` or digit), match a tree entry, or split
into several tokens, which is exactly what `is_valid_bare_identifier_text`
already refuses to equate. Its `yes`/`no` carve-out is the exception:
`CToken::GetBool` is `strncmp(text, "yes", 4)`, reading text rather than type, so
that exclusion is over-conservative rather than necessary. That comparison is
also case-sensitive, and it is the only thing that decides a boolean, so `YES` is
not `true` to EU4. foch's tokenizer lowercased before matching and yielded
`Bool(true)` — foch being more permissive than the game, the unsafe direction,
and the first lexer divergence found that is a correctness risk rather than extra
strictness. Fixed below. Unestablished: what
`AddDynamicToken` puts in the tree at script-parse time, and whether consumers
other than `GetBool` branch on the type. Making the kernel equate the spellings
would move leaf kinds, subtree hashes and cache identity, so it stays a separate
question; this fix only stops canonicalization from relying on the coarser side.
Evidence: `target/validation/p687-quoting-2026-09-22/`, including the
disassembly.

That last one was not latent. foch's tokenizer lowered a bare word before
matching, so `YES` parsed to `Bool(true)`, and `render_scalar` writes every
`Bool(true)` as the canonical `yes` — measured end to end, `v = YES` came out of
foch as `v = yes`. To the game that rewrites a value it reads as false into one
it reads as true, in any merged file whose source wrote `YES` or `Yes`. Fixed by
matching only the exact lowercase spellings; everything else stays an identifier
and keeps its text. The whole workspace suite passed unchanged, so nothing
depended on the folding. Regressions:
`only_lowercase_yes_and_no_are_boolean` and
`emitting_a_non_lowercase_yes_keeps_its_spelling`.

Two LSP diagnostics came out of this, after one wrong turn. `V008` reports a
numeric literal on a schema `float` field carrying a fourth decimal, and `V009`
a `yes`-shaped value on a schema `bool` field that is not the exact lowercase
spelling. Both are errors where the game reads a different value than the file
states, and `V008` drops to a warning when the discarded digits are zeros, where
the value survives but the written precision still is not the one kept. At
exactly three decimals it says nothing.

The wrong turn is worth recording. `V008` was withdrawn on the argument that
vanilla writes `monthly_piety = 0.0025` thirteen times, which three-decimal
truncation would turn into a fifth-off balance value, so modifier fields must
use the game's finer reader. Counting the call sites disproved it: the finer
`GetFloat64` at 1/32768 has two callers, `CCountry::ReadMember` and an `fpml`
overload, neither a script path, while the script surface —
`CReader::Read(CFixedPoint&)`, `TValueEffect`, `TValueTrigger` — reaches
`CToken::ReadValue(CFixedPoint&)`, which copies at most three fraction digits
into a `"000"` buffer and scales the integer by 1000. Script reads three
decimals. The same vanilla evidence reads the other way once the rule is known:
only 15 values in `common/`, `events/`, `decisions/` and `missions/` carry a
meaningful fourth decimal at all, 13 of them that one number, which is what a
rare authoring slip looks like rather than a family with finer precision.

Reading the consumers rather than the lexer turned out to matter more.
`CToken::GetFloat` tail-calls `StringToFixedPoint`, which is `atoi` for the
integer part plus **exactly three truncated decimal places**, so `0.5`, `0.50`
and `0.500` are one value to the game and `0.1234` and `0.1239` are both `0.123`;
`GetInt` is `atoi`, so `123abc` is `123`; `GetBool` is the case-sensitive
`strncmp` above. foch compares scalars as source text and therefore reports all
three numeric pairs as different — measured, not assumed — while folding `yes`
and `YES` that the game keeps apart. Three of those are foch being stricter than
the game (spurious divergence), one is foch being looser (a real risk). The
coercion is per field, so any normalization has to be driven by the CWT field
type rather than by the lexer, and it is pinned to this engine build. Nothing has
been changed for it yet. Evidence: `findings-coercion.md` beside the
disassembly, under `target/validation/p687-quoting-2026-09-22/` — which is a
build directory and is **gone**. The call-site census it recorded (113 callers
of `GetFloat`/`StringToFixedPoint`, 8 of `CToken::ReadValue(CFixedPoint&)`, 2
of `GetFloat64`, the last two both save-data paths) survives in the P-695
Linear thread and in the prose above; the disassembly itself would have to be
retaken to re-derive it.

Regressions: five unit tests in `src/merge/boolean.rs` cover the transform
itself (comment-blind shape, comment survival, comment-only body, both scalar
spellings kept, genuine duplicates still collapsed, and the simplify path
keeping a comment through an `AND` unwrap), and
`canonicalization_does_not_make_a_comment_a_content_difference` plus
`canonicalization_does_not_change_the_verdict_on_reordered_quoted_scalars` pin
the product-visible relation. The second compares the family verdict against a
policy that never reaches a trigger root, so it asserts the property directly:
canonicalization must not change the verdict.

Validation: `cargo fmt --all --check` and strict workspace Clippy passed.
`cargo test --workspace` passed apart from the sandbox denials that `AGENTS.md`
records as environment results. **No existing expectation changed** — the fix is
invisible to every corpus fixture and merge test already in the suite, which
bounds how much current output it can move, though it does not prove the same on
Workshop input.

Not established: a full-page or fixed-cohort merge with this change, and whether
any real mod pair actually hit either shape.

## Per-entry no-op equivalence ran outside its content family (2026-09-21)

P-658, the empty-path case that P-640 left out of scope.
`clausewitz_statements_semantically_equivalent` wrapped both sides in
`AstFile { path: PathBuf::new(), .. }`, and `canonicalize_boolean_or_definitions`
is the only step downstream of it that reads the AST path. An empty path
classifies as `ScriptFileKind("other")`, which matches no
`hand_container_scope_fallback` arm and binds no CWT root, so
`script_container_scope_kind` is always `None` there.

The issue's premise needed narrowing. `script_context` classifies a keyword set
(`trigger`, `limit`, `potential`, `allow`, `condition`, `hidden_trigger` and the
boolean operators) before it consults the scope kind, and the
`configured_definition` branch is policy-driven, so both still fire under an
empty path. What is lost is the schema and hand-fallback role for *non-keyword*
containers. The two sets of canonicalization roots are incomparable rather than
nested: a real path can also suppress a root, because a `Trigger`-scoped parent
propagates `Trigger` down and makes a keyword child fail `parent_context !=
context`.

Measured, not argued. A temporary probe parsed every vanilla file of the three
families whose descriptors call `.per_entry_dedup_safe()` —
`common/scripted_effects`, `common/scripted_triggers`, `common/ideas` — and
compared each top-level definition normalized under an empty path against the
same definition under its real relative path: 31 files, 5,315 definitions, **10
that normalize differently**, all 10 with the real path emitting net more
canonicalization. That is a per-definition net, not a per-node comparison, so it
does not exclude a suppressed root inside a definition that gained more than it
lost. The smallest is
`common/scripted_triggers/02_scripted_triggers_for_mission_conditions.txt:11`,
where `custom_trigger_tooltip` is a trigger container for that family only
through the hand fallback and sits under a keyword *effect* parent (`if`), so
the parent context does not already supply `Trigger`. A mod re-shipping vanilla
in the explicit `OR`/`AND` shape was judged different from vanilla and kept. The
observed direction is under-permissive: a retained duplicate, not a dropped
contribution. Re-rooting divergence was 0 — all three families use
`MergeKeySource::AssignmentKey`, so only top-level definitions are ever
compared and the synthetic one-statement file keeps the statement at its real
depth.

Reach, stated separately from the defect. The decision has one production call
site, `src/merge/output/materialize/per_entry_noop.rs`. The semantic backend
(`MergeBackendId::GumtreePcsNway`, the default) skips
`drop_per_entry_noop_duplicates` whenever `preserves_complete_tree_module`
holds, and `enable_common_definition_modules` makes all three gate-passing
families definition modules, so on the product backend the affected set is
empty. The address-patch backend calls it ungated and states in-file that it is
test-only. So nothing here places the defect in current product merge output; it
is a latent gap that becomes live if a per-entry-safe family is not a definition
module or that guard changes.

Fix: `clausewitz_statements_semantically_equivalent` takes the game-relative
path, threaded from `base.ast.path` through `drop_per_entry_noop_duplicates`.
That path is already in scope in both calling frames — `structural.rs` uses it
two statements earlier for the whole-file comparison — and for a definition
module `fold_visible_module_files` gives the folded view the module
`output_path`, which classifies into the same family.

Regressions:
`statement_equivalence_resolves_containers_from_its_content_family_path` pins
both arms of the contrast, including that an empty path must *not* be
family-aware, so a future refactor that drops the path fails rather than
silently reverting;
`per_entry_noop_drops_a_vanilla_equivalent_definition_under_its_content_family`
and `per_entry_noop_keeps_a_changed_definition_under_its_content_family` cover
the owning output flow. Reverting the threading fails only the first of that
pair, which is the intended asymmetry: the fix adds recognition of equivalence
and never drops a real change.

Validation: `cargo fmt --all --check` and strict workspace Clippy passed.
`cargo test --workspace` passed apart from
`output_transaction_rejects_an_existing_unix_socket`, a sandbox socket denial
that `AGENTS.md` already records as an environment result. Evidence is under
`target/validation/p658-empty-path-2026-09-21/` (`findings.md`, the archived
`p658_probe.rs`, `divergent-definitions.txt`, and the 10 original/empty-path/
real-path canonical-form triples in `divergences/`).

Not established: a full-page or fixed-cohort merge with this change, any change
to product merge output, or in-game behaviour. Only vanilla was surveyed, so
10/5,315 is a vanilla-corpus rate, not a mod rate.

## Workshop lineage input-tree inconsistency (2026-09-20)

P-640. The retained full-page run `run-mrGKJR` failed output validation with
179 engine failures, 167 of them `lineage tree does not match merge input for
partition File`. That report predates the parallel merge work, so the class was
first re-verified on current master `e7439c5` through `analyze_merge`: the three
representative paths `common/technology.txt`, `decisions/AndalusianNation.txt`
and `decisions/ArabNation.txt` all still failed with the same error.

Cause: one AST was classified under two path conventions. `parse_script_file`
and `parse_script_bytes_cached` left the disk path in `ParsedScriptFile.ast.path`
while setting `relative_path` correctly, and a decoded base snapshot kept the
absolute path of the machine that built it, because `rebase_parsed_documents`
repairs only the outer `path`. `normalize_clausewitz_file` identifies the
content family and resolves CWT block roles from that AST path, and
`classify_content_family` matches relative prefixes, so an absolute path
resolved to `other` and Boolean-OR canonicalization did not run.
`TreeDagProtocol::effective_node` observed lineage from `source.ast.path` while
`TreeDagProtocol::join` rebuilt the merge input from `FileDag::file_path()`, so
the same statements normalized into different trees and `compose_join_lineage`
rejected the join.

The repair applies the existing relative-path contract at the two entrypoints
that construct a `ParsedScriptFile`, not at the site that reported the error:
`parsed_script_file_from_result` in `src/game/eu4/script/mod.rs` and
`StoredParsedScriptFile::into_parsed_script_file` in
`src/game/eu4/base/snapshot/parsed_scripts.rs`. `ParsedScriptFile.path` still
holds the disk location, and `rebase_parsed_documents` is deliberately
unchanged. Both entrypoints are needed: `InputScriptCache` fills `loaded` from
the base snapshot and `lazy` from on-disk mod files, so fixing either alone
leaves the other side of every join on the old convention. Normalizing at
snapshot decode rather than at build also repairs snapshots already installed or
downloaded; `StoredAstFile.path` is retained because bincode is positional, and
no schema, wire-format or analysis-rules version bump is required. The lineage
consistency check itself is unchanged.

Bounded recheck on the retained ordered playset, its Workshop trees read in
place, and the same installed EU4 v1.37.5.0 base that `run-mrGKJR` recorded
(`snapshot.bin` sha256 `fec03656…` matches its `base-metadata.json`). The
representative window went from 3 engine failures to 0: two files generated and
`decisions/ArabNation.txt` deferred as `needs_user_choice` on a real
`delete_modify` conflict. Analyzing all 167 same-class paths at once gave 81
generated, 60 `needs_user_choice`, 26 skipped as semantic no-ops against
vanilla, and 0 engine failures, with no deferral of any other reason. One worker
and four workers produced identical committed trees, provenance and merge traces
under the P-609 masks.

This changes normalization for every structured merge, not only the files that
were failing. Merge inputs and the vanilla ancestor now canonicalize under their
real content family, so the no-op-against-vanilla check in
`src/merge/output/materialize/structural.rs` and stale-vanilla-target detection
run under that policy too, and a full-page file count can differ from
`run-mrGKJR`. Newly built base snapshots encode a relative `StoredAstFile.path`,
so `data build eu4` output bytes change for identical game content. Nothing a
mod snapshot persists depends on the AST path, so no cache version was bumped.

Out of scope and untouched: the other 12 engine failures in that report (one
missing non-empty vanilla base, two cross-file module failures, nine
control-flow/event-join failures) and the pre-existing empty-path normalization
in `clausewitz_statements_semantically_equivalent`
(`src/merge/structured/merge.rs:300`), which is the same defect class and is
tracked separately as P-658.

Regressions: `parsing_from_an_absolute_root_keeps_the_relative_path_in_the_ast`
and `parsing_supplied_bytes_keeps_the_relative_path_in_the_ast`,
`decoding_a_foreign_snapshot_restores_the_relative_ast_path`,
`dag_join_accepts_inputs_parsed_from_absolute_roots` — which fails with the
exact production error string when the entrypoint fix is reverted — and
`dag_join_still_rejects_a_lineage_tree_that_does_not_match_its_input`, which
passes on both sides of the fix, so the check was not weakened.

Validation: `cargo fmt --all --check` and strict workspace Clippy passed.
`cargo test --workspace --no-fail-fast -- --test-threads 4` reported 1,521
passed and 3 failed; all three failures are sandbox socket denials
(`output_transaction_rejects_an_existing_unix_socket`,
`data_install_downloads_release_asset_from_manifest`,
`page_fetch_is_frozen_and_reused_without_another_network_request`) and were not
re-run outside the sandbox in this session, so the pre-push gate remains the
maintainer's check. Evidence is under
`target/validation/p640-lineage-2026-09-20/` (`findings.md`, the run logs and
reports, `lineage-failure-paths.txt`, `manifest.toml`, the archived
`p640_retained_window.rs` harness and `normalization-probe.rs`).

Not established: a complete full-page merge with this change, the fixed cohort,
or in-game behaviour. `cargo workshop-probe` stays the maintainer's full-page
run and P-581 keeps full-flow validation.

## Parallel merge units (2026-09-19)

P-609 analyzes independent merge units on worker threads and applies their
results in plan order, so the worker count changes when a unit is analyzed but
not what is written. Commits:

- `02b1d01` — split each unit into analysis, which reads only frozen inputs, and
  apply, which does every output write and every report and review change.
- `d01370e` — `run_units` in `src/merge/output/materialize/executor.rs`: scoped
  workers (`foch-merge-N`, 64 MiB stacks like the CLI main thread) take plan
  indices in order; the calling thread applies results strictly by index. The
  first error or panic in plan order ends the run, cancellation is checked before
  every apply, a definition module stays one job, and copy, overlay and deferred
  units never reach a worker. One worker keeps the previous serial path.
  `MergeAnalysisOptions.merge_workers` and `foch merge --jobs N` set the count.
- `98360a4` — cache entries are written through unique temporary files, since
  workers store parse and address-patch cache entries concurrently.
- `50ef6a8` — the default count is the detected CPU count, and memory is bounded
  separately: a unit starts only while its estimate (2,000 bytes per input byte)
  fits beside the unfinished ones within 60% of physical memory less the memory
  the process holds when units start; a larger unit runs alone. Units dispatched
  ahead of the one being applied are bounded at 256 per worker.
- `2cd5257`, `1fe4c6f` — review fixes: the budget subtracts current rather than
  lifetime-peak memory, so a long-lived desktop process keeps its parallelism;
  Linux honours cgroup memory limits; scheduler tests fail through a watchdog
  instead of hanging.
- `70fa206` — interactive merges stay parallel. Workers analyze without
  prompting; when the unit about to be applied could have prompted (any backend
  outcome other than a clean merge), the calling thread pauses the workers,
  waits for running ones, analyzes that unit again with the prompt, and resumes
  them. Prompts and the resolutions they persist stay in plan order and alone on
  the terminal; with 1, 4 and 8 workers a handler answering every conflict gives
  the same prompts, the same `foch.toml` and identical output. `4b8444c` makes a
  panic during such a redo stop the paused workers instead of releasing them.
- `e8b163e` — output validation checks the generated mod against the game
  installation the merge resolved, including a manifest's
  `[project].game_path`. Before, it fell back to Steam discovery, so the two
  Workshop probe tests failed on every CI platform since `12a7f92` (also on
  `master`) and a local run could validate against a different installation.

`definition_module_elapsed_ms` is now the sum of each module's analysis and
apply time rather than one wall-clock span; it was already excluded from any
byte comparison.

A bounded comparison used the retained 30-mod newest page, a cloned probe cache
and data dir, retained windows from the 2026-09-17 plan, and release builds, one
run at a time on a shared 32 GiB M2 Max. The table gives `materialize`, the
parallel phase, and the `/usr/bin/time -l` peak memory footprint. File windows
were measured at `50ef6a8` and the module window at `1fe4c6f`; the budget
change between them does not bind for small units.

| Window (units) | 1 worker | 12 workers, admitted | Peak footprint 1 → 12 |
| --- | ---: | ---: | --- |
| history/provinces (3,916 files) | 21.4 s | 3.0 s | 6.34 → 7.08 GB |
| common/countries (1,224 files) | 84.4 s | 17.8 s | 6.58 → 7.11 GB |
| 9 largest definition modules | 97.3 s | 62.4 s | 13.33 → 18.09 GB |

Every multi-worker output was identical to its one-worker output after masking
only the report's module time, the plan's `generated_at` and the output path in
`descriptor.mod`; one-worker control runs of the two file windows were identical
too. With 4 workers the country window took 37.1 s under the earlier bound of 16
pending units per worker and 23.5 s at 256, because workers idled behind its
9 s units. With one worker the nine modules peaked at 0.65 to 6.9 GB each (325
to 1,758 bytes per input byte), about 29 GB together beside a 6 GB baseline,
which is why the default is bounded by estimated memory instead of CPU count.
File units peaked at a median of 1.7 to 5.7 MiB. These are local observations,
not a controlled benchmark; the slowest country unit grew from 9.0 s to 12.5 s
at 12 workers, most likely from contention.

Tests compare 1, 2, 4 and 8 workers, and 4 workers with room for one unit, byte
for byte on a playset with every unit kind while an early conflict is forced to
finish after a late one. Gate-based tests cover overlap, the worker, pending and
memory bounds, module atomicity, ordered errors and panics, cancellation and the
interactive fallback; for each, the matching scheduler defect was injected and a
test failed. The CLI runs `--jobs 1` and `--jobs 4` to identical trees.

Validation on `4b8444c`: `cargo fmt --all --check`, strict workspace Clippy and
`cargo test --workspace --no-fail-fast -- --test-threads 4` passed (1,516 tests)
except the three that need local sockets or HTTP servers, which pass outside the
sandbox; the pre-push `cargo test --workspace` gate passed outside it. The two
Workshop probe tests also pass with an empty `HOME`, which reproduces the CI
failure `e8b163e` fixes. The Windows memory query in `src/platform/memory.rs` is not
compiled locally. Evidence, harness and per-unit tables are in
`target/validation/p609-parallel-2026-09-19/` (`summary.md`, run logs, reports,
`*-w1-mem.units.tsv`, `local_parallel_probe.rs`, `run.sh`, `compare.sh`).

Known limit: with prompts on, a module's later namespaces are prompted before an
earlier one is staged, so a staging I/O error can follow a prompt the serial loop
before P-609 would not have shown. A unit that could prompt is analyzed twice
when workers are in use.

Not established: a complete full-page merge with these changes, the fixed
cohort, and in-game behaviour. Repeating `cargo workshop-probe` on the installed
inputs is the maintainer's full-page check and belongs to P-581.

## Commit and validation checkpoint (2026-09-19)

The previously uncommitted implementation is recorded in these commits:

- `02ef0fb` — reject unavailable enabled mod inputs, with CLI regressions.
- `a95cd6f` — avoid repeated database/tree work, with matching, lineage and
  traversal-work regressions.
- `12a7f92` — add resumable Workshop page probes, account selection, source
  identity checks, progress diagnostics, the frozen page fixture and usage docs.

All source commits passed the installed pre-commit gate: `cargo fmt --all --check`,
strict all-target/all-feature workspace Clippy, and `cargo build --workspace --tests`.
`cargo test --workspace --quiet` then passed on `12a7f92`, including the local
socket/HTTP fixtures, CLI integration tests, corpus-harness tests and desktop
tests. Checks used `DEVELOPER_DIR=/Library/Developer/CommandLineTools`; no hook
was bypassed. Local logs are under `target/validation/commit-push-2026-09-19/`
(`commit-input.log`, `commit-performance.log`, `commit-probe.log`, and
`workspace-tests.log`).

This delivery check did not launch the ignored full Workshop jobs or EU4, and
does not establish a complete page/cohort result. P-581 retains full-flow
validation ownership; P-609 tracks the still-unimplemented parallel merge work.
Earlier dated sections describe their own checkpoint, including input
availability and validation limitations at that time.

## Full-page retry stalled in advisor history (2026-09-17)

The maintainer's retries, `target/workshop-probe/runs/run-mOlJzi/` and
`run-UVHrGZ/`, both reached the harness's 30-minute merge limit. Both logs stopped
after the 16,600/34,209 progress message; the definition modules from the earlier
investigation had completed. The last live process observation was CPU-bound.
Sampling that process missed its timeout exit, so the retained input was used to
rebuild the exact plan and reproduce the failure in a bounded analysis.

The first structural item after that progress marker is item 16,703,
`history/advisors/00_converter_advisors.txt`. Its two source files total 56,609
lines of repeated advisor blocks. The isolated case timed out at 90 seconds;
all sampled merge-thread stacks were in repeated-sibling matching. Progressive
sampling exposed additional work after each preceding bottleneck was removed:

- Candidate selection rescanned every pair for each pair to find mutual unique
  maxima. Best scores and all tied peers are now accumulated while scoring pairs.
- Ambiguous-node membership repeatedly scanned ambiguity lists and peer lists.
  Membership is now indexed, including rebuilding the indexes after decoding or
  lineage filtering. The serialized ambiguity array is unchanged.
- Descendant recovery searched the whole opposite tree per node, even when a
  parent-scoped anchor required a corresponding parent. It now searches that
  parent's children, or a kind index for nodes without that restriction, retaining
  the existing compatibility checks and cross-parent matches where permitted.
- Join lineage compared the entire input tree for every output-node source.
  Each referenced revision is now validated once per partition; node validity and
  origin checks still run for every node.

The final isolated analysis completed in 28.02 seconds, including 20,581 ms in
the structural file. It reported `partial_success`, no engine or validation
errors, and one withheld file with 2,293 unresolved leaf conflicts. These are
reported ambiguities/conflicts, not independently adjudicated incompatibilities.
No advisor identity rule or arbitrary winner was introduced for performance.

A follow-up selected the 200 consecutive plan items starting at the stuck file.
The structural probe rejected its 13 copy-through items, so the subsequent run
retained all 187 structural items in that window. This completed in 30.01 seconds
(materialization 22,200 ms), producing 143 generated files, one no-op omission and
43 deferred files, with no engine failures or output validation errors. This is
a scoped analysis, not a complete full-page merge, output commit, fixed-cohort
acceptance, or in-game verification. Timings are local observations, not a
controlled benchmark.

Regressions exhaustively compare all 19,683 small candidate relations against the
original mutual-maximum and tie semantics, check dense candidate summaries and
ambiguity serialization/membership, compare indexed descendant candidates with
the unrestricted compatibility relation, and enforce one lineage-tree check per
referenced input at different file sizes while rejecting mismatched trees.
Complete root/CLI package tests, strict workspace Clippy, formatting and diff
checks pass. The probe now prints its child PID and the latest stderr line in
each heartbeat; structural-file logs include path and elapsed time. A heartbeat
indicates process liveness, not completed semantic work.

Evidence is under `target/validation/workshop-stall-2026-09-17/`: `plan.json`,
`before-converter.log`, the successive `*.sample.txt` stacks and timed-out logs,
`after-lineage-converter.log` / `.report.json`,
`history-structural-window.log` / `.report.json`, path-selection files, and
`package-tests.log` / `clippy.log`. Temporary diagnostic sources are retained
there and removed from normal test discovery. The earlier rejected window probe
is retained as `history-window.log`. Changes were uncommitted at this checkpoint
and are included in the 2026-09-19 commits above. P-581 stayed in progress because
a complete `cargo workshop-probe` result with these fixes had not been verified.
Repeating that command builds the repaired executable and reuses the already
installed inputs.

## Definition-module repeated-work audit (2026-09-17)

The timeout hotfix below did not remove all repeated module-wide work. The same
path still scanned every top-level statement for each definition during joins,
lineage normalization and conflict rendering. Trivia detachment cloned complete
subtrees before recursively rebuilding their descendants. Resolution replay also
filtered all selections per definition, and stale-target detection repeatedly
normalized the same vanilla partitions across contributors.

The local implementation now prepares a borrowed definition index once per input
at each processing stage, retaining every same-key statement in source order.
Ancestor seeding, source observation, joins, conflict candidates, final tooltip
projection and stale-target detection use indexed selection. The old unindexed
partition-normalization helper was removed. Replay selections are grouped once;
stale-target detection reuses normalized vanilla partitions and skips matching
when there are no remove-style operations. Trivia detachment rebuilds node fields
directly without first cloning a block's descendants.

Three deterministic regressions exercise lineage/source observation, joins and
conflict previews, and exact source-selection replay. Scaling fixed-size
definitions from 64 to 256 produces exactly four times the instrumented index,
resolution and trivia traversal work. They also verify delta production, candidate
content/order and replay output. These counters protect these traversal boundaries;
they do not prove that all merge algorithms have linear complexity. Existing
duplicate-key, comment canonicalization, file-fallback and provenance tests pass.

The same retained event-modifier case described below was rerun with the same
ordered Workshop inputs and base. Module materialization fell from 32,568 ms to
10,248 ms; total scoped analysis completed in 18.02 seconds (previously 61.17).
These are local observations with cache/environment effects, not a controlled
benchmark. After excluding `definition_module_elapsed_ms`, the complete reports
are identical: `partial_success`, no engine failure, and the same deferred module
and unresolved conflicts. The full-page merge and in-game behavior remain unverified.

Evidence: `target/validation/definition-module-work-2026-09-17/` contains
`after.log`, `after-out.report.json`, the empty `report.diff`, canonical reports,
and `diagnostic.rs` (removed from normal test discovery). Complete root/CLI package
tests passed, including the three work regressions; strict workspace Clippy,
formatting and diff checks passed. Logs are `package-tests.log` and `clippy.log`.
Changes were uncommitted at this checkpoint and are included in the 2026-09-19
commits above. P-581 remained in progress pending the maintainer's complete
`cargo workshop-probe` rerun with the installed inputs.

## Workshop merge timeout and bounded repair (2026-09-16)

The maintainer's full-page run, `target/workshop-probe/runs/run-zJVfOe/`, acquired
and validated all 30 selected inputs. Download took 978,178 ms; the merge child
was killed by the harness's 30-minute limit after 1,800,711 ms. The outer panic
was a timeout assertion, not a Rust panic inside the merge engine. No complete
merge output/report was produced. The last progress was at the definition
modules around `common/estates_preload`, after 2,411 of 34,209 planned paths.

A bounded reproduction retained `common/event_modifiers/00_event_modifiers.txt`
through the public `analyze_merge` API, preserving the original ordered 30 mods,
their installed source paths, and the analyzed base. Scope expansion included
97 database input paths. Before the fix, it did not finish within 120 seconds.
A 3-second macOS sample found 2,287 of 2,455 samples on the merge thread inside
`DefinitionModuleAdapter::normalize_partition -> detach_trivia`: every
definition stripped comments from the entire module before selecting its own
statements, repeatedly traversing and cloning unrelated definitions.

The fix selects the complete same-key definition group first, then strips its
comments before Boolean-OR canonicalization. Existing comment/lineage cases and
a regression for duplicate keys, surrounding definitions, and missing keys pass.
Module-start logging now identifies the active output path, and probe failures
distinguish timeout from process exit and include the exact log paths.

The same bounded release-build analysis completed after the fix in 61.17 seconds,
including 32,568 ms of materialization. This is one local observation, not a
controlled throughput benchmark. Its report is `partial_success`, with no engine
failure and one deferred module containing 12 unresolved leaf conflicts, including
delete/modify disagreements and divergent scalar values. The unit was withheld
as `needs_user_choice`; no arbitrary winner was applied. This was scoped analysis,
not a completed full-page CLI merge or a commit of generated output.

Evidence is under `target/validation/workshop-timeout-2026-09-16/`: `before.log`,
`before.sample.txt`, `after.log`, `after-out.report.json`, `gaps.json`, and
`diagnostic.rs` (the temporary reproducer, removed from normal test discovery).
The full fixed acceptance cohort is unchanged. Retry `cargo workshop-probe` to
use already installed inputs and verify the complete page with the repaired
engine; that complete result remains pending.

Validation: the focused definition-module regressions, complete root/CLI package
tests (`cargo test -p foch -p foch-cli --no-fail-fast`), strict workspace Clippy,
formatting, and diff checks passed. Package tests were run with the local
socket/server permissions they require. Logs are `regression.log`,
`package-tests.log`, and `clippy.log` in the same evidence directory. Changes
were uncommitted at this checkpoint and are included in the 2026-09-19 commits
above.

## Automatic Workshop exploration (2026-09-15)

The local implementation now provides `cargo workshop-probe`, a Rust test-harness
entrypoint for newest-page selection, automatic SteamCMD input preparation,
paired ACF validation, base preparation, actual CLI merge, and gap reporting.
Download preparation now selects the most-recent remembered Steam account with
automatic login enabled (or the sole eligible account); `FOCH_STEAM_ACCOUNT`
is an optional override. It delegates credential reuse to SteamCMD and never
silently defaults to anonymous when account discovery fails. Account selection
is needed only when inputs require downloading.
It reuses frozen selections and installed inputs on subsequent runs, preserves
each attempt's logs/results, bounds subprocess time, and refuses incomplete
inputs. See the [usage guide](../apps/foch-cli/tests/merge_quality/README.md#automatic-newest-page-exploration).
The earlier `download.fish` handoff below is superseded by this complete workflow.

P-603 is fixed locally at the public input-inventory boundary: unavailable
enabled mod roots now cause an error before base loading or output creation.
The CLI regression covers all/partially missing inputs, local paths and Workshop
IDs, analysis and confirmation, base/no-base modes, force, and explicit disabling.
No source mod or game file is changed by that check.

Pipeline fixtures inject acquisition of tiny synthetic Workshop inputs, then use
the real CLI to build/install their base and merge them. They verify preservation
across static/event modifier directories, deterministic repeat output, reuse
without downloading or rebuilding the base, incomplete-download rejection, and
path/reason reporting for unsupported cross-directory duplicate names. Selection
decoding, HTTP fetch/cache reuse, and subprocess timeout behavior are also tested.
These tests establish automation behavior; P-581 remains in progress for complete
real-content merge evidence.

A real attempt of the same ignored test using the development build fetched and
froze all 30 page items, started SteamCMD automatically, and observed a
`No Connection` download error for every item even though SteamCMD exited 0.
Input revalidation correctly failed and merge never started. The unchanged
attempt evidence is `target/workshop-probe/runs/run-K6fbqn/`, including
`download.stdout.log`, `download-result.json`, and `report.json`. The subsequent
diagnostic improvement also extracts per-item SteamCMD failures into structured
download results; it was checked against the observed error format. This is an
input-acquisition failure, not a measured EU4 semantic gap. A live SSR response
also corrected the page decoder to use Steam's `eresult` field.

Follow-up diagnosis of the same attempt traced all 30 failures to Steam's
`content_log.txt`: `BYldRequestDepotManifest` failed to obtain a manifest request
code with `Access Denied`. The matching Workshop log then reported manifest
download failure as `No connection`. The anonymous login succeeded and item
metadata/manifest IDs were returned. The observed blocker is download
authorization for this anonymous session; the outer message alone was misleading.
Relevant timestamped excerpts, with original source line numbers, are preserved
under `target/validation/steamcmd-access-denied-2026-09-15/`. The next check is an
authenticated SteamCMD session using an account with EU4 access and
`FOCH_STEAM_ACCOUNT`; authenticated download success has not yet been verified.

A subsequent bounded check found one remembered, most-recent desktop Steam
account with automatic login enabled. Explicitly selecting that account in
SteamCMD, with `@NoPromptForPassword 1` and only the smallest selected Workshop
item queued, exited 5 with `Cached credentials not found` before downloading.
Desktop login metadata therefore did not provide reusable SteamCMD credentials
in this environment. Logs are under
`target/validation/steamcmd-cached-login-2026-09-15/`; no passwords or tokens were
read or supplied. Initial interactive SteamCMD authentication was required at
that checkpoint.

After the maintainer completed SteamCMD login, the same bounded invocation
reported `Logging in using cached credentials`, completed without password or
Steam Guard interaction, and downloaded item `3801430887` (More Policies,
2,067 bytes). Its descriptor ID matches the selection, and both paired ACF
records agree on manifest `2758251771954978074` and update time `1789364990`.
Logs are under `target/validation/steamcmd-cached-login-after-auth-2026-09-15/`.
This verifies authenticated acquisition of one real input; the full page merge
has not been run. The new automatic account-selection tests cover preference,
explicit override, missing/ambiguous accounts, and disabled automatic login.
The updated probe suite passed 9 tests (2 explicit jobs ignored); logs and
strict workspace Clippy results are under
`target/validation/workshop-account-reuse-2026-09-15/`. Ordinary runs now need
only `cargo workshop-probe` while the selected account's cached login is valid.

Validation: the root/CLI package suite passed apart from the two documented
sandbox socket/server restrictions, and both restricted tests passed when rerun
with those permissions. The final probe suite passed 7 tests (2 explicit jobs
ignored), and strict workspace Clippy, formatting, and diff checks passed.
The original missing-page input was rerun after the fix: exit 1, `blocked`, and
no output directory; see the original probe's `after-fix-merge.log`.
Final probe test logs are in
`target/validation/workshop-probe-automation-2026-09-15/probe-tests.log`.

Local Rust checks use `DEVELOPER_DIR=/Library/Developer/CommandLineTools` because
the selected Xcode installation currently refuses linking until its license is
accepted; no global developer-directory setting or license state was changed.

## Initial newest-page probe (2026-09-15)

The next P-581 probe uses the first EU4 Workshop page sorted by newest
publication (`mostrecent`), directly in page order, to discover concrete
failures. It is an exploratory playset, not a claim that all selected mods are
compatible. Declared dependencies must remain visible; do not silently remove
missing inputs or invent order overrides. The fixed product cohort is unchanged.

The selection is frozen in
[`workshop-recent-page1-2026-09-15.json`](../apps/foch-cli/tests/merge_quality/fixtures/workshop-recent-page1-2026-09-15.json),
including the query URL, collection time, raw-page SHA-256, ranks, IDs, titles,
published/updated times, and advertised file sizes. It contains 30 items totaling
3,129,671,385 advertised bytes (about 3.13 GB, not measured download traffic).
None were installed in the discovered Steam library. Workshop-page metadata is
not installed ACF identity or verified dependency metadata.

Local artifacts are under `target/validation/workshop-recent-page1-2026-09-15/`:

- `source-page.html`, `foch.toml`, and `inspect.log`: frozen source page,
  executable ordered input, and all 30 entries reported as `path=<missing>`.
- `download.txt` and `download.fish`: prepared SteamCMD batch; not executed.
  The maintainer runs the long download manually. Check each item's actual
  download location and its paired ACF before resuming; SteamCMD's exit code
  alone does not prove availability.
- `merge.log`: the normal installed-base attempt continued through analysis
  despite missing sources, then failed at the installed base snapshot lock with
  sandbox `Operation not permitted`. It is not a completed real-content merge.
- `missing-input-repro/foch.toml`, `missing-input-merge.log`, and
  `missing-input-out/.foch/foch-merge-report.json`: bounded reproduction using
  the previously built tiny synthetic base, the same missing Workshop IDs,
  isolated cache/config, and a local Launcher directory. Normal merge with
  `--confirm --non-interactive` exited 0, reported `ready`, generated/copied
  no game files, and reported no unsupported input or engine failure.
- `no-base-merge.log`: `--no-game-base` independently reproduced empty `READY`
  output. This diagnostic does not validate base-aware merge semantics.

This reproduces **P-603**, an input-integrity defect: missing enabled mod roots
are silently omitted from the inventory rather than blocking analysis/export.
`build_mod_candidates_metadata` retains `root_path=None`, and
`build_file_inventory` skipped such candidates. The subsequent local fix and
automatic workflow are described above; these files retain the original failure.
No content-family semantic gap or successful real-Workshop merge is established
by these missing-input runs. P-581 awaits installed inputs and further analysis.
The CLI artifact and paired system Workshop ACF hashes were unchanged after the
probe. JSON selection assertions, the reproduction report assertions, and
`git diff --check` passed; no production source changed.

## Validation recheck (2026-09-14)

Both submodules were present. The following gates passed on `f0d348f`, without
source changes or test exclusions:

```fish
cargo test -p foch --lib database
cargo test --workspace --no-fail-fast
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The focused database suite passed 13 tests. The workspace run passed 1,169 root
library tests (10 ignored), 42 CLI integration tests, 110 corpus-harness tests
(4 ignored), 20 desktop tests, and the remaining integration and documentation
targets. There were no failures. Ignored installed-input and full-cohort tests
were not run.

An additional CLI smoke used a tiny synthetic base reporting `v1.37.5.0`, with
isolated data/config/cache directories and synthetic mods:

- Analysis alone left the requested output directory absent.
- Independent edits to static and event modifiers produced one
  `CStaticModifierDataBase` unit and two output files. The static tax value
  became `0.20`, its unchanged discipline remained `0.05`, and the event morale
  became `0.30`. Generated-output validation reported no parse errors or
  unresolved references.
- Repeating the merge produced byte-identical content in both directories.
- A mod-introduced name repeated across both directories produced
  `unsupported_input`, with neither output file written even under `--force`.
- SHA-256 checks of the synthetic source files were unchanged after all runs.

Local logs, synthetic inputs, installed fixture base, reports, and generated
outputs are under `target/validation/p581-2026-09-14/`. This is automated and
synthetic-input validation, not a new installed-Workshop comparison or accepted
cohort. P-581's real-playset comparison/scoring and manual `cargo acceptance`
remain outstanding; no EU4 runtime test was run.

## Cross-directory database output (2026-09-10)

P-580 makes a database that is fed by several directories one merge unit that
writes one file per contributing directory, and P-591 makes those rules reach a
real installation at all.

`detect_game_version` returns the launcher's `rawVersion` (`v1.37.5.0`) while a
rule snapshot is keyed by the extracted triple (`1.37.5`), so before this change
`load_rules_for_version` never matched on an installed game and every database
rule was inert in production. `normalize_game_version` reduces the former to the
latter. The two changes ship together on purpose: matching alone would have made
`classify_database_entry` defer the whole modifier database on any real playset
touching either directory.

Output is per directory because `replace_path` is declared per directory, the
extractors dispatch on the directory a definition was read from
(`static_modifiers_definition` against `event_modifier_definition`), and EU4
reads both, so disjoint names need no directory order. `MergePlanTarget::Module`
now carries `outputs: Vec<MergeModuleOutput>`, each with its own output path,
namespace prefix and `replace_path` prefix; the persisted plan schema changed
with it. A unit stages every namespace before committing any, so a unit that
fails in its second directory leaves no file from its first.

Files bound to one database share one name-to-object registry, so the same name
in two of its directories is the same game object. What that means for output
depends on how the database registers the repeat: it either composes the two as
different aspects of one object, or the directory read later replaces the one
read earlier. The extracted rules record neither that registration behavior nor
the directory read order, so both are extraction gaps.

The analyzed vanilla snapshot decides it per database at merge time, with no
hardcoded database list. If vanilla itself declares a name in two of the
database's directories, the game composes them, and no repeat there is a
conflict. Measured top-level key overlap in the installed 1.37.5:

| Database | Directories (keys) | Vanilla repeats |
| --- | --- | --- |
| CStaticModifierDataBase | static_modifiers (375) / event_modifiers (3055) | 0 |
| CRulerPersonalityDatabase | ancestor_personalities (41) / ruler_personalities (60) | 0 |
| CCountryDataBase | country_colors (278) / country_tags (974) | 278, all |
| CTradeGoodsDataBase | prices (32) / tradegoods (32) | 32, all |

For the last two the same key names different aspects of one object
(`SWE = "countries/Sweden.txt"` against `SWE = { color1 = ... }`), and vanilla's
own arrangement is the evidence that composing them is correct. Where vanilla
never repeats a name, nothing establishes what a mod-introduced repeat would do,
so it is reported as `unsupported_input` — never `needs_user_choice`, which
would present an engine evidence gap as a gameplay divergence.

Observed on the installed EU4 v1.37.5.0. With two synthetic mods, one editing
`common/static_modifiers` and one `common/event_modifiers` — before: one
`unsupported_input` unit withholding both directories; after: one `safe`
`CStaticModifierDataBase` unit writing
`common/static_modifiers/zzz_foch_static_modifiers.txt` (360 definitions) and
`common/event_modifiers/zzz_foch_event_modifiers.txt` (5420 definitions), each
carrying its own mod's contribution alongside vanilla. A second probe with one
mod adding `SWE` to `common/country_tags` and another adding `SWE` to
`common/country_colors` produced one `safe` `CCountryDataBase` unit writing both
directories and no `unsupported_input`, because vanilla's own 278 repeated tags
establish that the database composes. Source mods were unchanged.

An adversarial review of the first commit confirmed seven defects, all fixed
with regressions:

- only the primary namespace's `replace_path` reached the generated
  `descriptor.mod`. Outputs sort by path, so a reset declared on the later
  directory was dropped and the merged mod overlaid a namespace it had merged
  as replaced;
- the review recorded a unit's planned outputs rather than the files it wrote,
  so a directory whose merge was a vanilla no-op was reported as written;
- an input in a subdirectory failed its whole definition module. EU4 reads the
  directory itself, so such a file now keeps its own per-path handling instead
  of joining, and blocking, the module;
- a database whose directories are not definition modules at all — 1.37.5 has
  one, `interface/state_view` — deferred instead of keeping the per-path merge
  its content family already defines;
- the collision check read the raw file inventory, so a name hidden by a
  `replace_path` reset still counted; it now reads each namespace's merged
  bytes;
- a withheld unit kept the stale-vanilla targets, handler resolutions, warnings
  and dep-misuse adjustments its first namespace staged; and
- a withdrawn unit deleted only its primary output, and the analysis report
  rendered only the primary path.

Validation passed:

```fish
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p foch -p foch-cli --no-fail-fast
```

The full suite passes: 1,169 in the root library, 42 CLI integration, 110 in the
fixed-corpus harness, and the remaining targets, with 0 failures. No full
Workshop acceptance or in-game test was run.

A worktree needs both submodules before its tests mean anything. `git worktree
add` checks out neither, and an absent `vendor/cwtools-eu4-config` fails 13
schema, CWT, script, structured-merge and corpus tests that have nothing
obviously to do with CWT, while an absent `packages/tree-sitter-paradox` breaks
the build outright. Run this once in a new worktree:

```fish
git submodule update --init packages/tree-sitter-paradox vendor/cwtools-eu4-config
```

Two tests additionally need privileges a restricted sandbox may withhold:
`output_transaction_rejects_an_existing_unix_socket` binds a Unix socket and
`data_install_downloads_release_asset_from_manifest` opens a local HTTP server.
Both pass normally; a sandbox denial is an environment result, not a defect.

## Database-rule planning (2026-09-07)

`78c6d7c` adds the PyGhidra extractor and `content/rules/1.37.5.json`.
P-579 makes `path_plan` select that snapshot by resolved game version and group
matching files by database. Retained selections expand to that database's
available inputs. Plan and review IDs use the database name; input paths,
precedence, and vanilla sources remain attached. Snapshot validation follows
the planned unit, including vanilla inputs from another directory.

An ambiguous database match is an error. Unmatched files in rule-covered
families stay separate; other unmatched resources and versions without rules
keep the existing family policies. Base-only units without a participating
namespace reset remain copy-through paths. Reset-only mods still participate
in supported module merges.

Known single-directory module outputs remain supported. At this commit
cross-directory groups still deferred because output required one compatible
descriptor; P-580 above lifted that. The deferral never established separate
game namespaces: the inspected 1.37.5 binary uses the same singleton, loader,
registration, and lookup for static and event modifiers.

The order in which that loader reads the two directories is **not recorded**.
An earlier revision of this page claimed the static directory is read first;
nothing supports it. The extractor collects selections into a `set` and emits
them sorted by directory name
(`tools/eu4-analysis/eu4_analysis/load_rules.py:120`, `:172-183`), so the JSON
array order is a sorted dump, and for `CStaticModifierDataBase` it is
`event_modifiers` then `static_modifiers` — the reverse of the removed claim.
Read order must not be inferred from array position. Trace order does exist in
the extractor (`x86.py`; `catalog.py` and `families.py` both preserve it) and is
discarded by `discover_load_rules`; recovering it is an extraction gap.

Validation passed:

```fish
cargo test -p foch --lib database
cargo test -p foch -p foch-cli
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
git diff --check
```

The focused database regressions passed (11 tests), covering version selection,
directory and filename matching, retained input expansion, cross-directory
vanilla, namespace resets, and planning through materialization/review. Root
library, integration, and CLI suites also passed. No full Workshop acceptance
or in-game test was run.

## Product goal

Foch takes an ordered EU4 playset, preserves contributions whose loader
semantics it understands, produces a deterministic merged mod, and reports real
ambiguity instead of hiding it behind an arbitrary winner.

The current source line is an unreleased EU4-only alpha at `0.0.1`. It is not a
reliable one-click merger for arbitrary modlists. The active product direction
is one root Rust library with a `foch` CLI and a player-facing Tauri desktop
application. The fixed 14-case Workshop cohort remains the product merge-quality
gate; it does not establish desktop product readiness.

The manual product-acceptance entrypoint is `cargo acceptance`. A repository
Cargo alias selects the Rust test orchestrator; it runs the cache and corpus
gates sequentially in separate processes without requiring fish. This entrypoint
change does not establish a new accepted cohort.

## Static-modifier product checkpoint

P-553 reproduced safe output that summed unchanged vanilla values into independent
mod changes. `common/static_modifiers` now uses conflict semantics for divergent
final values; independent field changes, equivalent contributions and explicit
dependency adapters remain automatically mergeable. P-556 also excludes unchanged
carriers from final value and delete/modify candidates while retaining complete
input evidence, the ancestor and real deletion choices.

The shared synthetic matrix exercises public analyze/commit and CLI
preview/confirmation, reparses actual output and checks ordering, provenance,
omission, stable IDs and input immutability. A bounded installed RCE/EE probe also
preserved selected contributions within the same definition. Commands, precise
observations and limitations are in the verification record above. No new full
Workshop cohort or EU4 runtime acceptance is claimed.

## Structural-reset checkpoint

The committed range from `705a854` through `2870816` established the new source
shape:

- the root `foch` package owns shared models plus input, check, graph, simplify,
  merge, and platform behavior;
- reusable CWT machinery lives under `src/game/schema`;
- concrete loader, parser, content-family, base-data, and editor behavior lives
  under `src/game/eu4`;
- the semantic-tree kernel and higher-level merge orchestration live under
  `src/merge`;
- the full merge-output cache was removed and owner-specific caches moved next
  to the input, schema, parser, or merge behavior that defines their identity;
- merge execution is split into complete read-only analysis and guarded commit;
- the desktop frontend has the typed six-command client, input-readiness view,
  analysis progress/cancellation UI, and searchable paginated review browser;
- the CLI is under `apps/foch-cli`, the desktop under `apps/foch-desktop`, and
  the merge-quality harness under `apps/foch-cli/tests/merge_quality`; and
- the superseded `foch-core`, `foch-syntax`, `foch-cwt`, `foch-language`,
  `foch-engine`, `foch-merge-kernel`, and `foch-merge-quality` packages are gone.

Foch remains EU4-only. The reusable schema boundary is preparation for future
game implementations, not a claim that another Paradox game is supported.

## Product-analysis checkpoint

The committed range from `e4b1a9c` through `30aa902` builds on the structural
reset:

- `inspect_current_eu4_input` reads the current EU4 installation, base-data
  identity, Launcher playset descriptors, paired Workshop ACF identities, and
  Workshop descriptors without initializing configuration or cache state;
- the inspected load order, exact game root, Workshop identities, and base
  snapshot lease are frozen into the later `InputRequest`; missing, invalid, or
  path-escaping Launcher descriptors block readiness;
- every planned merge unit now has exactly one stable review outcome: `safe`,
  `copy`, `needs_user_choice`, `unsupported_input`, `engine_failure`, or
  `deferred`;
- `foch merge` displays that complete review before confirmation; the separate
  `merge-plan` command is gone, and `foch input inspect` is the read-only input
  command;
- normal cache open, lookup, and store paths do not prune generations or apply
  maintenance byte caps. Eviction and cleanup happen only through explicit
  `foch cache clean` or `foch cache clear` operations, including the legacy
  parser-cache root;
- the LSP/VS Code public setting is `fochLsp.projectManifest` with environment
  override `FOCH_LSP_PROJECT_MANIFEST`; no compatibility alias remains; and
- the desktop backend implements the six inspection/analysis/query commands.
  The most recent Ready inspection's exact request is atomically bound to its
  analysis ID, while blocked reinspection and queued cancellation discard the
  token.

The exact commits are:

- `e4b1a9c` — exact current-EU4 input inspection;
- `7567dce` — per-unit merge review ledger;
- `d593699` — strict Launcher descriptors for current-input readiness;
- `b8db81a` — desktop merge-analysis commands;
- `f9041db` — CLI, cache, and project-manifest terminology cleanup; and
- `30aa902` — merge-quality harness alignment with review output.

No desktop commit/export command or durable `MergeSession` has been added.
Session design remains deferred.

## Verification through `30aa902`

The current source checkpoint passed:

```fish
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo check --manifest-path fuzz/Cargo.toml --all-targets --all-features
git diff --check
node --check packages/vscode-foch/extension.js
```

The final full test run passed with these principal counts:

- root library: 1,145 passed / 10 ignored;
- merge E2E: 31 passed / 2 ignored;
- CLI library: 36 passed;
- CLI integration: 41 passed;
- fixed-corpus harness: 110 passed / 2 ignored; and
- desktop backend: 16 passed.

Focused input, review-ledger, materialization, defer, LSP, cache, architecture,
and binary-contract tests also passed. Every source commit above passed the
installed pre-commit hook: Rust format, strict workspace clippy, and workspace
test build.

During the final `2870816` commit, the hook's format and strict clippy phases
passed, but its redundant workspace build exhausted local disk space. The
repository's documented emergency `FOCH_SKIP_PRE_COMMIT=1` switch was used only
after the stronger independent gates above had passed. No hook was bypassed
with `--no-verify`.

The full frontend bundle/type/lint/Vitest gates were not rerun because this
clone has no installed `node_modules`, and dependencies were not installed as
part of this source reset. The packaged Windows application and its CI smoke
have not run. The long fixed 14-case Workshop acceptance cohort was also not
run.

## What works at the current checkpoint

### Merge lifecycle

- `foch merge` analyzes the complete semantic result, prints every review unit,
  and leaves the requested output directory untouched until confirmation.
- The analyzed artifact tree, report, input identity, base snapshot, and any
  reviewed prior-output bytes are frozen before confirmation.
- Commit revalidates those guards and atomically installs the frozen bytes; it
  does not run the semantic backend again.
- Replacing a non-empty target requires separate, fingerprinted authorization.
- Safe files or complete definition modules may commit while unsafe units are
  withheld. `partial_success` is a valid product result.
- `--force` applies only to supported `needs_user_choice` fallbacks.

### Inputs and trust

- Playset order and declared dependencies are semantic inputs.
- Source mods and the game installation are read-only.
- Workshop installation identity comes from paired
  `appworkshop_236850.acf` records; normal product work does not recursively
  hash or copy entire Workshop trees.
- The analyzed EU4 base snapshot is the semantic ancestor for supported
  structural merges.
- Desktop analysis consumes the same exact input token that produced the latest
  Ready inspection instead of silently rediscovering the playset or game root.

### Repository products

| Path | Responsibility |
| --- | --- |
| `src/` | Root `foch` library and concrete EU4 implementation |
| `apps/foch-cli` | `foch`, `foch lsp`, CLI integration tests, merge-quality harness |
| `apps/foch-desktop` | Player-facing Tauri application linked directly to `foch` |
| `packages/tree-sitter-paradox` | Independently versioned grammar |
| `packages/vscode-foch` | Independently versioned VS Code extension using `foch lsp` |

## What is not yet proven

- No complete current 14-case product cohort has been accepted. Product quality
  across the fixed denominator is therefore unknown.
- Product acceptance re-parses and semantically scores generated output, but it
  does not launch EU4 or prove in-game playability.
- Games other than EU4 do not have verified loader, content-family, base-data,
  or merge behavior.
- The desktop source implements input inspection and review browsing, but no
  packaged Windows workflow has been verified.

## Measurement records

The V2 JSONL files under `apps/foch-cli/tests/merge_quality/data/` are
append-only and resumable per case. An interrupted cohort is valid measurement
history but not an accepted baseline. Only a complete cohort for the current
product artifact, runner, kernel, scope, and scorer may support a quality
claim.

Do not stage, restore, truncate, delete, or rewrite dirty measurement records
without first establishing their identity and getting the user's decision.
Installed local availability must not shrink the fixed 14-case, 26-item
denominator.

## Execution tracking

Active milestones, issues, dependencies, and acceptance criteria live in the
Linear `foch` project. This file records verified repository state and evidence;
do not reconstruct an execution backlog here.

## Fresh-agent runbook

1. Read this page, [architecture](./architecture.md), and
   [merge design](./merge-design.md).
2. Inspect `git status --short --branch` and `git log -3 --oneline`. Preserve
   unrelated changes and append-only measurement history.
3. Distinguish committed implementation, local worktree observation, recorded
   verification, and accepted product evidence.
4. Check Linear first for the current issue, dependencies, and blockers. Use
   Notion only for project narrative or research context.
5. Run focused tests before workspace gates. Update this page when a verified
   product fact changes and write execution status back to Linear.
6. Never use `--no-verify`, mutate source mods/game files, or claim a cohort
   passed until the supported wrapper validates it.

## Reading order

1. [README](../README.md)
2. [Architecture](./architecture.md)
3. [Merge design](./merge-design.md)
4. [Merge-quality dataset](./merge-quality-dataset.md)
5. [Cache architecture](./cache-architecture.md)
6. [Project manifest](./foch-project-manifest.md)
7. [Resolution DSL](./foch-toml-resolutions.md)
8. [Known issues](../KNOWN_ISSUES.md)

The structured-merge shadow, common-applicability probe, reviews, and research
notes are historical or auxiliary evidence. They are not the active backlog or
current architecture.
