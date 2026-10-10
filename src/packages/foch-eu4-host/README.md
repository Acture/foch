# EU4 DLL host

This unpublished Rust package builds the Windows x64 `foch_eu4_host.dll`.
Deploy it as **`VERSION.dll` in a Foch-owned runtime layer**, beside that
layer's `eu4.exe`. It executes a frozen launch plan; it does not install
packages, choose versions, scan DLL directories, or alter the game installation.

The management core in `foch::plugin` resolves actual installed artifacts and
configuration. `foch plugin launch` uses the shared `foch-runner` runtime layer
to copy selected files, write the frozen host plan below, and start the game.
It never loads a plugin in the management process.

## Management commands

Build `foch-cli` and the host locally. Import a package containing a
`foch-plugin.toml`, or bind an extracted upstream release to a built-in adapter:

```powershell
cargo build -p foch-cli -p foch-eu4-host --release --locked
foch plugin import ./EU4UnicodePatch --adapter io.github.yozoratempest.eu4-unicode-patch
foch plugin import ./EU4MenuPatch --adapter io.github.yozoratempest.eu4-menu-patch
foch plugin enable io.github.yozoratempest.eu4-unicode-patch --playset default --version 0.1.14
foch plugin enable io.github.yozoratempest.eu4-menu-patch --playset default --version 0.1.4
foch plugin plan --playset default --game-path "G:/SteamLibrary/steamapps/common/Europa Universalis IV"
foch plugin launch --playset default --host-dll ./target/release/foch_eu4_host.dll
foch plugin status --run-dir "<runtime directory printed by launch>"
```

Extract ZIP releases before importing. Optional Unicode fonts must be placed in
the extracted package's `plugins/eu4_unicode_patch` directory before importing.
Built-in adapter metadata alone is not an installed artifact: missing,
ambiguous or modified selected versions are refused before launch.

`launch --prepare-only` writes the same reviewable layer without starting EU4.
`--runtime-base` chooses a short local directory; `--user-dir` chooses a game
profile (default: the player's usual EU4 directory). Extra game arguments follow
`--`. Without `--host-dll`, launch looks beside the `foch` executable for
`foch_eu4_host.dll`. Both `launch` and `status` support `--format json`.

Preparation excludes old `VERSION`, `d3d9`, `dinput8`, `winmm`, `dxgi` proxies
and the installation's `plugins` directory, recording exclusions in
`foch-host/run.json`. Plugin resources and INI configuration are copied into
separate package directories. The layer owns `gfx/fonts`; Unicode's generated
font cache is linked to Foch data and keyed by package/base-font identity.
Every Foch-managed writable destination is checked against the source
installation before creation, including nested data junctions; parent traversal
is refused.

Other game asset directories are shared through directory links. The layer
isolates Foch's deployment, plugin configuration and known font/cache writes;
it is not a filesystem sandbox. Game and native DLL code runs with the player's
permissions and can write through shared links or access original paths.
Installed plugin/version directories and manifests must be ordinary entries
inside the configured store; symbolic links and reparse points are refused.

Player layers are retained when management exits, and test-session sweeping
skips them. Close the game before removing a retained layer; unlink its data
junctions without traversing them. Configuration/enable changes apply to the
next launch. `status` reads complete JSONL events from this run and does not
equate a successful DLL load with an active plugin.

## Build and verification

```powershell
cargo build -p foch-eu4-host --release --locked
cargo test -p foch-eu4-host -p foch-plugin-abi --locked
cargo clippy -p foch-eu4-host -p foch-plugin-abi --all-targets --locked -- -D warnings
```

On Windows x64 the build artifact is `target/release/foch_eu4_host.dll`.
Rename/copy it to `VERSION.dll` only during runtime-layer preparation. The host
and SDK remain outside the crates.io dependency closure of `foch-cli`.
Other platforms build the portable plan/pattern library, without a Windows
proxy. Real DLL integration tests run only on Windows x64 and require MSVC.

The integration test compiles disposable C DLLs and a small `eu4.exe`; it
checks all 17 named and ordinal exports against system VERSION, calls a version
API through the proxy, and verifies native initialization before executable
CRT constructors. It also checks legacy polling/refusal/timeout, asynchronous
native callbacks, malformed ABI tables, invalid status strings/pointers, entry digests,
Unicode runtime paths, missing plans/exports and unrelated executables.
The C fixtures exercise the SDK header against the Rust host directly.

The management-to-host test also stages installed native/legacy packages,
checks that management and host reject the same malformed wire entries, and
executes the resulting plan in the disposable executable.

Verified locally on 2026-10-11 with EU4 1.37.5 x64: official Unicode 0.1.14
reports `active` at `entry` and reaches the main menu/country-selection screen.
Its generated font cache leaves original fonts intact. Official Menu
0.1.4-experimental reports `active` alone and alongside Unicode; both selected
modules come from the Foch layer and the old plugin64 module is excluded.
Unicode's 348-site and Menu's 26-site executable checks pass. The Menu/combined
probes ran on an inactive Windows desktop, where Direct3D device creation fails;
they establish initialization compatibility, not gameplay or menu transitions.
Chinese rendering/input, Return to Menu and save reload remain manual checks.
The installed old `Plugin.dll` is x86 and rejected; old `plugin64.dll` has no
status export and has no verified Foch adapter.

## Startup

The proxy forwards 17 VERSION exports by name and matching system ordinal to
the absolute system-directory DLL using argument-preserving x64 tail jumps.
System forwarding resolves once at the entry gate, outside `DllMain`. For
forwarding-only loads it resolves lazily on the first VERSION call, preserving
arguments and Win32 last-error state; consumers must call these APIs outside
their own `DllMain`. Concurrent first calls share the same retained system DLL.
During static DLL startup it installs
a one-shot gate only when the executable is named `eu4.exe`. A dynamic load
of the proxy, or an unrelated executable, receives forwarding only.

The gate runs before executable CRT initialization. It restores the original
entry bytes and page protection, flushes the instruction cache, synchronously
initializes `entry` plugins, and returns to the original entry with the volatile
registers and flags preserved. No plugin load or plugin API call occurs in
the proxy's `DllMain`. `entry` initialization calls precede the game CRT;
a worker waits for pending entry initialization to reach a terminal state
before loading and polling `deferred` plugins.

Missing or invalid plans load no plugins and preserve forwarding. A malformed
plan or a host preparation error is written to the existing runtime directory's
`foch-host/host-error.txt`. The host does not create that directory inside an
arbitrary installation. If the gate cannot be installed, Windows rejects the
proxy at startup; if another DLL changes the gate before it executes, the
process exits with code 126 rather than overwriting the other change. Cross-page
gates are refused so both writes preserve the page's original protection.
If writing a gate fails after changing bytes, it rolls back the original bytes,
flushes the instruction cache and restores protection before rejecting startup.
An unrecoverable rollback exits the process rather than unloading a proxy that
the executable entry might still target.

`process_attach` is a reserved legacy fallback, **currently refused**. It must
not silently become `entry` or `deferred`; its necessity first requires E1.

## Frozen plan, format 1

The plan is UTF-8 JSON at `foch-host/plan.json`, relative to `VERSION.dll`.
Unknown fields, unsupported format versions and plans over 1 MiB are refused. Paths are absolute
Windows paths; entry DLLs must resolve inside the runtime layer. Entries are
loaded in array order, with all `entry` entries before any `deferred` entries.
Disabled plugins are omitted entirely. The host never computes another order.

```json
{
  "format": 1,
  "run_id": "run-2026-10-11-001",
  "game_version": "1.37.5.0",
  "events": "C:\\foch-runtime\\foch-host\\events.jsonl",
  "plugins": [
    {
      "id": "dev.foch.sample",
      "version": "0.1.0",
      "kind": "native",
      "phase": "entry",
      "path": "C:\\foch-runtime\\plugins\\sample.dll",
      "abi_major": 1,
      "config_json": "{\"greeting\":\"hi\"}",
      "dirs": {
        "data": "C:\\foch-data\\plugins\\sample",
        "cache": "C:\\foch-data\\plugins\\sample\\cache",
        "log": "C:\\foch-runtime\\plugins\\logs",
        "plugin": "C:\\foch-runtime\\plugins"
      }
    },
    {
      "id": "io.github.yozoratempest.eu4-menu-patch",
      "version": "0.1.4-experimental",
      "kind": "legacy",
      "phase": "deferred",
      "path": "C:\\foch-runtime\\plugins\\eu4_menu_patch.dll",
      "status": {
        "export": "EU4MenuPatchStatus",
        "mode": "poll",
        "timeout_ms": 30000,
        "pending": [0],
        "values": [
          {"value": 1, "state": "active"},
          {"value": -1, "state": "refused", "reason": "host identity"},
          {"value": -2, "state": "refused", "reason": "compatibility check"},
          {"value": -3, "state": "refused", "reason": "target already modified"},
          {"value": -4, "state": "failed", "reason": "hook preparation"}
        ]
      }
    }
  ]
}
```

An entry may additionally specify `sha256`, a 64-character hex digest. The
host verifies it before executing the DLL and holds a read-only shared file
handle through loading to prevent concurrent replacement during that check.
Production deployment should bind each entry to its selected artifact digest.
Configuration defaults to `"{}"` and must encode a JSON object. Directories
are supplied by management; absent data/cache/log directories return an empty
ABI string, while the plugin directory defaults to the entry DLL's parent.

Dependencies are resolved from the entry DLL directory and System32, without
changing the process's search directories. Existing process modules still
follow Windows module reuse rules. Management must reject conflicting
dependency DLL names/versions before launch.

## Native ABI and legacy status

Native DLLs export `foch_plugin_get_api` with the SDK's C ABI. The host checks
the returned major version, readable struct prefix and required executable
callbacks before calling `init` once. It supplies stable, process-lifetime
strings, executable image identity, configuration, directories, thread-safe
logging/reporting and other plugins' actual states. Native initialization is
polled while it reports `initializing`; subsequent transitions can use `report`.
A malformed status or unreadable/invalid UTF-8 detail stops initialization
polling and records terminal `failed`. Unaligned report buffers are copied
without forming an unaligned Rust reference.

The draft SDK's shared pattern/hook/patch slots retain their existing layout,
but this first-version loader returns `FOCH_E_UNSUPPORTED` for those services,
as the published first-version design excludes shared hook management.
It does not currently send live configuration/toggle or shutdown callbacks.
Enable/configuration changes take effect on the next launch. Loaded DLLs and
host callback storage are retained until process exit; there is no hot unload.
Plugins must keep exceptions/panics within their own ABI boundary.

Legacy status uses a no-argument export returning a signed 32-bit integer.
`sync` reads once; `poll` requires explicit pending values and a timeout of
1–120000 ms. A pending timeout, missing export or absent status interface is
`unknown`, not `active`. Unmapped integer results are `failed`; listed values
map only to `active`, `inactive`, `refused`, `failed` or `unknown`.
DLL-load success is separately recorded as `dll_loaded`.

## Event stream

Events append to the plan's absolute `events` path as UTF-8 JSONL. The parent
directory must already exist. Writes from initialization, worker polling and
plugin threads are serialized; each line is a complete JSON object.

```json
{"format":1,"event":"state","run_id":"run-2026-10-11-001","plugin_id":"dev.foch.sample","version":"0.1.0","phase":"entry","pid":1234,"timestamp_ms":1791650000000,"state":"active","reason":null,"reason_code":0}
```

State events use `loading`, `dll_loaded`, `initializing`, `active`, `inactive`,
`refused`, `failed` and `unknown`. A log event has `event: "log"`, numeric ABI
`level` and UTF-8 `message`, with the same run/plugin identity fields. `log_path`
is included when the plan supplied a log directory. This is one-way startup
reporting: a management process may close/reconnect without ending the game.

Windows references: [DLL initialization guidance](https://learn.microsoft.com/en-us/windows/win32/dlls/dynamic-link-library-best-practices),
[LoadLibraryExW search flags](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-loadlibraryexw),
[PE headers](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format).
