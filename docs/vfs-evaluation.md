# VFS dependency evaluation

Evaluated 2026-09-26 against Foch `8208defa45af42f5f9b509f6668748a49bd0c4e2`,
on macOS Apple Silicon. This is a bounded dependency evaluation, not a Workshop
merge result or a performance benchmark.

Execution record: P-735 (evaluation complete); P-736 tracks the separate path
identity defect and the selected path-type migration, which remains unimplemented.

## Recommendation

Keep `std::fs` and `walkdir` for the current installed-directory input path.
Do not migrate that path wholesale to `vfs` or PhysicsFS now. Neither candidate
removes the input identity, ordered contributor inventory, snapshot validation,
or EU4 visibility code. Adopting `vfs` also requires restoring the current
no-symlink and deterministic enumeration behavior; PhysicsFS adds native linking
and a process-global mount context.

This is not a finding that Foch's input code is uniformly better. The evaluation
reproduced a collision in Foch's physical-path-to-semantic-key conversion. Fix
that boundary and consolidate its duplicated conversions. A VFS dependency alone
does not fix the collision if the same conversion remains in front of it.

Reconsider PhysicsFS when reading archived mods without extraction is an actual
product requirement. Its ZIP support was exercised successfully. Reconsider the
Rust `vfs` abstraction when multiple interchangeable storage implementations are
needed. Retaining per-layer handles successfully preserved all contributors, so
provenance is **not** a reason to reject reuse.

## Method and limits

- Compiled and ran `vfs` **0.13.0**, `walkdir` **2.5.0**, and PhysicsFS
  **3.2.0** from its upstream release archive. PhysicsFS was linked directly
  through a small evaluation-only Rust FFI; no third-party Rust binding was
  evaluated.
- The Foch side executes verbatim extracts of `collect_relative_files`,
  `collect_relative_files_from_entries`, and `normalize_relative_path` from
  `src/input/resolve.rs`. Its fixture filter accepts `common/`; this does not
  execute Foch's full input resolver, cache, or merge pipeline.
- Three synthetic source directories contain overlapping files, unique files,
  an empty upper directory, and a symlink to a fixture outside the source root.
  Additional fixtures exercise filename collisions and a ZIP archive.
- Both ordered overlays read the highest-precedence file. Raw lower-layer
  visibility is tested separately from Foch's EU4-specific visibility rules.
- No game installation or Workshop source was accessed. Source fixture file
  paths and bytes were equal before and after the read probes.
- No Windows/Linux runtime test, throughput comparison, full replacement
  implementation, or product acceptance run was performed.

## Observations

| Case | Foch helper / current design | Rust `vfs` | PhysicsFS |
| --- | --- | --- | --- |
| Same-path priority and directory union | Ordered inventory remains custom; not a full resolver run | Read `b`; union contains unique files from all layers | Read `b`; union contains unique files from all layers |
| Access every contributor | Current inventory stores a vector per path | Retained physical-layer handles read `base`, `a`, `b` | `getRealDir` identifies the winner; duplicate mounts did not create separate source views |
| Independent playsets | Request-owned inventory in source | Two simultaneous overlay objects independently returned `a` and `b` | Source review: `searchPath` and `writeDir` are process-global; not a claim that concurrent reads are unsafe |
| Symlink policy | Extracted walker excludes symlink | `PhysicalFS` read the external fixture target | Default mount behavior excluded the link and refused lookup |
| Enumeration determinism | Walker sorts collected paths | 16 calls produced 16 distinct raw enumeration orders; caller must sort | No ordering comparison made |
| Missing root | Walker returns an error | `read_dir` returns an error | Not tested |
| Error during iteration | Injected permission error propagated by extracted helper | Source review: `PhysicalFS::read_dir` unwraps entry and UTF-8 conversion errors | Not tested |
| Directory reset | Foch retains its `replace_path` rules | Empty upper directory still exposes lower `old.txt` | Same; needs an EU4 adapter |
| ZIP read without extraction | Current descriptor/input path has no archive backend | Not tested; no archive backend in the evaluated built-in list | Mounted a generated ZIP and read its contents successfully |
| Literal backslash versus separator | Two actual files mapped to one semantic key | Not a replacement for Foch's own key conversion | Same boundary remains Foch's responsibility |
| Non-UTF-8 names | Two distinct in-memory `PathBuf`s collapse through `to_string_lossy` | Disk panic probe could not run: this environment rejected fixture creation with `EPERM` | Not tested |

Library behavior is not automatically an application defect. Unsorted directory
enumeration and following symlinks are usable policies elsewhere; they require
adaptation to Foch's current contracts. An empty upper directory is not intended
as an implementation of an EU4 `replace_path` directive in either library.

PhysicsFS duplicate-mount observation: mounting the same physical source string
again at `sources/0`, `sources/1`, or `sources/2` returned success but opening
those source views failed. Its `doMount` implementation returns early when the
source string is already in the search path. Other access strategies are
possible; the straightforward per-playset/per-source mount scheme needs work.

## Concrete Foch finding

The physical fixture contained both:

```text
common/a/b.txt      -> nested file
common/a\b.txt      -> literal backslash in a macOS filename
```

The current walker returned both. `normalize_relative_path` converted both to
`common/a/b.txt`. `build_file_inventory` uses that normalized string as its key.
Separately, `common/\xff.txt` and `common/\xfe.txt` constructed as Unix byte paths
both convert to the same replacement-character string. The latter was verified
in memory only because this environment refused the non-UTF-8 disk filename.

This is a reproduced helper-level identity collision, not evidence of an
observed ordinary Workshop merge failure or proof that EU4 accepts those names.
The input boundary should reject unrepresentable/ambiguous physical names with
a diagnostic, or use a verified injective representation. It should not silently
assign distinct physical files the same semantic identity.

Affected conversion sites include `src/input/resolve.rs`, `file_filter.rs`,
`scripts.rs`, `mod_snapshot/mod.rs`, and `mod_snapshot/store.rs`. Imported
descriptor strings and discovered OS filenames need distinct treatment:
normalizing a Windows-style descriptor does not justify rewriting a literal
backslash found by a Unix filesystem walk.

## What a migration would actually replace

| Boundary | Current code | What survives migration |
| --- | --- | --- |
| Directory enumeration | `resolve.rs:1312–1376`, 65 physical lines including spacing, already using `walkdir` | Content-root pruning, filters, link policy, error handling, stable ordering |
| Reading script bytes | `scripts.rs::LazyScriptFile::load_verified`, a `std::fs::read` call | Size/digest verification, input identity, parse cache, semantic path |
| Contributor inventory | `resolve.rs::build_file_inventory` | Mod IDs, precedence, vanilla ancestor and snapshot metadata |
| EU4 visibility and definitions | `merge/planning/dag.rs`, `module_view.rs`, `merge/path_plan.rs` | Dependency ancestry, `replace_path`, database grouping and definition registration |

`vfs::walk_dir` provides traversal but its generic metadata has no symlink kind
and its iterator exposes no equivalent of `walkdir::filter_entry` for pruning
before descent. Preserving current policies requires a physical adapter or
custom traversal. Merely replacing `fs::read` with `open_file` plus `read_to_end`
does not remove the surrounding input subsystem.

The table is a source-level replacement boundary assessment, not a measured
net-lines-saved result from an implemented migration. It establishes that most
current complexity remains and identifies the adapters that a trial would add.

## Evidence and reproduction

The self-contained observations above are the durable record. Local probe
source, pinned Cargo lockfile, PhysicsFS release archive, JSONL output and logs
are under `target/validation/vfs-2026-09-26/` (ignored build/evaluation material,
not shipped product code). The source extracts are intentionally frozen to the
evaluated commit. On this macOS setup, while those local artifacts remain:

```fish
fish target/validation/vfs-2026-09-26/run.fish
```

The probe passed strict Clippy and formatting. Its final process exited zero;
the JSONL records include the skipped non-UTF-8 disk case explicitly. An initial
launch failed to locate the locally built PhysicsFS dylib; setting the probe's
`DYLD_LIBRARY_PATH` resolved that build-environment issue. This was not a library
functional failure. No Foch product dependencies or production code changed.

Primary sources:

- [vfs 0.13.0 API](https://docs.rs/vfs/0.13.0/vfs/)
- [vfs PhysicalFS source](https://docs.rs/vfs/0.13.0/src/vfs/impls/physical.rs.html)
- [vfs OverlayFS source](https://docs.rs/vfs/0.13.0/src/vfs/impls/overlay.rs.html)
- [PhysicsFS 3.2.0 release](https://github.com/icculus/physfs/releases/tag/release-3.2.0)
- [PhysicsFS 3.2.0 implementation](https://github.com/icculus/physfs/blob/release-3.2.0/src/physfs.c)

## Relative-path type follow-up (2026-09-28)

A separate macOS probe compared `relative-path` 2.0.1, `typed-path` 0.12.3,
and the existing lockfile's `camino` 1.2.5. P-736 now specifies native
`Path/PathBuf` for physical I/O and `relative-path`'s
`RelativePath/RelativePathBuf` for portable game-root-relative identities.
Requiring native `PathBuf` for every logical path was unnecessarily restrictive:
native paths have host OS semantics and do not encode the distinction between
physical and logical paths.
These are path value types, not VFS backends; this does not reverse the decision
against wholesale VFS replacement. No product dependency was added.

Observed behavior:

- `RelativePathBuf::from_path` preserved native relative components, kept the
  literal-backslash fixture distinct from the nested path on macOS, and rejected
  native absolute and non-UTF-8 paths. Resolving against an explicit root and
  Serde round trips worked.
- `RelativePath::new` and Serde do not enforce Foch's stricter rooted namespace:
  `../outside` is accepted, and a raw leading slash is accepted and discarded
  when `to_path` attaches the supplied root. Validate external logical paths;
  the type name is not a guarantee that a path stays beneath a game root.
- `typed-path` parsed explicitly Windows-style relative text on macOS, and its
  checked join rejected absolute replacement and an escaping parent. It is a
  candidate for foreign descriptor parsing, not needed for ordinary native I/O.
- A first probe assertion expecting checked Unix-to-Windows conversion to reject
  a literal backslash failed. Inspection of the actual result showed both
  `common/a\b.txt` and `common/a/b.txt` became the same Windows path. The final
  probe records this limitation rather than assuming that “checked” preserves
  component identity. Its documented invalid-pipe case was rejected as expected.
- `camino::Utf8Path` accepted absolute paths and retained host separator
  semantics. UTF-8 alone does not provide a portable relative-path model.

Foch still needs a narrow validation boundary for a game-root-relative namespace
and portable filenames, including after deserialization and before host-path
conversion. A mature path type can internally store UTF-8; the requirement is to
stop manipulating bare strings as paths, not to prohibit that representation
inside a library that supplies path semantics.

The standalone probe finished with exit zero and passed strict Clippy and
formatting. Its source, lockfile, initial failed assertion and final output are
in `target/validation/relative-path-2026-09-28/`. This is a type/API check on
macOS, not a completed product migration or cross-platform acceptance result.
Primary API references: [relative-path](https://docs.rs/relative-path/latest/relative_path/),
[typed-path](https://docs.rs/typed-path/latest/typed_path/),
[camino](https://docs.rs/camino/latest/camino/).
