# Bilateral peace window preview

This unpublished Windows x64 plugin is an incomplete preview for EU4 1.37.5.
It keeps the native demand and concession drafts and displays both in the
peace description. The two tabs select which side to edit.

**Mixed offers cannot be sent.** The preview preserves both drafts and refuses
submission when both contain terms. Clear one side to use the original
single-direction peace action. Native validation, AI acceptance, and actual
two-direction settlement remain unimplemented. Startup alone does not verify
the in-game window.

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
