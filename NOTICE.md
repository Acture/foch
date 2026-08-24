# Third-party notice

This crate contains an attributed, parser-independent adaptation of selected
algorithms from Mergiraf 0.18.0:

- upstream repository: <https://codeberg.org/mergiraf/mergiraf>
- upstream revision: `e8e13887b85b8cb56b1dc1624c5f94e3d39182b6`
- upstream license: GPL-3.0-only

The adaptation intentionally excludes Mergiraf's CLI, language registry,
tree-sitter parsers, line-based frontend, and source-format renderer. Derived
source files identify their upstream counterparts in file headers. Ported
tests remain attributed in the test modules that contain them.

foch's original code remains AGPL-3.0-only. The combined work is distributed
under the compatible terms identified in this crate's Cargo metadata; the
upstream GPL text is preserved in `LICENSE-MERGIRAF.txt`.
