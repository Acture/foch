# Merge Quality AST Diff Snapshot

Date: 2026-07-06

Scope: committed `foch-merge-quality` corpus, 6 compatch cases, 35 overlapping
ground-truth files. The comparison policy is the current corpus scorer:

- GUI/GFX files: order-sensitive AST comparison.
- Non-GUI Clausewitz files: order-insensitive AST comparison.
- Comments and spans are ignored.
- Bare identifiers and quoted strings with the same valid identifier text are equivalent.

Current result:

- `matches_human`: 2/35
- `diverges_ast`: 33/35
- `conflict_withheld`: 0
- `not_emitted`: 0
- `drops_content`: 0
- `diverges_structure`: 0

The two AST matches are:

- `3630876155 common/rebel_types/ita_monarchist.txt`
- `3630876155 common/rebel_types/ita_republicans.txt`

## Divergence Categories

| category | count | meaning |
|---|---:|---|
| `gui_order_only` | 8 | Same leaf multiset, but GUI/GFX sibling order differs. These are not counted as AST matches because GUI order can affect layout or rendering. |
| `value_and_content` | 24 | Both scalar values and leaf inventory differ. This is real semantic divergence, not pretty-print noise. |
| `content_mismatch` | 1 | Leaf inventory differs without simple same-path scalar diffs in the diagnostic sample. |

## Recurring Patterns

1. GUI order-only files are a major bucket, but not the majority. These need a GUI-specific ordering/layout policy, not a global order-insensitive pass.
2. `interface/frontend.gui` recurs as `value_and_content` across most cases. Typical differences are `dlc_icon_bg_empty` vs `dlc_icon_bg`, `gfx_emptyness` vs `GFX_dlc_icon_even_bg_flip`, doubled escaping in `textureFile`, and missing/extra `if_resolution` values.
3. `interface/subscription_banner_view.gui` recurs as `value_and_content`. Foch keeps animated offset values (`1000`, `3000`, `5000`, etc.) while humans often keep static offsets (`0`, `75`) and different `Orientation`.
4. `_expanded_family_scripted_triggers.txt` recurs as a nested-`OR` problem. Foch emits `OR/OR/has_global_flag`, while the human compatch keeps the two flags as flat `OR/has_global_flag` entries.
5. Non-GUI gameplay files often reflect human policy choices, not formatter differences: taking one side, adding tooltips, changing values, or preserving manually curated lists.

## Manual Inspection Notes

These are from opening the generated foch output, both source mods, and the
human compatch for case `3630876155`.

### `common/scripted_triggers/_expanded_family_scripted_triggers.txt`

One source mod and the human compatch define `is_expanded_mod_active` as a flat
`OR` containing the two global flags. The other source mod defines the same
trigger as a single `has_global_flag = $mod$_expanded_mod_active`. Foch wraps
both sides mechanically and emits an `OR` containing a nested `OR` plus the
duplicate flag.

Judgment: this is a foch correctness bug, not formatting. `BooleanOr` should
flatten an incoming `OR` body and deduplicate identical child predicates.

### `interface/000_expanded_mod_family.gfx`

For this case, foch and the human compatch contain the same sprite leaves, but
the sibling order is different. The scalar values and sprite inventory match.

Judgment: this specific file is an order-only difference. The current scorer is
intentionally strict for all GUI/GFX files, but `.gfx` `spriteTypes` order may
be safe to normalize separately from `.gui` widget order.

### `interface/frontend.gui`

The human compatch keeps an explicitly renamed widget:
`name = "dlc_icon_bg_empty"` with `spriteType = "gfx_emptyness"`. Foch keeps the
same-name widget as `name = "dlc_icon_bg"` with
`spriteType = "GFX_dlc_icon_even_bg_flip"`. The source mods both have a
`dlc_icon_bg` child, so the current same-name merge key collapses a human split
into a single widget.

The same file also shows `textureFile` backslashes emitted as doubled escaping
in foch output compared with the human/source strings.

Judgment: this is a real GUI merge problem. Same-name GUI children sometimes
need to be treated as colliding variants rather than one matched node. The
backslash behavior also looks like a parser/emitter escaping bug.

### `interface/subscription_banner_view.gui`

The human compatch keeps `open_subscription_view_button_offset`, static
positions such as `x = 0` and `x = 75`, and `Orientation = "LOWER_LEFT"` in the
relevant branch. Foch keeps the animated offset values from the source variants
(`1000`, `3000`, `5000`, etc.) and merges both orientation/layout variants.

Judgment: this is not pretty-print drift. The human compatch applies a layout
policy choice, effectively choosing a collapsed/static banner shape. A generic
recursive union will not reproduce this without GUI-specific policy.

## Per-File Findings

| case | path | human relation | category | representative difference |
|---|---|---|---|---|
| `3630876155` | `common/scripted_triggers/_expanded_family_scripted_triggers.txt` | `subset/took_base` | `value_and_content` | Foch nests the expanded-family flags under `OR/OR`; human keeps both flags directly under `OR`. |
| `3630876155` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `gui_order_only` | Same 40 leaves; only GUI/GFX sibling order differs. |
| `3630876155` | `interface/countrycourtview.gui` | `disjoint/union` | `gui_order_only` | Same 4418 leaves; only GUI sibling order differs. |
| `3630876155` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | `dlc_icon_bg_empty`/`gfx_emptyness` missing on human side of the intended merge; foch keeps `dlc_icon_bg`/`GFX_dlc_icon_even_bg_flip`; `textureFile` escaping also differs. |
| `3630876155` | `interface/subscription_banner_view.gui` | `disjoint/took_overlay` | `value_and_content` | Human keeps `open_subscription_view_button_offset`, mostly `x = 0/75`, and `Orientation = LOWER_LEFT`; foch keeps animated offsets and `LOWER_RIGHT`. |
| `3630904821` | `common/scripted_triggers/_expanded_family_scripted_triggers.txt` | `subset/took_base` | `value_and_content` | Same nested-`OR` problem as above. |
| `3630904821` | `decisions/PragmaticSanction.txt` | `redundant/union` | `content_mismatch` | Human wraps conditions in `allow/OR/custom_trigger_tooltip`; foch keeps direct `has_female_heir`, `imperial_influence`, and effect cost leaves. |
| `3630904821` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `gui_order_only` | Same 40 leaves; only order differs. |
| `3630904821` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | Same recurring frontend icon/sprite/texture/`if_resolution` differences. |
| `3630904821` | `interface/hre.gui` | `redundant/union` | `gui_order_only` | Same 1041 leaves; only order differs. |
| `3630904821` | `interface/subscription_banner_view.gui` | `disjoint/took_overlay` | `value_and_content` | Same recurring subscription banner offset/orientation mismatch. |
| `3630934157` | `common/religions/00_religion.txt` | `redundant/union` | `value_and_content` | Human has additional Bogomilist/Jainism/aspect content; foch keeps different Coptic color values and extra `BOGOMILIST` Catholic heretic entry. |
| `3630934157` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `gui_order_only` | Same 40 leaves; only order differs. |
| `3630934157` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | Same recurring frontend icon/sprite/texture/`if_resolution` differences. |
| `3630934157` | `interface/provinceview.gui` | `redundant/union` | `value_and_content` | Human has one extra `building_close_button` group (`clicksound`, `name`, `position x=304`, `position y=120`). |
| `3630934157` | `interface/subscription_banner_view.gui` | `disjoint/took_base` | `value_and_content` | Same recurring subscription banner offset/orientation mismatch. |
| `3630934157` | `interface/topbar.gui` | `disjoint/union` | `gui_order_only` | Same 3469 leaves; only GUI sibling order differs. |
| `3634824708` | `common/buildings/00_buildings.txt` | `redundant/union` | `value_and_content` | Human adds cathedral province modifiers (`EE_FRA_power_steam_tax`, `me_pap_buffed_churches`) and mission limits; foch also has extra shipyard `ai_will_do` leaves. |
| `3634824708` | `common/tradegoods/00_tradegoods.txt` | `disjoint/took_overlay` | `value_and_content` | `cloves` trade power is `0.1` vs human `0.15`; human has additional fur/slaves conditions and regions. |
| `3634824708` | `events/PriceChanges.txt` | `redundant/union` | `value_and_content` | Human keeps `felt_hats_happened`, `copper = 1`, and conditional year structure; foch has direct duplicate `is_year = 1600`. |
| `3634824708` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `value_and_content` | Human has the `religions_and_cultures_expanded` sprite/effect/texture; foch misses that sprite. |
| `3634824708` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | Same recurring frontend icon/sprite/texture differences, with lower leaf count on foch side. |
| `3634829839` | `common/ages/00_default.txt` | `redundant/union` | `value_and_content` | `ab_portugal_colonial_growth` is `85` vs human `50`; foch keeps extra `obj_two_unions` subject conditions. |
| `3634829839` | `common/scripted_triggers/_expanded_family_scripted_triggers.txt` | `subset/took_overlay` | `value_and_content` | Same nested-`OR` problem as above. |
| `3634829839` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `gui_order_only` | Same 40 leaves; only order differs. |
| `3634829839` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | Same recurring frontend icon/texture/`if_resolution` differences. |
| `3634829839` | `interface/subscription_banner_view.gui` | `disjoint/took_base` | `value_and_content` | Same recurring subscription banner offset/orientation mismatch. |
| `3635635014` | `common/governments/00_governments.txt` | `subset/took_base` | `value_and_content` | Human has many additional reform-list entries across `exclusive_reforms`, `absolute_rule_vs_constitutional`, `bureaucracy`, and `deliberative_assembly`; foch preserves much smaller lists. |
| `3635635014` | `common/scripted_triggers/_expanded_family_scripted_triggers.txt` | `subset/took_base` | `value_and_content` | Same nested-`OR` problem as above. |
| `3635635014` | `events/Elections.txt` | `redundant/union` | `value_and_content` | Foch duplicates `post_ruler_focus_clearance` and keeps extra election-term desc/trigger leaves; human has different `dutch_republic` and NED hidden-effect logic. |
| `3635635014` | `interface/000_expanded_mod_family.gfx` | `redundant/union` | `gui_order_only` | Same 40 leaves; only order differs. |
| `3635635014` | `interface/frontend.gui` | `redundant/union` | `value_and_content` | Same recurring frontend icon/sprite/texture/`if_resolution` differences. |
| `3635635014` | `interface/subscription_banner_view.gui` | `disjoint/took_overlay` | `value_and_content` | Same recurring subscription banner offset/orientation mismatch. |

## Immediate Implications

- A pretty-printer alone cannot move the 33 `diverges_ast` files to match. Only 8/33 are pure GUI ordering.
- The highest-leverage correctness fixes are:
  1. Flatten BooleanOr output when a contributor body is already an `OR`.
  2. Improve GUI named-child matching so recurring frontend/subscription-banner widgets do not choose the wrong sibling or scalar source.
  3. Treat several gameplay roots as policy-sensitive rather than blindly unioning: `decisions`, `religions`, `buildings`, `tradegoods`, `ages`, `governments`, and `events`.
  4. Add targeted corpus assertions for recurring files instead of relying only on aggregate verdict counts.
