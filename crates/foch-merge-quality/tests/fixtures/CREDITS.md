# Merge-quality fixtures — credits & provenance

These fixtures are **minimal excerpts** of community Europa Universalis IV
Steam Workshop mods, used as ground truth for measuring `foch`'s merge quality.
For each compatibility patch ("compatch") we keep only the handful of files the
scorer actually compares — the files present in *both* patched mods that the
compatch hand-merged — in three slices:

- `a/`, `b/` — the two patched mods (the merge inputs);
- `compatch/` — the community compatch (the human-authored merge, our ground truth).

We do **not** redistribute the full mods. Each slice is a small subset of files
kept solely so the merge-quality test (`tests/scoring.rs`) is reproducible
offline without a multi-gigabyte Steam Workshop download.

## Attribution

All content belongs to its original authors. Each item is linked to its Steam
Workshop page, where the author is credited:

| Compatch | Patched mod A | Patched mod B |
| --- | --- | --- |
| [`3630876155` — FEE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3630876155) | [`2164202838` — Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) | [`2185445645` — Flavour and Events Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2185445645) |

Each `descriptor.mod` in `a/`/`b/` retains the mod's own name and supported
game version, preserving authorship metadata.

## Takedown

These excerpts are included in good faith for research and interoperability
analysis. **If you are an author and would like your content removed, open an
issue or contact the maintainer and we will remove it promptly** — the
merge-quality test will simply skip any fixture that is absent.
