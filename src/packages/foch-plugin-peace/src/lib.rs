//! Native EU4 peace editor with terms flowing in both directions.

pub mod composition;
pub mod native;
pub mod profile;
pub mod ui;

#[cfg(all(windows, target_arch = "x86_64"))]
mod bridge;
#[cfg(all(windows, target_arch = "x86_64"))]
mod runtime;

/// # Safety
/// The caller must obey the Foch native-plugin ABI lifetime and threading rules.
#[cfg(all(windows, target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foch_plugin_get_api(
	host_abi_major: u32,
) -> *const foch_plugin_abi::FochPluginApi {
	runtime::api(host_abi_major)
}
