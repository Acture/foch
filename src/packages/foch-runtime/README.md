# Foch runtime services (`foch-runtime`)

`foch-runtime` is the reusable Rust library for typed EU4 data-plane services, native
plugin load plans (`plan`), byte-pattern scanning (`pattern`), host artifact validation
and inline-hook mechanics (`host`), war provider contracts (`war`), and the isolated
out-of-process game test runner (`runner`).

## Separation from `foch-runtime-host`

This package builds only an `rlib` (`crate-type = ["rlib"]`). In-process DLL loading,
`DllMain`, and the `VERSION.dll` proxy exports live in the independent
[`foch-runtime-host`](../foch-runtime-host/README.md) `cdylib` package:

- **Host (`foch-runtime-host`)**: Compiles `foch_runtime_host.dll` (staged into the game
  directory as `foch_runtime.dll` / `VERSION.dll`), exporting the 17 `VERSION.dll`
  forwarding ordinals and executing `foch-host\plan.json`.
- **Runtime library (`foch-runtime`)**: Linked by the CLI, test runner, and native plugin
  `cdylib` crates. Keeping `DllMain` and `VERSION` exports out of `foch-runtime` ensures
  host symbols never pollute plugin dependency graphs, and building the host and plugins
  together uses separate DLL artifact names.

## Cargo Features

| Feature | Default | Purpose |
| :--- | :--- | :--- |
| `runner` | Yes (`default = ["runner"]`) | Enables `foch_runtime::runner` and pulls in `foch` and `foch-test` for isolated out-of-process game test sessions. |
| *(none)* (`default-features = false`) | No | Minimal footprint for native plugin DLLs (`dataplane`, `host`, `pattern`, `plan`, `war`). Brings zero host `DllMain` or `VERSION` linker definitions into the plugin dependency graph. |
| `test-support` | No | Opt-in Windows-only synthetic EU4 memory builder (`dataplane::testing::FakeGame`). Leave disabled in production plugin builds so test helpers are never linked into shipped DLLs. |

## Layered Typed Dataplane (`foch_runtime::dataplane`)

### Crate-Private Layout vs. Public Semantic Views

`dataplane::layout` and `dataplane::profile` are crate-private (`pub(crate)`). Plugins
never import raw RVA constants or struct offset tables; instead, they consume typed views
and helpers that keep all engine units in thousandths and surface fallible reads as
`Option` or `Result`:

- **World & economy views**: `Eu4Game`, `Countries`, `CountryView`, `Province`,
  `TradeNodes`, `NodeView`, `RouteMutation`, `live_order`, `PackedCountryTag`,
  `decode_packed_tag`.
- **Flags & war snapshots**: `CountryFlags`, `CountryFlag`, `FlagReadError`, and
  `dataplane::war::{CampaignStamp, CountryRef, WarRoster, read_world, check_country_restrictions}`.
- **Guarded primitives**: `Memory`, `StdVector`, `read_sso`, `is_key_byte`, `is_name_byte`.
- **Synthetic testing (`cfg(all(windows, feature = "test-support"))`)**:
  `dataplane::testing::FakeGame` builds owned in-process synthetic country, province,
  flag, and trade-node allocations read back through real `Memory` kernel copies without
  exposing layout offsets.

### Compiled-In Version × Platform JSON Profiles

Layout offsets are defined in reviewed JSON profiles under `profiles/eu4/` (packaged via
`Cargo.toml` `include = ["/profiles/**", ...]`), embedded at compile time with
`include_str!`, validated under `#[serde(deny_unknown_fields)]`, and published through a
static `OnceLock` registry. Two 1.37.5 Windows x64 profiles are embedded:

- `profiles/eu4/1.37.5-windows-x64.json` — the AMDAC baseline build
  (`exe_sha256` `c9ec55a4…`), the registry's first entry and therefore the default
  build behind `pinned_exe_sha256()` and the no-suffix `Eu4Game::from_*` wrappers.
- `profiles/eu4/1.37.5-windows-x64-stock.json` — the unmodified Steam build
  (`exe_sha256` `9ad3efe1…`). The two executables are identical in size and differ in
  exactly six bytes (four PE header bytes and the two checksum-patch bytes), so the
  stock profile carries the same hooks, globals, structs, strides, and caps; live
  runtime verification against the stock Steam build is still pending.

- **Strict digest matching**: Profiles are keyed strictly by 64-character lowercase
  `exe_sha256` (the default build's digest is exposed via `pinned_exe_sha256()`, and
  membership is queried via `supports_build(exe_sha256)`). Human version strings
  (`"1.37.5"`) are never used as lookup keys, and unsigned external JSON is never
  loaded from disk.
- **ABI & structure schema**: Each profile records `abi` (`stl: "msvc_x64"`,
  `pointer_size: 8`), `hooks` (`monthly_tick` RVA and prologue byte pattern), `globals`
  (`context`, `country_manager`, `flag_names`), and `structs` (`context`, `db`, `node`,
  `entry`, `def`, `route`, `province`, `country_manager`, `country`, `ui`, `campaign`,
  `war`, `diplomacy`, `flags`).
- **Field-level verification metadata**: Every hook and field entry carries a mandatory
  `status` (`"live_verified"` or `"static_only"`) and an `evidence` note recording its
  live probe or static decompilation provenance.

### Multi-Build Routing & Fail-Closed Resolution

- `resolve_monthly_tick(&FochHostApi)` verifies `host.exe_sha256` against the compiled-in
  profile registry before checking image bounds or calling `host.find_pattern`, then
  cross-checks that the unique pattern match equals `exe_base + hooks.monthly_tick.rva`.
- `Eu4Game::from_tick_for_build(exe_sha256, exe_base, candidate_context)` and
  `Eu4Game::from_exe_base_for_build(exe_sha256, exe_base)` (plus default-build wrappers
  `Eu4Game::from_tick` and `Eu4Game::from_exe_base`) validate the digest, verify
  `candidate_context` against `globals.context`, and bind the `(exe_base, context)` pair
  to the matched profile.
- Bound views route all subsequent reads—including `NodeView::member_provinces`,
  `TradeNodes::route_target_key`, `NodeView::route_target_key`, and
  `NodeView::outgoing_target_keys`—through that build's profile, failing closed (`None`
  or `Err`) on unknown digests, uppercase/truncated hashes, or out-of-bounds image sizes.

## Reusable Host Mechanics (`foch_runtime::host`)

`foch_runtime::host` packages PE validation and inline-hooking mechanics shared with
`foch-runtime-host` and the test runner:

- **Artifact validation (`host::resolve`, `host::read`, `host::validate_host`, `host::validate_plugin`)**:
  Parses AMD64 PE32+ headers and export directories to confirm host DLLs export all 17
  `VERSION.dll` proxy ordinals (`PROXY_EXPORTS`) without `foch_plugin_get_api`, and
  plugin DLLs export `foch_plugin_get_api` without `VERSION` exports.
- **`host::image`**: Bounds-checked PE32+ section inspection (`inspect`,
  `executable_ranges`, `fits_page`, `gate_fits_page`, `GATE_BYTES = 14`) ensuring the
  14-byte entry gate and hook windows sit inside executable sections and single pages.
- **`host::prologue`**: x64 instruction length decoder (`steal_len`, `rip_operands`,
  `RipOperand`, `MIN_STEAL = 14`, `MAX_STEAL = 28`) that selects the first complete
  14..=28 byte prologue window, refuses relative branches or terminators, and reports
  RIP-relative `disp32` operand offsets.
- **`host::query` *(Windows x64)***: `VirtualQuery` committed-page checks (`executable`,
  `readable`) that reject unmapped, `PAGE_GUARD`, or `PAGE_NOACCESS` regions.
- **`host::link` *(Windows x64)***: Address `Linker` (`hook_create`, `hook_set_enabled`,
  `patch_create`, `patch_set_enabled`, `find_in_module`) providing priority-ordered
  detour chains, executable-section pattern scans, range-constrained trampoline
  allocation within `disp32` reach of stolen RIP-relative operands, and `disp32`
  relocation.

## Memory Safety, Overhaul-Mod Ceilings & Verification Boundary

### Checked-Copy Memory Contract

The shared data plane is pinned to compiled-in EU4 builds. Windows `Memory` accessors use
checked kernel copies (`ReadProcessMemory` / `WriteProcessMemory` on the current process,
plus a `VirtualQuery` write pre-filter that leaves `PAGE_GUARD` pages untouched) and
reject addresses at or below the null-page gate (`0x1_0000`) or crossing unmapped
boundaries. Writes (`Memory::write_*`, `CountryView::pay`, `Eu4Game::pay_country`,
`RouteMutation::set_closed`) are `unsafe` and may transfer a prefix before a failure with
no rollback. Neither reads nor writes establish object type, lifetime, or allocation
bounds. Plugins must verify executable identity, game thread stability, object bounds,
and aliasing contracts before reading or mutating live game state.

### Overhaul-Mod Ceilings & Edge-Case Guards

- **Countries (`1..=i16::MAX`)**: Registry counts (`Eu4Game::countries`) and war
  participant vectors scale up to `32,767` countries so overhaul mods are not truncated
  at 1,024 tags.
- **Slot 0 `"---"` sentinel skip**: `CountryView::is_sentinel_tag` identifies EU4's
  `"---\0"` dummy slot, and `Eu4Game::read_world` / `dataplane::war::read_world` skip it
  while still refusing empty or corrupt country registries.
- **Trade nodes (`1..=4096`) & members (`<= 65_536`)**: `Eu4Game::trade_nodes` accepts
  `1..=MAX_TRADE_NODES` (`4096`) to cover both small regional maps and large overhaul
  trade networks, and `NodeView::member_provinces` accepts up to `MAX_NODE_MEMBERS`
  (`65_536`) positive province IDs.
- **Null-vector end nodes**: `TradeNodes::routes` and `StdVector::read` accept
  default-constructed empty MSVC vectors (`begin == 0, end == 0`) used by vanilla
  sink/end trade nodes.

### Honest Verification Status & War Provider Boundary

- **`live_verified` vs. `static_only`**: Only fields probed in automated live EU4
  sessions carry `live_verified` in the JSON profile (such as `hooks.monthly_tick`,
  `globals.context`, `globals.country_manager`, trade node/entry/route pointers, province
  base tax/manpower, `country.tag`, `country.treasury`, and `ui.*`). Statically reversed
  fields—including `def.members`, `route.target_key`, `globals.flag_names`, `flags.*`,
  `country.monthly_income_bank`, `country.overlord`, `country.subjects_vec`,
  `country.tribute_type`, `campaign.*`, `war.*`, and `diplomacy.*`—remain `static_only`
  until verified in live Foch game sessions.
- **War provider contract (`foch_runtime::war`)**: The `war` module defines a provider
  contract (`WarBridge`, `World`, `War`, `WarId`, `Side`, `JoinRequest`, `JoinReceipt`,
  `JoinRefusal`), including exact war identity, side, and eligibility/entry requests. A
  native war provider is not yet implemented, and the `directed-pacts` DLL currently
  refuses gameplay activation. The read-only EU4 adapter (`dataplane::war`, re-exported
  as `war::eu4`) reads original principals, current participants, and the entering
  country's subject/truce fields through shared `Memory`. These fields are not yet
  verified in game sessions, and those checks alone do not establish permission to join a
  war.

See [DLL deployment](../foch-runtime-host/README.md) for host build, installation, and
artifact validation commands, and [runtime tests](../../../docs/foch-runtime-tests.md)
for test annotations and runner usage.
