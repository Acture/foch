# v2 province features

The deployed weights consume the 29-column plane in
`foch_eu4_state::plane` (five terrain one-hots in columns 22–26, a
binary war-goal flag in column 20) plus the 26-wide rules row. Those
columns stay the inference contract. A mod terrain or casus belli that
is not one of the names the weights saw must not drop the frame.

v2 training replaces each of those categorical channels with

```text
feature = f(modifiers) + E_id
```

`f` is a small map from the category's continuous mechanics to the same
width the rest of the node embedding expects. `E_id` is a residual
embedding looked up by category id. The two terms are added, so a wrong
or missing id cannot erase the mechanic signal.

## Identifiers

- Index `0` is `<UNK>`.
- Every id the table does not contain, including a category created by
  the active playset, uses `0`.
- During training, each example drops its real id to `0` with
  probability `p` where `0.30 <= p <= 0.50`. Dropout is per example, not
  a second embedding row. The mechanic vector is never dropped.
- At inference, dropout is off. Unknown ids still resolve to `0`, and
  `f` still sees that category's own modifiers.

## Terrain

`f` reads the category mechanics from the active playset's
`map/terrain.txt` (game, then mods, honoring `replace_path`):

- `movement_cost`
- `defence`
- `combat_width`
- `supply_limit`
- `attrition`

A missing key is the neutral default already used by the five-bucket
codec (`movement_cost` 1, the others 0, except `supply_limit` 1). The
codec in `plugins/eu4-read-decide/src/terrain.rs` is the compatibility
path: it projects an unknown or mod-defined category onto the nearest
of `plains`, `forest`, `hills`, `mountains`, `marsh` in that normalized
space, and an unresolved name stays an all-zero one-hot. It does not
refuse the frame. Direct hits on those five names, and a file `type =`
that names one of them, keep the column the current weights learned.

`template+0x30` is the terrain object pointer
(`candidates::template::TERRAIN`). The display name is the SSO at
`terrain+0x118`. The five mechanic fields were not pinned on that
object: `movement_cost` has no rip-relative `lea` in the 1.37.5 binary,
because the script reader is generic. Do not invent offsets for them.

## War goals

The same shape replaces the binary `is_wargoal` column and any fixed
list of vanilla casus-belli names. `f` reads the CB's continuous
modifiers (warscore cost, province weight, ticking weight, and the
other numeric keys that CB definition carries). `E_id` is the CB id,
with the same `<UNK> = 0` and the same 30–50% dropout. A mod CB the
embedding has never seen still contributes through `f`. Until those
weights exist, column 20 stays 1 on the goal province and 0 elsewhere.
