//! The in-process EU4 DLL loader. Deploy the cdylib as `VERSION.dll` only
//! inside a Foch-owned runtime layer; see this package's README.

#[cfg(any(test, all(windows, target_arch = "x86_64")))]
mod image;
pub mod pattern;
pub mod plan;
// The Windows linker is the only caller. Tests still build the decoder on
// every platform; a Linux library build does not.
#[cfg(any(test, all(windows, target_arch = "x86_64")))]
mod prologue;

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;
