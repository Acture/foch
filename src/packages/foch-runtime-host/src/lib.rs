//! Windows x64 DLL host. Never link this package into a CLI or plugin.
//! Its independent cdylib keeps DllMain, VERSION exports and linker metadata
//! out of the reusable runtime library, including mixed workspace builds.
//!
//! Reusable PE/prologue/trampoline mechanics live in `foch_runtime::host`.

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;
