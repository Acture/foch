# Dynamic Trade Blackhole

A first-party Foch native plugin (and DLL-only EU4 mod) that closes and
reopens vanilla outgoing trade routes at runtime, so trade converges
toward wealthy nodes instead of the fixed vanilla end nodes. It never
touches `common/tradenodes`, so no other trade mod is displaced.

## Concept

EU4's trade topology is fixed at load: value always flows toward the three
vanilla end nodes. This plugin re-opens that decision during play:

- a node's **wealth** is a weighted sum of its tax base (purchasing power),
  production and trade power (weights in percent, all configurable);
- each outgoing edge is judged against the **basin wealth** reachable
  through it on currently open edges, discounted per hop;
- an open edge whose basin is at least `converge_pct` poorer than the node
  closes; a closed edge reopens only under `hysteresis_pct`, so a band
  between the two thresholds cannot flap;
- a node whose edges all close is an emergent **black hole** — exactly the
  engine's own representation of an end node;
- closures drain the poorest basins first, reopens restore the richest
  first, and per-node/per-month budgets keep the trade map readable.

Because the plugin only removes edges, the live topology is always a
subgraph of the vanilla DAG: no cycles, no new states the engine could not
have loaded itself.

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
| `policy` | Run the wealth-gradient planner |

`force_node` matches a node's **display name** in the English locale (the
engine keeps display names on the definition; internal keys only exist
per route entry), e.g. `Great Lakes`, `Lübeck`. Configuration is the
manifest schema (`[config.*]` in `foch-plugin.toml`); values are chosen
per playset in `~/.config/foch/plugins/selections.toml`.

## Build, package, run

```powershell
cargo build -p foch-plugin-trade-blackhole -p foch-eu4-host -p foch-cli --release --locked
uv run python -I D:\eu4-re\scripts\assemble_pkg.py   # or fill [[files]] by hand
foch plugin import ./target/plugin-pkg
foch plugin enable dev.foch.trade-blackhole --playset default
foch plugin plan --playset default --game-path <EU4>
foch plugin launch --playset default
foch plugin status --run-dir <runtime dir>
```

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
| node | +0xb0 | current total trade value (int32) |
| def | +0x30 | MSVC SSO **display name** (`Great Lakes`, `Lübeck`) |
| def | +0x98 / +0xa0 | `vector<CTradeRoute>` begin/end, stride 0x78; 159 edges across all nodes |
| route | +0x10 | MSVC SSO **internal key** of the target (`zanzibar`, no suffix — matches `00_tradenodes.txt`) |
| route | +0x30 | target node's `CTradeNodeDefinition*` |

Mutation primitive: writing the route vector's `begin` into `end`
empties a node's outgoing routes — the same state a vanilla end node
has — and restoring the cached original `end` reopens it. No allocation,
no realloc, no engine state the loader could not have produced.

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

- 2026-11-06, EU4 1.37.5 Steam x64, `foch test --with-plugins default`
  (`plugin_host_smoke`, 1444.11.11 SWE, `advance_days=70`): smoke passed,
  empty game error log, clean exit. Plugin events: `graph frozen: 81
  nodes, 159 edges`, then `force_toggle tick 1..4: node 1 closed /
  reopened 2 edges` alternating — the Great Lakes node's two vanilla
  outgoing routes were closed and fully restored four times in one
  session. Artifacts: `foch-tests-*` session dir, `foch-host-events.jsonl`.

## Known limits

- Only the pinned Steam 1.37.5 x64 build; every other build is refused
  (`refused: eu4.exe is not the reversed build`).
- Ironman/achievements are unavailable while any DLL plugin is loaded.
- `force_toggle` works on whole nodes; per-edge policy mutations (route
  entry reordering, since the vector is contiguous) arrive with the
  policy milestone.
- Node identification uses English display names; a locale-independent
  key lookup (route entries carry the internal key) is future work.
- Tax/production aggregates for the wealth function are still zero in
  the snapshot (M2); the policy mode currently judges on trade value
  until the province aggregates land.
