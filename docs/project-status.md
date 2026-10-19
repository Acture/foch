# Project Status

Checkpoint: 2026-10-16. Release `v0.0.2` is tagged at `6ac4b28`, including the
stale LSP diagnostics fix `6d48fbe`. Linear owns active work and blockers; this page
records implementation and measured evidence.

## Product state

Foch is an EU4-only alpha. It analyzes an ordered playset, preserves compatible
contributions under verified content-family semantics, and reports ambiguity
for review. It is not a verified one-click merger for arbitrary modlists.

| Module | Implemented behavior | Evidence boundary |
| --- | --- | --- |
| Merge | Freeze inputs, review conflicts, commit safe files or complete definition modules to a separate mod | Bounded cases below; no current accepted full cohort or in-game result |
| Input and paths | Ordered playsets, dependency precedence, validated game-relative paths, paired Workshop ACF identities | Source mods and game files are read-only during merge |
| Parser and schema | Embedded CWT rules, syntax repair or isolation, family-aware structural analysis and numeric output | CWT coverage alone does not establish loader semantics |
| LSP | Diagnostics, completion, hover, navigation, symbols, localisation stubs, syntax repair suggestions and generated-definition provenance | EU4 preview; cross-file findings refresh on save and are withheld for a file whose text differs from its scan |
| Desktop | Direct library integration, input inspection and merge-review browsing | Commit/export and a durable merge session are not implemented; the CLI workflow does not distribute desktop |
| GUI migration | `decisions` mode migrates scripted GUI actions to player decisions | `window` and `mixed` are not implemented; generated gameplay has not been checked in game |
| Runtime tests | Inline annotations and a built-in game runner | Previous native Windows EU4 1.37.5 checks cover isolated per-clause runs; shared sessions, arbitrary dates and hosted Wine CI remain unverified |
| Native plugins | Package/selection management, runtime staging and a Windows host | Previous Windows startup checks cover Unicode and Menu plugins; the host is unpublished; menu/save behavior, Chinese input/rendering and desktop integration remain unverified |

The source includes recent lineage, control-flow, culture, empty-override,
source-ordering and byte-preservation fixes. `common/on_actions` and
`common/estates` stay per-path where the game combines contributions.
Transforms bind their input bytes and commit or withhold complete output groups.
`foch --version` reports the embedded CWT identity; rebuild base data when its
analysis identity no longer matches.

See [merge design](merge-design.md), [architecture](architecture.md),
[known issues](known-issues.md), [LSP preview](lsp-0.1-preview.md),
[runtime tests](foch-runtime-tests.md) and the
[plugin host](../src/packages/foch-eu4-host/README.md).

## Current local quality checks

The 2026-10-16 checks use macOS arm64, EU4 `v1.37.5.0`, the embedded schema,
and two merge workers. Base data, caches and outputs are isolated under
`target/manual-merges/release-0.0.2-20261016/`.

The final candidate completed these real merges after the LSP fix:

| Ordered Workshop input | Result | Elapsed | Output |
| --- | --- | --- | --- |
| `3609019235`, `3609590723` (Ricefarmer Government and Culture & Society) | `ready`; no conflicts, unsupported input or engine failures | 10.40 s | 2 generated files, 22 copied files |
| `2976470733`, `1999055990`, `2164202838`, `2411504869` (Chinese localisation, supplement, Europa Expanded and its localisation) | `partial_success`; 2 conflict units, no unsupported input or engine failures | 112.04 s | 6,680 generated files, 3,317 copied files |

Both outputs re-parsed with zero parse errors and zero fatal errors. The larger
input withholds complete culture and disaster modules: culture identity
correspondence and competing `war_of_the_roses` changes require review.
Item `3715230404` is unavailable locally, so this is not a reproduction of the
earlier five-mod result. Timings are local observations, not an Irony comparison.

An independent tree comparison found all 59 source idea/policy definitions from
the two Ricefarmer packs unchanged in the result, ignoring comments, source
positions and equivalent numeric spellings. All 22 copied files were byte
identical to their selected sources. A separate parser pass checked all 6,841
output scripts/interface files across the two cases with no diagnostics.

Static validation is not clean: the two-pack output reports 702 unresolved
references and 143 missing-localisation findings; the larger output reports
1,359 and 822 respectively. These have not been adjudicated as newly introduced
merge defects. Successful execution and `safe` classification do not establish
semantic accuracy.

The editor schema survey checked 8,158 installed game scripts and emitted 32
diagnostics: 13 scope findings (`V007`), 8 cardinality/repetition findings
(`V002`) and 11 decimal-precision notices (`V008`). This measures diagnostic
volume, not precision or recall. The base build parsed 8,986 documents without
parse failures and installed an isolated matching snapshot.

On 2026-10-18 the semantic index moved to the schema binding the editor uses
(Windows, EU4 `v1.37.5.0`, embedded schema). Checking the installed game's own
scripts as the only playset entry reports no unresolved calls and no calls in
the wrong position, 39 missing localisation keys (from 143) and one invisible
`FROM` (from 740). `build_semantic_index` over the game takes 2.5 s with
60,407 references (from 4.9 s with 33,064). On a 39-mod Workshop playset,
unresolved and misplaced calls rose from 263 to 472: undefined tags, areas and
provinces are now checked against their definitions, and engine keys the
vendored rules lack (`move_great_project`, `every_neighbor_sea_zone`) report
until a supplement covers them. Missing localisation fell from 2,852 to 2,421,
mostly hidden events' titles the rules type as plain text. The editor survey
over the game reports 46 diagnostics: the 32 above and 14 scope findings in
`on_actions`, whose callback scopes now apply and expose vendored alias scopes
narrower than the game's use.

An independent stdio probe found that an unsaved syntax repair cleared the
buffer warning but retained the disk scan's old error. The candidate fixes this
by recomputing document diagnostics from current text, binding cross-file
findings to scanned content, and preserving open buffers on workspace refresh.
Focused regressions and a release-binary protocol probe cover repairs and
subsequent rescans without modifying the source file.

The final candidate passed Rust formatting, strict workspace Clippy and all
2,367 workspace tests (22 explicitly ignored), repository maintenance checks,
grammar tests, VS Code smoke and VSIX packaging. Release preflight passed all
20 checks. See the [compact probe record](../src/apps/foch-cli/tests/merge_quality/data/probes/release-0.0.2-2026-10-16.json)
for artifact identity, inputs and limits. These checks do not launch EU4.

## Product acceptance

The fixed Workshop gate contains **14 cases and 26 unique items**. No complete
cohort for the current product has been accepted. Local availability must not
shrink that denominator. The bounded probes above do not replace it.

Committed complete measurements belong to an earlier executable. Historical
reference-input audits raise reference-eligibility questions; their
`drops_content` counts must not be read as confirmed lost source contributions.
The earlier Irony investigation established no comparative correctness, speed
or usability result.

The maintainer runs the long gate manually:

```fish
cargo acceptance
```

Acceptance re-parses and semantically scores generated output. It does not
launch EU4 or establish in-game playability. See the
[acceptance contract](merge-quality-dataset.md).

## Distribution

[`0.0.2`](https://github.com/Acture/foch/releases/tag/v0.0.2) is published through
GitHub, Cargo, PyPI/uv and Homebrew. The
[release workflow](https://github.com/Acture/foch/actions/runs/38880713227)
and [cross-platform CI](https://github.com/Acture/foch/actions/runs/38880714159)
passed. All six crates were published separately with Cargo after the GitHub
release; the workflow's crates.io job was intentionally disabled.
The [public installation run](https://github.com/Acture/foch/actions/runs/38882638105)
passed Cargo and uv on all three targets and Homebrew on macOS and Linux.
WinGet is not in the community source; local-manifest smoke does not
make `winget install Acture.Foch` available.

SteamCMD acknowledged a successful update of the existing
[Workshop introduction](https://steamcommunity.com/sharedfiles/filedetails/?id=3820682490)
to the `0.0.2` download links. Public-page and API readback timed out, so the
rendered description has not been independently rechecked after this update.

An alpha release establishes an installable artifact; it does not imply an
accepted Workshop cohort. CLI targets
are `linux-x64`, `darwin-arm64` and `win32-x64`. Every channel must report the
release version and its embedded CWT identity. Releases include no EU4 base
data; build it from an installed game.

The release includes the moderate rustls dependency advisory documented in
[known issues](known-issues.md); the dependency update is tracked for a later
release. Offline merge checks do not establish network-library security.

Follow the [release runbook](RELEASE_CHECKLIST.md). The VS Code preview remains
independently versioned at `0.1.0`; its bundled server follows the CLI release.

## Evidence and maintenance

Raw evidence belongs beside its producing code or harness. The CLI harness owns
the [measurement data](../src/apps/foch-cli/tests/merge_quality/data/).
JSONL streams are append-only and resumable. Never stage, truncate or rewrite
pre-existing dirty records without identifying them and obtaining the user's
decision. Local audit files and documentation were preserved during this pull.

`docs/` holds current public guidance. Private research and superseded
development history belong in `notes/`; public builds do not require it.
