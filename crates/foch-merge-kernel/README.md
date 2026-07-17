# foch-merge-kernel

Parser-independent structured merge primitives for foch.

The crate owns normalized trees, semantic anchors, tree correspondence, and
structural amalgamation. It does not know about Paradox game layouts, mod
dependency graphs, output paths, or source formatting. Those remain in
`foch-engine` and `foch-language`.

Parts of the matching and amalgamation algorithms are adapted from Mergiraf
0.18.0. See [`NOTICE.md`](NOTICE.md) and
[`LICENSE-MERGIRAF.txt`](LICENSE-MERGIRAF.txt).
