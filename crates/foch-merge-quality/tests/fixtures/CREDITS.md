# Merge-quality fixtures — credits & provenance

These fixtures are full local copies of selected community Europa Universalis IV
Steam Workshop mod directories, used as provisional reference evidence for
measuring `foch`'s merge quality. Full context is intentional: `foch merge` can make
workspace-sensitive decisions, so reduced file slices are not an authoritative
test corpus.

The archive layout is:

- `corpus.json` — the selected compatch cases;
- `workshop/<steam_id>/` — the full local candidate or referenced-mod directory.

Version-control metadata and platform junk such as `.git/` and `.DS_Store` are
excluded because they are not playable mod content and are not consumed by
`foch`.

## Attribution

All content belongs to its original authors. Each item is linked to its Steam
Workshop page, where the author is credited:

| Compatch candidate | Referenced mod A | Referenced mod B |
| --- | --- | --- |
| [`3630876155` FEE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3630876155) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) | [`2185445645` Flavour and Events Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2185445645) |
| [`3630904821` HREE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3630904821) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) | [`1352521684` Holy Roman Empire Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=1352521684) |
| [`3630934157` RCE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3630934157) | [`3342969370` Religions and Cultures Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=3342969370) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) |
| [`3634824708` TGE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3634824708) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) | [`1770950522` Trade Goods Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=1770950522) |
| [`3634829839` ASE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3634829839) | [`2172666098` Ages and Splendor Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2172666098) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) |
| [`3635635014` GE - EE Compatch](https://steamcommunity.com/sharedfiles/filedetails/?id=3635635014) | [`2164202838` Europa Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=2164202838) | [`1596815683` Governments Expanded](https://steamcommunity.com/sharedfiles/filedetails/?id=1596815683) |

Each retained `descriptor.mod` keeps the mod's own name and supported game
version, preserving authorship metadata.

## Takedown

These fixtures are included in good faith for research and interoperability
analysis. **If you are an author and would like your content removed, open an
issue or contact the maintainer and we will remove it promptly** — the
merge-quality test will simply skip any fixture that is absent.
