//! Windows x64 DLL host. Never link this package into a CLI or plugin.
//! Its independent cdylib keeps DllMain, VERSION exports and linker metadata
//! out of the reusable runtime library, including mixed workspace builds.

#[cfg(all(windows, target_arch = "x86_64"))]
use foch_runtime::{pattern, plan};

#[cfg(any(test, all(windows, target_arch = "x86_64")))]
mod image;
#[cfg(any(test, all(windows, target_arch = "x86_64")))]
mod prologue;
#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;
