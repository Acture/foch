# Bilateral peace window

This unpublished Windows x64 plugin is an incomplete experiment for EU4
1.37.5. It keeps the native demand and concession drafts and displays both in
the peace description, including a `NET DEAL` line that subtracts the given
side from the demanded side. The two tabs select which side to edit.

**Mixed offers are sent as one merged, net-priced offer.** Submitting with
terms on both sides clones the inactive side's selected terms into the active
offer, re-applies the give side's clauses through the native signed cost
accumulator so the merged offer's cached total becomes the net deal value
(demands minus concessions) that native validation and AI evaluation read,
lets the original submission copy that offer into the peace action, and then
restores the editor's own selection. The merged clones stay owned by the
editor's offer until it is destroyed; nothing is freed twice.

While both drafts have terms, the summary hook also writes the net value
into the active draft's cached total, so the native cost display and the
`NET DEAL` line in the agreement text both show the merged deal value; a
single-direction selection keeps its own display.

Every executed peace offer is logged with its clause count, parties,
direction and cached score. A merged offer whose inactive-side clauses
vanish between submission and execution is visible in that log line
instead of as a silent one-direction treaty.

The pricing chain above is verified by disassembly of the pinned image only
(`eu4.exe` sha256 `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`).
`src/contract.rs` records that static read. It is not an in-game result.

The cost rebuild at `0x978b20` always passes `negate = 0` and the offer's
direction byte, then overwrites the cached total at `+0xf0`. Its first 15
bytes are three home-slot saves, which the host `steal_len` accepts. The
plugin hooks that prologue. A one-direction offer is left exactly as the
original function wrote it. A mixed offer, one that has both a same-party
clause and a swapped-party clause, then gets the send path's two `negate = 1`
passes on the give side only, so `Execute`, the command at `0x598ed0`, and
any other caller of this function read the signed total. Kind 0's accumulator
negates the per-mille add when that byte is set and then calls `0x99e430`;
other kinds' accumulators were not each traced, and the extra totals records
from those two passes are **not verified in game**. The copy
constructor copies that dword and does not call the rebuild. `GetCost` reads
the dword and does not call the rebuild. `CPeaceOffer::Execute` calls the
rebuild before it walks clauses; with the hook installed, the signed total
is written back before `Execute` continues. Without the hook, a net total
does not survive that call.
The function that calls `Execute` (`0x598ed0`, no
`CExecutePeaceOfferCommand` string in the image) also rebuilds its embedded
offer at `+0x48` after two lookups succeed. A separate `GetCost` caller at
`0x36e510` constructs a new offer instead of copying one, so it is not
reading the player's net total. Which of these the accepting AI uses is
**not named and not verified in game**.

The offer's kind table is 40 slots in this image (constructor immediate
`0x28`, clone rejects kind `>= 40`). That is a layout limit, not a list of
vanilla treaty names. `CPeaceOffer::Execute` calls each clause's `vtable+0xF0`
with the offer in `r8` and the offer's receiver in `r9`. For 37 of the 40
slots that entry is the stub at `0xe66b0`, which writes zero through its
second argument and does not read the offer. Kind 0's `vtable+0x98`
(`0x9b4040`) returns immediately when the amount at clause `+0x30` is zero
and does not read an offer direction byte in that body. Kind 3's
`vtable+0xF0` compares the offer receiver `Execute` passed in with the
clause's own receiver. Kind 39's `vtable+0x98` reads the clause's receiver
and the pointer at `+0x30`; that is the scripted-treaty shape, still not
named by a symbol. `Execute_CedeProvinces` (`0x990ed0`) saves its offer
argument and does not read it back. The string `war_reparations` is present
and was not tied to an execute. Scripted treaties in `common/peace_treaties`
and subject types reached through `create_subject` (including `copy_from`)
stay an opaque target word. How a mixed treaty settles for provinces, money,
war reparations, subject clauses, or a modded treaty is **not verified in
game**. Test only in a disposable EU4 campaign profile. Startup alone does
not verify any of this.

The agreement text keeps the game's own clause bytes, including a non-English
UI, and adds only the numeric net line plus `(+)` / `(-)`.

Collect the probe fixture without launching the game:

```text
foch test src/packages/foch-plugin-peace/fixtures/probe --collect-only
foch test src/packages/foch-plugin-peace/fixtures/probe --no-run --out target/peace-probe-bundles
```

Build with:

```powershell
cargo build -p foch-plugin-peace -p foch-eu4-host
cargo test -p foch-plugin-peace --lib
```

For a local package, place `foch-plugin.toml` and the built
`foch_plugin_peace.dll` in one directory, then import it with
`foch plugin import <directory>`. Use a disposable EU4 profile and an isolated
Foch runtime for development. It is not a Workshop or release package.
The exact executable and patched instructions are checked before activation;
other builds are refused.
