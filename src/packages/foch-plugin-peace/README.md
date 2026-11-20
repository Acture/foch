# Bilateral peace window

`foch-plugin-peace` (`io.github.acture.foch-peace`) is an experimental,
unpublished Windows x64 native plugin for Europa Universalis IV `1.37.5`. It
keeps both the native demand and concession drafts alive across tab switches,
previews the combined bilateral deal in the peace window, and submits mixed
demands and concessions as a single merged, net-priced offer.

## Features

- **Bilateral peace interface**: Retains the native demand draft (`view+0x48`)
  and concession draft (`view+0x208`) simultaneously while the two direction
  tabs (`view+0x660`) switch which side the player is editing.
- **Non-English UI and localization compatibility**: Preserves the game's
  rendered clause bytes, encoding, colour escapes, and any UI-mod or
  non-English localization. The summary prepends only a numeric net line
  (`+A.B% -C.D% = +E.F%`) and wraps the two sides under `(+)` and `(-)`
  markers, without injecting English headings. Tab titles keep their localized
  text and append ` (+)` or ` (-)` idempotently. While both drafts carry
  terms, the summary hook also writes the net deal value into the active
  draft's cached total (`+0xf0`) and updates the `offer_score` readout beside
  the send button (leaving the top `warscore` progress widget native); a
  single-direction selection keeps its vanilla display.
- **Merged and signed net-value offers**: A native peace action carries one
  embedded offer (`+0x48`). Submitting with terms on both sides snapshots the
  concession draft, deep-clones the inactive side's selected clauses into the
  active offer, and nets mirrored ducats (Kind 0) between the same parties into
  one net amount while zeroing the smaller twin (which the native Kind 0
  executor skips). Give-side clauses are repriced through the native signed
  cost accumulator (`vtable+0x118`) so the merged offer's cached total becomes
  demands minus concessions. After the original send callback copies the merged
  offer into the peace action, netted ducat amounts and the editor's active
  selection are restored. Cloned clauses remain owned by the editor offer's
  owned vector (`+0x88`) until its native destructor runs, so nothing is freed
  twice.
- **`0x978b20` cost rebuild hook preserving net value**: The native cost
  rebuild at `0x978b20` calls each selected clause's `vtable+0x118` accumulator
  with `negate = 0` and the offer's direction byte (`+0x20`), overwriting the
  cached total at `+0xf0` (inside the totals struct at `+0xe8`) with an
  all-positive sum. Its first 15 bytes are three home-slot register saves
  (`mov [rsp+disp8], r64`) accepted by `foch-runtime-host`'s `steal_len`
  prologue check. The plugin hooks that prologue: one-direction offers stay
  exactly as the vanilla function wrote them, while mixed offers (containing
  both same-party and swapped-party clauses, excluding third-party clauses)
  receive two `negate = 1` passes on the give side only—the first cancels the
  positive add and the second subtracts it—so `CPeaceOffer::Execute`, the
  caller at `0x598ed0`, and any other caller of `0x978b20` read the signed net
  total.
- **Ally trust protection model**: `src/trust.rs` defines the product fairness
  rule (`preserves_trust` and `preserves_all_allies`) for mixed deals. An
  ally's trust is preserved when the overall deal net is non-negative
  (`deal.net >= 0`, so the war leader is not overreaching) and the ally's own
  net stake is non-negative (`ally.gains >= ally.losses`, comparing clauses
  whose receiver is that ally against their contribution to the leader's
  demands). `preserves_all_allies` requires every ally to satisfy this check
  individually so aggregate concessions cannot hide a single short-changed
  ally.
- **Execution recovery and logging**: On `CPeaceOffer::Execute` (`0x976730`),
  if a pending mixed submission matches the settling parties, the hook
  reconstructs the combined selection from the demand clauses and snapshotted
  concessions (recovering give-side terms dropped or zeroed by the response
  pipeline) and runs a dedicated concession-direction (`direction = 0`) pass
  for any remaining nonzero give-side ducats. Every executed peace offer is
  logged with its clause count, giver, receiver, direction, cached score, and
  per-clause details (`kind`, `target`, `amount`, `giver`, `receiver`), paired
  with `FOCH_RECEIPT/1` records.

## Supported version and dependencies

- **Target game**: Europa Universalis IV `=1.37.5` (Steam, Windows x64,
  `windows-x86_64`), pinned to `eu4.exe` SHA-256
  `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`.
- **Activation guard**: Before installing any redirect or hook, `profile::verify`
  checks the host ABI (`abi_major = 0`), the exact `eu4.exe` SHA-256 digest,
  and all 17 code sites in `src/profile.rs`. Any other executable or modified
  instruction site is refused cleanly, leaving the vanilla peace editor active.
- **Workspace dependencies**: Built against `foch-plugin-sdk` (`=0.0.1`) and
  `foch-runtime` (`=0.0.2`), loaded at runtime by `foch-runtime-host`
  (`foch_runtime_host.dll`), and managed through the `foch` CLI (`foch-cli`).

## Overhaul mod compatibility

- **40-slot mirrored kind table**: In the pinned image, an offer's kind table
  (`+0xd0`) has 40 buckets of `0x18` bytes (constructor immediate `0x28`), and
  the treaty clone dispatcher (`0x9a1040`) rejects kind `>= 40` (immediate
  `0x27`). This 40-slot bound (`contract::KIND_SLOTS = 40`) is a binary layout
  limit of `eu4.exe`, not a hardcoded list of vanilla treaty names.
- **Custom `common/peace_treaties` and `create_subject` / `copy_from` opaque
  target words**: The plugin identifies each clause by the `(kind, target)` key
  returned from `vtable+0x78`, validating `kind < 40` and treating `target` as
  an opaque 64-bit word (`u64`). Scripted peace treaties added by overhaul mods
  under `common/peace_treaties` and custom subject types reached through
  `create_subject` (including `copy_from`) occupy one of the 40 image kind
  slots and keep their target word opaque without requiring vanilla name
  tables. The monthly-tick probe likewise accepts dynamic or modded 3-character
  country tags (such as `C00` or `A01`) and any kind slot `0..=39`.

## Build and local installation

Build the plugin and the Windows native host from the repository root:

```powershell
cargo build -p foch-plugin-peace -p foch-runtime-host
cargo test -p foch-plugin-peace --lib
```

For a local plugin package, place `foch-plugin.toml` and the built
`foch_plugin_peace.dll` in one directory, then import it with:

```powershell
foch plugin import <directory>
```

Use a disposable EU4 profile and an isolated Foch runtime for development. Set
`FOCH_PEACE_NO_REDIRECTS=1` to bisect startup by running profile verification
without installing UI/send/execute redirects or the `0x978b20` rebuild hook.
This crate is unpublished (`publish = false`) and is not a Workshop or release
package.

## Offline probe collection

The script-armed probe (`src/probe.rs`) reads `foch_peace_probe_*` country
flags on the monthly economy tick and emits a `FOCH_RECEIPT/1` submission line
(`type = "mixed_probe"`, `native_calls = "0"`). Collect or bundle the probe
fixture without launching EU4:

```powershell
foch test src/packages/foch-plugin-peace/fixtures/probe --collect-only
foch test src/packages/foch-plugin-peace/fixtures/probe --no-run --out target/peace-probe-bundles
```

## Known limitations and verification boundary

- **Static disassembly evidence vs. live game behavior**: The pricing chain,
  vtable slots, and direction reads documented here and codified in
  `src/contract.rs` are derived from static disassembly of the pinned `eu4.exe`
  image (`c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`)
  and offline unit tests (`direction_read_verified_in_game` and
  `mixed_settlement_verified_in_game` are `false` for all variants). Process
  startup, hook installation, or a `mixed_probe` receipt (`native_calls = "0"`)
  does not verify in-game treaty evaluation or settlement.
- **AI acceptance path is not verified in game**: The offer copy constructor
  (`0x975f40`) and `CPeaceOffer::GetCost` (`0x979060`) copy or read the cached
  total at `+0xf0` without calling `0x978b20`. `CPeaceOffer::Execute`
  (`0x976730`, calling rebuild at `0x97696d`) and its caller `0x598ed0` (no
  `CExecutePeaceOfferCommand` symbol in the image; rebuilds its embedded offer
  at `+0x48` after two lookups succeed) both call `0x978b20`, where the
  plugin's hook restores the signed net total. However, a separate `GetCost`
  caller at `0x36e510` constructs a fresh offer instead of copying the player's
  offer and therefore never reads the player's cached net total. Which path the
  accepting AI uses in practice is **not named in the image and not verified in
  game**.
- **Untraced per-kind cost accumulators and secondary totals fields**: Only
  Kind 0's `vtable+0x118` accumulator was traced through its per-mille negation
  and call to `0x99e430`. Accumulators for the other kind slots were not
  individually traced, and the effect of the two `negate = 1` passes on other
  fields inside the totals struct (`+0xe8`) is **not verified in game**.
- **Per-clause-family live settlement is not verified in game**:
  `CPeaceOffer::Execute` calls each clause's `vtable+0xF0` with the offer in
  `r8` and the offer's receiver (`+0x18`) in `r9`, alongside `vtable+0x98`:
  - For 37 of the 40 kind slots, `vtable+0xF0` is the shared stub at `0xe66b0`,
    which writes zero through its second argument and does not read the offer.
  - **Ducats (Kind 0)**: `vtable+0x98` (`0x9b4040`) returns immediately when
    the amount at clause `+0x30` is zero and does not read an offer direction
    byte in that body, but payee resolution follows the offer's direction
    context; whether the plugin's dedicated concession-direction pass transfers
    treasury funds as intended in a live mixed deal is **not verified in
    game**.
  - **Annex and subject clauses (Kind 3)**: `vtable+0xF0` (`0x9a7070`) compares
    the offer receiver passed by `Execute` with the clause's own receiver
    (`+0x18`), and `vtable+0x98` (`0x9a60e0`) calls `CCountry::Annex`
    (`0x367390`) with a side path at `0x9a6583` gated on bit `0x20` of
    `country+0x958`. No native trust immediate or symbol was found on that
    branch, and `trust::preserves_trust` remains a library rule rather than a
    hook into a native trust-mutation function.
  - **Cede provinces**: `Execute_CedeProvinces` (`0x990ed0`) saves its offer
    argument and does not read that saved register back, while its downstream
    province-transfer call chain has not been traced to an offer-direction
    byte.
  - **Trailing slot / scripted treaties (Kind 39)**: `vtable+0x98` (`0xac9fa0`)
    reads the clause's receiver (`+0x18`) and the pointer at `+0x30` (the
    scripted-treaty shape, unnamed by symbol).
  - **War Reparations (`war_reparations`)**: The `war_reparations` string is
    present in the executable, but its execution function has **not been
    located or tied to a kind slot**.
  - How a mixed treaty settles in a running game for provinces, ducats, war
    reparations, subject clauses (`create_subject` / `copy_from`), or scripted
    `common/peace_treaties` terms is **not verified in game**. Test only in a
    disposable EU4 campaign profile.
