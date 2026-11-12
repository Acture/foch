# Dynamic Trade Blackhole

A first-party Foch native plugin (and DLL-only EU4 mod) that closes and
reopens vanilla outgoing trade routes at runtime, so trade converges
toward wealthy nodes instead of the fixed vanilla end nodes. It never
touches `common/tradenodes`, so no other trade mod is displaced.

## Concept

EU4's trade topology is fixed at load: value always flows toward the three
vanilla end nodes. This plugin re-opens that decision during play with a
role-based rule: each trait keeps its own role over a node's neighborhood
(the node plus its direct vanilla trading partners), and nothing is ever
blended into a combined score:

- **tax base** (purchasing power) is the only closing signal: the node
  with the neighborhood's highest *per-province mean tax* closes all its
  outgoing edges — an emergent black hole. The mean, not the sum:
  node-level sums are dominated by province count (1.37's reworked
  Manchuria has 77 mostly-poor provinces and out-sums Beijing), while
  the mean expresses wealth intensity and lets the true cores emerge
  (Hangzhou 5.22 over Beijing 3.24 and Girin 1.94; Champagne, Genoa,
  Venice top the world). `sink_basis = "sum"` keeps the raw mass if a
  scenario wants it. Reopening only happens inside the `hysteresis_pct`
  band, so month-to-month noise cannot flap a black hole;
- **trade power** means transit: a hub keeps flowing (its edges stay
  open; producers' edges into it are vanilla and stay open too);
- **production** means upstream: a producer keeps exporting, and the
  engine already turns its output into local trade value.

Tax comes from the provinces themselves: the `CProvince` array exposed by
the `Set base tax to` console command (context+0x1ca8, stride 0x2e10,
base tax at +0x3e4 in thousandths — live-verified against the history
files), summed per node over an offline `node_members.txt` generated from
`00_tradenodes.txt` and shipped inside the plugin package. The runtime
key-to-definition mapping is learned from incoming route entries (an
entry names its target's internal key), so nothing depends on the game
locale.

The north-star design is a propagation network (producers inject, hubs
relay, tax sinks absorb, unused paths wither); the neighborhood rule is
its first decisive approximation, chosen because pure per-edge flow
pruning would freeze once minor traffic keeps every edge faintly alive.

Because the plugin only removes edges, the live topology is always a
subgraph of the vanilla DAG: no cycles, no new states the engine could
not have loaded itself.

## Package layout

- `src/policy.rs` — the wealth-gradient planner, pure and fully tested
- `src/engine.rs` — monthly orchestration (mirror, modes, logging)
- `src/runtime.rs` — `LiveBackend`: reads and mutates the reversed engine
  structures through safe, validated accessors
- `src/hook.rs` — the game-thread detour the Foch host chains for us
- `src/offsets.rs` — the pinned build's pattern signature and layout
  table (see the RE record below)
- `tests/e2e-mod/` — the smoke-test mod for the automated end-to-end run

## Modes (config)

| Mode | Behavior |
| --- | --- |
| `observe` | Log the trade graph every month; never mutate |
| `force_toggle` | Close and reopen one node's whole route vector on alternating months (the mutation-primitive proof) |
| `policy` | Run the neighborhood tax-peak planner |

`force_node` matches a node's **display name** in the English locale (the
engine keeps display names on the definition; internal keys only exist
per route entry), e.g. `Great Lakes`, `Lübeck`. Configuration is the
manifest schema (`[config.*]` in `foch-plugin.toml`); values are chosen
per playset in `~/.config/foch/plugins/selections.toml`.

## Build, package, run

```powershell
cargo build -p foch-plugin-trade-blackhole -p foch-eu4-host -p foch-cli --release --locked
uv run python -I src/packages/foch-plugin-trade-blackhole/tools/assemble_pkg.py
foch plugin import ./target/plugin-pkg
foch plugin enable dev.foch.trade-blackhole --playset default
foch plugin plan --playset default --game-path <EU4>
foch plugin launch --playset default
foch plugin status --run-dir <runtime dir>
```

`tools/assemble_pkg.py` copies the release DLL plus the offline
`tools/node_members.txt` (the node membership table generated from
vanilla `common/tradenodes/00_tradenodes.txt`, shipped with the package)
into `target/plugin-pkg` and fills the `[[files]]` sha256 digests; after
importing a rebuilt package, remove the previous `0.1.0+<hash>` store
directory so `plugin enable` stays unambiguous.

Automated end-to-end (loads a 1444.11.11 start by itself, advances 70
days across two month boundaries, requires no interaction; the plugin
host is staged into the test session's runtime layer through
`foch test --with-plugins`):

```powershell
foch test src/packages/foch-plugin-trade-blackhole/tests/e2e-mod `
  --with-plugins default --game-path <EU4> --timeout 900
```

The judge fails on a non-empty game error log, and the plugin's own
decisions are preserved in the session artifacts as
`session_*/foch-host-events.jsonl`.

## RE record: EU4 1.37.5 Steam x64

Reversed with Ghidra 12.1.3 (headless PyGhidra) against `eu4.exe` sha256
`c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`.
The binary has no RTTI; the anchors were found through the `tradenodes`
string cluster, which carries the developers' own source paths
(`source/tradenode.cpp`, `tradenodedatabase.cpp`, `tradenodedefinition.cpp`),
then confirmed instruction by instruction and corrected against live
memory dumps from automated test sessions.

Hook target: the economy update at image+0x75d690 that ends with
`CTradeNodeDataBase::UpdateAll(ctx + 0x2198)` (image+0xb4ba90). It was
chosen over `UpdateAll` itself because the host's hook gate only steals
whole instructions that end exactly at byte 14, and this candidate's
prologue (`MOV [rsp+0x10],DL` + seven pushes) tiles exactly 14 bytes.
Its first argument is the global game context; the plugin cross-checks
it against the context global before every use.

| Structure | Offset | Meaning (live-confirmed unless noted) |
| --- | --- | --- |
| global context ptr | image+0x233fe78 | the hook's first argument |
| `CTradeNodeDataBase` | ctx+0x2198 | embedded, not allocated |
| db | +0x30 / +0x3c | node **pointer** array (stride 8) / count; 81 entries in a full 1.37.5 install |
| node (heap `CTradeNode`) | +0x88 | `vector<int32>` steering weights, paired with routes |
| node | +0xa8 | `CTradeNodeDefinition*` |
| node | +0xb0 | current total trade value (int32, thousandths) |
| node | +0xc8 / +0xd0 / +0xd8 | total trade power, a second near-identical total, and the provincial-only share (int32, thousandths) |
| node | +0xe4 | total goods produced in the node (int32, thousandths) |
| province array | ctx+0x1ca8 | contiguous `CProvince` elements, stride 0x2e10, indexed by province id (from the `Set base tax to` console command) |
| `CProvince` | +0x3e0 / +0x3e4 / +0x3e8 | base manpower / **base tax** / base production (int32, thousandths; all three live-verified against the history files) |
| def | +0x30 | MSVC SSO **display name** (`Great Lakes`, `Lübeck`) |
| def | +0x98 / +0xa0 | `vector<CTradeRoute>` begin/end, stride 0x78; 159 edges across all nodes |
| route | +0x10 | MSVC SSO **internal key** of the target (`zanzibar`, no suffix — matches `00_tradenodes.txt`) |
| route | +0x30 | target node's `CTradeNodeDefinition*` |

Mutation primitive: per edge. The plugin captures a node's vanilla
route entries and steering weights before its first mutation, and every
state change rewrites the live arrays from the originals — open entries
first in vanilla order, closed entries parked at the tail — then shrinks
the vector's `end` to the open prefix. Any open subset is a contiguous,
legal vector (exactly what vanilla end nodes and low-outdegree nodes look
like), all writes stay inside the original capacity, and restoring every
flag rebuilds the byte-exact vanilla layout. The vanilla outgoing list
the policy plans against is frozen at the first observe, so live
reordering never disturbs vanilla slot indices.

Live-confirmed invariants the backend re-checks every observe: node count
within 20..=512, every node pointer and definition pointer plausible,
route vector strides exact, SSO names readable. An MSVC vector with
`begin == end == null` is a legal empty route vector (vanilla end nodes).
The exe digest is verified before any pattern runs, and the resolved hook
address is cross-checked against the recorded RVA.

The hook fires on the game's monthly economy cadence: a 70-game-day
automated session (1444.11.11 start, two month boundaries) produced
exactly four hook ticks. Mutation steps therefore run once per hook tick
without a separate date read (an early experiment that scanned the
context for a date triple crashed the game by reading past the object —
removed).

## Acceptance record

- 2026-11-06 (policy, mean-tax sink): with the per-province mean as the
  sink signal, Hangzhou (5.22 mean tax) terminated its whole outflow over
  Beijing (3.24) and Girin (1.94) — the intensity ranking matches
  offline sums from the history files exactly — alongside Champagne,
  Mexico, Deccan, Aleppo and Novgorod across four ticks.
- 2026-11-06 (policy, tax-peak): same harness — tax aggregated for 73/81
  nodes from the provinces; regional tax centers terminated across four
  monthly ticks (Malacca 205k, Mexico 201k, Nippon 197k, Sevilla 180k,
  Rheinland 162k, Timbuktu 134k, ... each closing its full outflow), and
  the earlier wealth-blend artifact is gone: Girin (149k tax) no longer
  cuts its edge toward Beijing, because Beijing's neighborhood holds
  Girin itself.

- 2026-11-06, EU4 1.37.5 Steam x64, `foch test --with-plugins default`
  (`plugin_host_smoke`, 1444.11.11 SWE, `advance_days=70`): smoke passed,
  empty game error log, clean exit. Plugin events: `graph frozen: 81
  nodes, 159 edges`, then `force_toggle tick 1..4: node 1 closed /
  reopened 2 edges` alternating — the Great Lakes node's two vanilla
  outgoing routes were closed and fully restored four times in one
  session. Artifacts: `foch-tests-*` session dir, `foch-host-events.jsonl`.
- 2026-11-06, same harness in `policy` mode (the shipped wealth inputs:
  goods produced + 25% trade power): 20 per-edge closures across four
  monthly ticks, each logged with its judgment (e.g. `close edge 78 ->
  79 (wealth 141755 vs basin 110682)`), hysteresis held with zero
  flapping, and the economy demonstrably re-routed downstream — node 36
  value 8487 -> 15180 once its branch was cut, nodes 33/35 lost their
  inflow to 0.0, and the vanilla end nodes kept accumulating.

## Known limits

- Only the pinned Steam 1.37.5 x64 build; every other build is refused
  (`refused: eu4.exe is not the reversed build`).
- Ironman/achievements are unavailable while any DLL plugin is loaded.
- Node identification uses English display names; a locale-independent
  key lookup (route entries carry the internal key) is future work.
- Eight of the 81 nodes (pure sources and the vanilla end nodes) have no
  incoming route entry to learn their internal key from, so their tax
  aggregate stays zero; none of them needs a closing decision anyway.
- The province-to-node membership ships as an offline table generated
  from vanilla `00_tradenodes.txt`; mods that redefine trade-node
  membership need the table regenerated.
