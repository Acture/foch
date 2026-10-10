# foch

Foch analyzes an ordered Europa Universalis IV playset and writes a separate,
deterministic merged mod. It keeps the compatible contributions of each mod
and reports genuine conflicts for review instead of silently picking a winner.
Source mods and the game installation are only read. The `foch-cli` crate
builds the `foch` program, including the `foch lsp` language server; the `foch`
crate is the library behind it. `foch-annotation`, `foch-lsp`, `foch-runner`
and `foch-test` are internal libraries of the program, published because
crates.io builds `foch-cli` from published crates only; their Rust APIs carry
no semver promise.

Foch supports Europa Universalis IV only and is alpha software: merging is not
yet reliable across arbitrary modlists, so check a merged mod in game. The
development plugin launcher supports isolated Windows x64 EU4 launches with a
separately built DLL host; see the [plugin deployment commands](https://github.com/Acture/foch/blob/master/src/packages/foch-eu4-host/README.md).
Read the
[current boundary](https://github.com/Acture/foch#current-boundary) before
relying on merge output.

## Install

`foch` is built for Linux x64, macOS arm64 and Windows x64. The
[install section](https://github.com/Acture/foch#install) of the project README
lists the channels a release has been installed and verified from.
`foch --version` prints `foch-cli <version>` and the `cwt-schema` id of the
rules embedded in that release.

The crates.io package `foch` 0.1.0 is an older, unrelated product. If you
installed it, run `cargo uninstall foch` before installing `foch-cli`; both
provide a `foch` executable.

## First run

An installed program is not yet ready to merge. Foch first needs an EU4
base-data snapshot built from your own game installation:

```sh
foch data build eu4 --from-game-path "/path/to/Europa Universalis IV" --game-version auto --install
```

[Build and try it](https://github.com/Acture/foch#build-and-try-it) walks
through that setup and a first merge. Installation says nothing about merge
quality, which the project measures separately; see the
[project status](https://github.com/Acture/foch/blob/master/docs/project-status.md)
and the [documentation](https://github.com/Acture/foch/blob/master/docs/README.md).

## Source and licensing

Foch's own code is AGPL-3.0-only. The program also contains an adaptation of
Mergiraf (GPL-3.0-only) and embeds a rule pack compiled from the CWTools EU4
config (MIT), so the `foch` library and every `foch` program are distributed
as `AGPL-3.0-only AND GPL-3.0-only AND MIT`; the source of the other five
crates is AGPL-3.0-only. Each program also statically links Rust crates under
their own licenses. See
[NOTICE.md](https://github.com/Acture/foch/blob/master/NOTICE.md) and
[THIRD-PARTY-LICENSES.txt](https://github.com/Acture/foch/blob/master/THIRD-PARTY-LICENSES.txt).

The source of each release is tag `v<version>` of
<https://github.com/Acture/foch> with its two build submodules,
`src/packages/tree-sitter-paradox` and
`src/packages/foch/vendor/cwtools-eu4-config`. It is published as
`foch-<version>-source.tar.gz` on that tag's
[GitHub release](https://github.com/Acture/foch/releases); GitHub's automatic
source downloads leave out the submodules.
