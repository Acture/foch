# `eu4-country-month` (`dev.foch.eu4-country-month`)

`eu4-country-month` (`v0.1.0`) is an experimental native Windows x86_64 plugin for `foch-runtime-host`
targeting **Europa Universalis IV 1.37.5** (`eu4.exe` SHA-256
`c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`, the AMDAC
checksum-patched baseline, or
`9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a`, the stock
Steam digest; live verification has only been run against the AMDAC baseline).
It parallelizes per-country
monthly state updates ahead of the game-thread monthly country loop, relocates the daily
active-country loop body, and includes opt-in environment-gated parallel dispatchers for Daily Loop A
(`0x759640`) and the Daily Loop B (`0xa89360`) fixture stand-in.

## Current Status

- **Offline host & fixture suite**: Passing via `cargo test -p foch-runtime-host` (differential
  serial-vs-parallel fixture checks, allocator arena accounting, large-world scaling, and
  fault-injected preflight fallbacks).
- **Production packaging & preflight**: Passing via `build.ps1` and `phase4-preflight.ps1`.
- **Live EU4 validation**: **Pending.** End-to-end live-game soak testing, live multi-month save
  determinism comparisons, and real-game A/B wall-clock timing measurements have not yet been
  recorded.

---

## Architecture & Hook Surface

The plugin compiles `country_month.c` and `country_detour.asm` into `eu4_country_month.dll`
(`phase = "entry"`, `abi_major = 0`, `restart_required = true`). During `init`, the plugin validates
`FochHostApi`, runs an in-memory relocation self-test (`probe_active_body`, verifying body hash
`0x7747E17D`), stages up to 12 hooks in `owned_hooks[12]` at priority `10`, and enables them
atomically with `owned_commit`. If any pattern lookup, byte-profile check, `VirtualQuery` code-page
check, or hook enablement fails, `owned_rollback` disables all hooks, frees near-allocated code
buffers, and reports `FOCH_STATE_FAILED`.

### Default Hooks (Always Installed When Signatures Match)

1. **Monthly Country Loop Head (`country_detour` -> `country_split`)**
   - Matches pattern `49 8B 87 E8 1C 00 00 45 0F B6 C4 41 0F B6 D5` and verifies the 73-byte
     (`0x49`-byte) `monthly_image` loop profile and both `call rel32` targets (`+0x13` primary
     monthly callback and `+0x3A` cadence helper).
   - On outer entry (`country_splitting` guard), `country_detour` preserves general registers and
     full x87/SSE state via 512-byte `FXSAVE64` / `FXRSTOR64` and calls
     `country_split(r15, rbx, rsi)`.
   - `country_split` reads the country array at `game + 0x1CE8` (`COUNTRY_ARRAY`) and dispatches
     per-country state array walks (`country + 0x24B0` .. `country + 0x24B8`,
     `STATE_BEGIN`..`STATE_END`) across a Windows thread pool.
   - Once workers join, `states_done` is set to `1` and execution jumps to `country_body`
     (`build_monthly_loop`), a near-allocated (`±0x7F000000` bytes) relocated copy of the 73-byte
     loop that replays in the original game-thread frame with live `EDI` before exiting through
     `country_replay_done` to `site + 0x49`.
2. **Per-Country State Function (`state_detour`)**
   - Matches prologue pattern
     `40 53 55 56 57 41 54 41 55 41 56 41 57 48 83 EC 28 48 8B F9 45 33 ED`.
   - Worker threads invoke the original function through the host trampoline cell (`*state_next`).
     When the game thread replays `country_body` with `states_done != 0`, `state_detour` returns
     immediately (`ret`) so state updates are not executed twice.
3. **Daily Active-Country Loop (`active_detour` -> `active_takeover`)**
   - Matches header pattern `44 8B E7 48 8B 8D E8 1C 00 00 48 8B 85 F0 1C 00 00`, validates the
     37-byte (`0x25`-byte) `active_header_image`, resolves three RIP-relative
     `cmp ecx, [rip+disp32]` globals at `+0x6F`, `+0x77`, and `+0x7F`
     (`active_g0`, `active_g1`, `active_g2`), and verifies the loop body
     (`site + 0x25` .. `site + 0x1EE`, hash `0x7747E17D`).
   - Against the installed game (`active_real = 1`), `build_active_body` relocates the
     `0x25..0x1EE` loop body into near executable memory, retargeting all 8 direct `call rel32`
     sites (`0x9E, 0xD5, 0xE3, 0x157, 0x173, 0x17F, 0x18B, 0x1C2`) and 3 RIP-relative comparisons
     (`0x6F, 0x77, 0x7F`), and runs it on the game thread.
   - Against the host fixture (identified by magic dword `0xC0DEF00Du` before the predicate at
     `+0x157`), `active_takeover` evaluates country fields (`+0x20`, `+0xD28`, `+0x1344`) on the
     worker pool and commits list appends (`+0x1D14`, `+0x1D18`, `+0x1D30`, step `0x3E8`) on the
     game thread.

### Runtime Environment Switches

| Environment Variable | Default | Effect |
| :--- | :--- | :--- |
| `FOCH_COUNTRY_SERIAL` | Unset (`0`) | When `FOCH_COUNTRY_SERIAL=1`, `country_split` skips the worker pool (`states_done = 0`) and runs the full monthly loop serially on the game thread (`serial=2` in `countries-real.txt`) as an A/B baseline control. |
| `FOCH_DAILY_A` | Unset (`0`) | When `FOCH_DAILY_A=1`, enables installation of Daily Loop A (`0x759640`, queue drain head at `0x7596a0`, body length `0x1441`, hash `0xEEA14823`) on the installed game. (The fixture profile `0x05F61D14` installs unconditionally in offline tests.) Hooks the loop head (`daily_a_detour`), epilogue (`daily_a_end_detour` at `site + 0x1442`), and process-wide alloc/free funnels (`DA_ALLOC_SITE = 0xA4`, `DA_FREE_SITE = 0x35F`). Workers run a decoded/relocated body copy (`daily_a_decode` / `daily_a_relocate`) against private queue snapshots (`game + 0x1E30`, count `+0x1E3C`), a `0x800`-byte shadow frame (`rbp` at `+0x300`, `daily_a_ctx` at `rbp + 0x1C0`), and TLS arenas. Day 1 (`daily_a_calls == 0`) runs warm on 1 worker so the `0x8bf160` singleton initializes before parallel dispatch; shared removals (`0x77ac80`) and list publications (`0xdf120` onto `game + 0x1E40..0x1E50`) replay in serial order on the game thread. |
| `FOCH_DAILY_B` | Unset (`0`) | When `FOCH_DAILY_B=1`, enables `install_daily_b` **only** for the test fixture export `fixture_daily_b_addr` (two-walk revolution center backward pass with `DB_THRESHOLD = 0x186a0` and forward province spread pass). The installed `0xa89360` game body is never hooked even when `FOCH_DAILY_B=1`. |

---

## Intentionally Unhooked Sites

Fail-closed static analysis in `country_month.c` keeps two daily-chain candidates on their native
serial paths to prevent cross-country data races and unsynchronized RIP-relative global writes:

- **`0xa89360` (Daily Loop B installed body) and callees `0x93d750` / `0xa1a9a0` / `0x14d6110`**:
  `install_daily_b` locates the 25-byte `0xa89360` prologue
  (`48 89 4C 24 08 55 53 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 28 EC FF FF`) and records
  `daily_b_real_site`, but intentionally leaves it unhooked (enforced by
  `daily_b_real_prologue_blocked`). In the native call graph, `0x93d750` only writes to stack slots
  and never updates the state dword at `this + 8`; the four call sites in `0xa1a9a0` index
  `country_table[tag_index] + 0x28c0` using `tag_index` read from `+0x468` on the current object
  (which is not proven to alias the current object); and when `this + 8` is already `1`, it triggers
  the `CALCULATING` re-entry assert into `0x14d6110`, whose fast path writes the caller's buffer
  pointer and three dwords through fixed RIP-relative global slots on every entry.
- **`0x1d0e20` (called from `0x75c895`) and callee `0xd405b0`**:
  The next daily-chain callee after `0x759640` and `0xa89360` is `0x1d0e20` (22 back-edges, single
  pointer-array walk). While `0x1d0e20` itself contains no RIP-relative stores, its callee
  `0xd405b0` writes `1` through fixed RIP-relative global bytes, making concurrent worker execution
  unsafe.

### Next callees on the pinned 1.37.5 image

Read-only disassembly of `eu4.exe` SHA-256
`c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e` (image base
`0x140000000`, bounds from `.pdata`). A RIP store is a memory operand based on
`rip` that the instruction writes. Allocator calls are `0x1a332d4` (the `E8` at
daily-A body `0x7596a0+0xA4`, and again at `+0x32B` and `+0x3D1`) and free thunk
`0x17394f0` (`jmp 0x175da20`, then `jmp 0x1a46238`), including those calls one
hop down. Nothing in this table is hooked.

The monthly tick is `0x75d690`–`0x75e189` and already contains the hooked country
loop at `0x75dd30`. The daily parent is `0x75bbe0`–`0x75c9ef` (`0x75c895` is
inside it) and calls `0x759640`, `0xa89360`, and `0x1d0e20`.

The unhooked sites still match this image: `0x14d6110` has seven RIP stores, the
first at `0x14d6186`; `0xd405b0` has three, the first at `0xd405ec`. `0x93d750`,
`0x1d0e20`, and `0xa89360` still have none in their own bodies.

| RVA | Caller | Bytes | Class | Evidence |
| --- | --- | --- | --- | --- |
| `0x2f36c0` | monthly `0x75d690` | `0x47c1` | blocked by RIP store | `0x2f3741` writes a byte through `rip` |
| `0x707aa0` | monthly | `0x11b7` | blocked by RIP store | `0x707afa` |
| `0x75f630` | monthly | `0xc1d` | blocked by RIP store | `0x75f83d` |
| `0x771000` | monthly | `0xbd2` | blocked by RIP store | `0x77163f` increments a `rip` dword |
| `0x1b69a0` | monthly | `0x97f` | blocked by RIP store | `0x1b6c31` increments a `rip` dword |
| `0x14d4c80` | monthly | `0x143` | blocked by RIP store | `0x14d4ce0` plus 16 more RIP stores in that body |
| `0xd8ba80` | daily A `0x759640` | `0x106e` | blocked by RIP store | `0xd8bae2` |
| `0xd8e6b0` | daily A | `0xfde` | blocked by RIP store | `0xd8e70d` |
| `0x2e9ab0` | daily A | `0x841` | blocked by RIP store | callee `0x798dc0` writes at `0x798e6b` |
| `0x759030` | daily A | `0x602` | blocked by shared free | seven direct calls to `0x17394f0`; the body itself has no RIP store |
| `0x760250` | daily parent `0x75bbe0` | `0x850` | blocked by RIP store | `0x760299` |
| `0x70af70` | daily parent | `0x920` | blocked by RIP store | `0x70b002` |
| `0x75b4b0` | daily parent | `0x727` | blocked by RIP store | `0x75b6a0` |
| `0x93ca70` | daily parent | `0x6bd` | blocked by RIP store | callee `0x14d6110` writes at `0x14d6186` |
| `0xacfae0` | daily parent | `0xae7` | blocked by shared object writes | no RIP store and no alloc/free in two hops; reads `.data` `0x233fe78` and writes `[rsi+0x370]` / `[rsi+0x374]` |
| `0x758f30` | daily A body `0x7596a0+0x25E` and `+0x274` | `0xfe` | clean read-only predicate | no call, no RIP store, no object store; reads `.data` `0x233d8d0` and province records of stride `0x2e10` |

`0x14c71b0` (monthly callee, `0x124` bytes) has no RIP store and no allocator
call. It steps an MT19937 object (`0x9908b0df`, index at `+0x9c0`) and writes
that object, so it is not a country/province parallel candidate. `0x170c30`
calls `0x1a332d4`. `0x7e2770` and `0xaee0c0` reach `0x17394f0`.

On this image the byte at `0x7596a0+0x35F` is `48 89 45 48`, not `E8`. The free
call in the daily-A body is at `+0x3C4`. `DA_FREE_SITE` is unchanged, so a real
install still fails closed on that check.

---

## Overhaul Mod & Large-World Scaling

The plugin avoids fixed country, province, or revolution-center array limits so overhaul mods with
expanded tag and province counts scale dynamically or fail closed to native execution:

- **Dynamic world bounds & worker clamping**: `country_split` (`game + 0x1CE8`), `active_takeover`
  (`game + 0x1CE8..0x1CF0`), `daily_a_dispatch` (`game + 0x1E30`, count `+0x1E3C`), and
  `daily_b_dispatch` (`center_count`, `country_count`, `province_count`) read live counts on every
  tick. Worker count `n` scales with `dwNumberOfProcessors`, clamped to `MAX_WORKERS = 32` and the
  active item count.
- **Overflow-checked buffers & operation logs**:
  - `daily_a_dispatch` enforces `count <= UINT_MAX / 2` and uses overflow-checked multiplications
    (`daily_mul_size`) for queue snapshots, `count` pre-allocated `0x20`-byte publication nodes, and
    per-slice operation logs sized to `op_cap = share * 2` (covering 1 light removal or 2 heavy ops
    per entry).
  - `daily_b_dispatch` grows output arrays (`fired`, `same_tag`, `messages`, `spread_id`,
    `spread_n`) on demand via `db_grow` and sizes Walk 1 op logs to `ops_each = 6 + message_factor`
    per center (`db_ops_for`) and Walk 2 op logs to `1` per province.
- **Per-worker arena growth & native fallback (`daily_a_arena`)**:
  Each of the up to `MAX_WORKERS` (32) persistent worker arenas reserves a `0x400000` (4 MB) primary
  virtual span (`MEM_RESERVE`), committed in doubling chunks starting at `0x10000` (64 KB) with 61
  freelist size classes in 16-byte steps (up to `61 * 16 = 976` bytes). When a slice's budget
  (`count * DAILY_ENTRY_BUDGET`, `64` bytes/entry) exceeds 4 MB, `daily_a_arena_ensure` allocates a
  second committed span (`extra_base`). Allocations over 976 bytes or beyond reserved spans fall
  back directly to the game's native allocator funnel (`daily_a_call_alloc`) without clipping
  (`oversized_fallbacks`).
- **Fail-closed preflight & replay rollback**: If any heap/virtual reservation fails or any worker
  sets `part->overflow`, parallel dispatch aborts before mutating game structures (or restores the
  saved queue and intrusive-list pointers via `undo` in `daily_a_dispatch`), writing
  `stage=preflight-fallback` and letting the original native loop run.

---

## Building & Preflight Checks

Run all commands from the repository root in PowerShell:

```powershell
# 1. Run the offline host & differential fixture test suite
cargo test -p foch-runtime-host

# 2. Build and stage eu4_country_month.dll + foch-plugin.toml into target\plugins\eu4-country-month
pwsh -File src/plugins/eu4-country-month/build.ps1

# 3. Verify staged manifest/DLL and check an installed eu4.exe against the known 1.37.5 SHA-256 digests
pwsh -File src/plugins/eu4-country-month/phase4-preflight.ps1 -Build -ExePath "C:\path\to\eu4.exe"
```

For manual Phase-4 A/B testing (never auto-launched by preflight scripts):
1. Import the staged package with `foch plugin import target\plugins\eu4-country-month`.
2. Run the baseline arm with `FOCH_DAILY_A` and `FOCH_DAILY_B` unset (or `FOCH_COUNTRY_SERIAL=1`
   for serial monthly control).
3. Run the treatment arm with `FOCH_DAILY_A=1` only (`FOCH_DAILY_B` unset), and stop immediately if
   `daily-a-real.txt` reports `arena-live` other than `0` or if two saves from the same start differ.

---

## Known Limits & Verification Boundary

| Area | Status | Notes |
| :--- | :--- | :--- |
| Host & fixture differential tests (`cargo test -p foch-runtime-host`) | **Verified** | Covers monthly state split, active-country loop relocation/fixture, Daily Loop A & B fixture parity, large-mod counts, and allocation-failure fallbacks. |
| Production x64 DLL build & manifest staging (`build.ps1`) | **Verified** | Builds with MSVC `cl.exe` (`/O2 /LD /W4`) and `ml64.exe` without fixture defines. |
| Pinned `eu4.exe` 1.37.5 hash & package preflight (`phase4-preflight.ps1`) | **Verified** | Confirms SHA-256 `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e` (AMDAC baseline) or `9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a` (stock Steam) and prints which variant matched, without starting the game; stock live verification is pending. |
| Live EU4 1.37.5 end-to-end soak & save determinism | **Not Yet Verified** | Manual operator ladder required; abort any `FOCH_DAILY_A=1` arm if `daily-a-real.txt` reports `arena-live != 0` or if two saves from the same start diverge. |
| Live EU4 A/B performance timings (`FOCH_COUNTRY_SERIAL=1` vs pool, `FOCH_DAILY_A=1`) | **Not Yet Verified** | QPC `span-ns` proof logging (`countries-real.txt`, `daily-a-real.txt`) is implemented, awaiting live benchmark capture. |
| Daily Loop B (`0xa89360`) live-game parallelization | **Intentionally Disabled** | `FOCH_DAILY_B=1` hooks only `fixture_daily_b_addr`; the installed `0xa89360` prologue is blocked from hooking due to `0x93d750` / `0xa1a9a0` / `0x14d6110` aliasing and RIP-relative writes. |
