# Foch runtime services

This package is the reusable Rust library for typed EU4 services, native plugin
plans, host artifact validation and the isolated game test runner. It builds
only an rlib. Native DLL loading and the VERSION proxy are in the independent
[foch-runtime-host](../foch-runtime-host/README.md) package.

The default `runner` feature provides isolated test sessions. Plugins consume
game services with `default-features = false`; this does not bring a host
DllMain or VERSION linker definition into their dependency graph. Building a
host and plugins together uses separate DLL artifact names.

The shared data plane is pinned to one EU4 build. Windows memory accessors use
checked kernel copies; writes are unsafe and may transfer a prefix before a
failure. Neither reads nor writes establish object type, lifetime or allocation
bounds. Plugins must verify executable identity, thread, object bounds and
access contracts before using live game state.

The war module defines a provider contract, including exact war identity,
side and eligibility/entry requests. A native war provider is not yet
implemented, and the directed-pacts DLL currently refuses gameplay activation.
The read-only EU4 adapter reads original principals, current participants and
the entering country's subject/truce fields through shared Memory. These
fields are not yet verified in game sessions, and those checks alone do not
establish permission to join a war.

See [DLL deployment](../foch-runtime-host/README.md) for host build, installation
and artifact validation commands, and [runtime tests](../../../docs/foch-runtime-tests.md)
for test annotations and runner usage.
