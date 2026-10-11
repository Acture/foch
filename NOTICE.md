# Third-party notice

The `foch` library crate, and every binary built from it, contains an
attributed, parser-independent adaptation of selected algorithms from Mergiraf
0.18.0:

- upstream repository: <https://codeberg.org/mergiraf/mergiraf>
- upstream revision: `e8e13887b85b8cb56b1dc1624c5f94e3d39182b6`
- upstream license: GPL-3.0-only

The adaptation intentionally excludes Mergiraf's CLI, language registry,
tree-sitter parsers, line-based frontend, and source-format renderer. Derived
source files identify their upstream counterparts in file headers. Ported
tests remain attributed in the test modules that contain them.

foch's original code remains AGPL-3.0-only. The combined work is distributed
under the compatible terms identified in the `foch` crate's Cargo metadata; the
upstream GPL text is preserved in `LICENSE-MERGIRAF.txt`.

## CWTools EU4 config

foch binaries embed a rule pack compiled from the CWTools EU4 config:

- upstream repository: <https://github.com/cwtools/cwtools-eu4-config>
- vendored in the `foch` library package as `vendor/cwtools-eu4-config`
  (the `src/packages/foch/vendor/cwtools-eu4-config` submodule of the
  repository)
- upstream license: MIT, Copyright (c) 2018 tboby; the full text is `LICENSE`
  in that directory

## Binary distributions

`AGPL-3.0-only AND GPL-3.0-only AND MIT` covers Foch's own code, the Mergiraf
adaptation and the CWTools rule pack, which every distributed `foch` program
combines. The PyPI wheels, the WinGet manifest and the Homebrew formula declare
that expression, as the `foch` crate does. The own source of the `foch-cli`,
`foch-annotation`, `foch-lsp`, `foch-runner` and `foch-test` crates is
AGPL-3.0-only; they build against `foch`.

Each program also statically links the Rust crates of its dependency graph,
each under its own license. `THIRD-PARTY-LICENSES.txt` gives every one of
those license texts with the crates that use it.

Each release archive carries `LICENSE`, `LICENSE-MERGIRAF.txt`, this notice,
`THIRD-PARTY-LICENSES.txt` and the CWTools license as
`LICENSE-cwtools-eu4-config.txt` next to the executable. Each wheel carries the
same texts under `.dist-info/licenses/`, the CWTools one as
`src/packages/foch/vendor/cwtools-eu4-config/LICENSE`.

## Source code

The Corresponding Source of `foch` release `<version>`, which `foch --version`
reports, is tag `v<version>` of <https://github.com/Acture/foch> together with
its two build submodules, `src/packages/tree-sitter-paradox` and
`src/packages/foch/vendor/cwtools-eu4-config`. It is published as
`foch-<version>-source.tar.gz` on that tag's GitHub release. GitHub's automatic
"Source code" downloads leave out the submodules.
