# Foch Windows DLL host

`foch-runtime-host` is an unpublished Windows x64 cdylib. It implements the
Foch plugin ABI and the VERSION proxy used inside a Foch-owned runtime layer.
The reusable `foch-runtime` library contains game services and the test runner;
it has no DLL entry point or proxy linker settings.

Build the CLI and host from the repository root:

```text
cargo build --locked -p foch-cli -p foch-runtime-host
```

The build creates `foch.exe` and `foch_runtime_host.dll`. For a source build,
the CLI finds the latter beside itself. For an installed CLI, extract the
optional Windows host bundle beside `foch.exe`; its DLL is named
`foch_runtime.dll`. If both are present, the development artifact takes
precedence. `foch plugin launch --host-dll PATH` selects an explicit host.

The file mapping is:

```text
build: foch_runtime_host.dll → host bundle: foch_runtime.dll → runtime: VERSION.dll
```

The ordinary CLI install does not require this DLL. Plugin launch and
`foch test MOD --with-plugins PLAYSET` report a missing-host error when neither
artifact exists. The loader stages validated DLL bytes into a separate runtime
layer; game and source mod installations remain read-only.

Validate a mixed host/plugin build and package the optional host locally:

```text
cargo build --locked -p foch-runtime-host -p foch-plugin-trade-blackhole
uv run --locked --project src/tools/foch-dev python -m foch_dev runtime-host check --host target/debug/foch_runtime_host.dll --plugin target/debug/foch_plugin_trade_blackhole.dll
uv run --locked --project src/tools/foch-dev python -m foch_dev runtime-host bundle --host target/debug/foch_runtime_host.dll --out target/host-bundle
```

The bundle is `foch-runtime-host-VERSION-win32-x64.zip`, carrying the compatible
distribution filename, license texts and SHA-256 checksums. Packaging and runtime
staging check actual PE exports without loading DLLs. Host exports must retain
the 17 VERSION names and ordinals; a plugin entry cannot be present in a host,
and VERSION host exports cannot be present in a native plugin.

Loading a plugin is not evidence that its game behavior works. Each plugin
reports its own active/refused/failed state and compatibility reasons. No hook,
version, executable identity, page-boundary or instruction relocation guard is
relaxed by this package split.
