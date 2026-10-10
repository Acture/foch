//! The in-process EU4 DLL loader. Deploy the cdylib as `VERSION.dll` only
//! inside a Foch-owned runtime layer; see this package's README.

#[cfg(any(test, all(windows, target_arch = "x86_64")))]
mod image;
pub mod pattern;
pub mod plan;

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;
