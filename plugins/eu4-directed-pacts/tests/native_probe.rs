use foch_plugin_directed_pacts::foch_plugin_get_api;
use foch_plugin_sdk::*;
use std::{
	ffi::c_void,
	sync::atomic::{AtomicUsize, Ordering},
};

static CONFIG: AtomicUsize = AtomicUsize::new(0);
static FIND: AtomicUsize = AtomicUsize::new(0);
static HOOKS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn config(_: *const FochHostApi) -> FochStr {
	FochStr::from_str(match CONFIG.load(Ordering::Relaxed) {
		1 => r#"{"read_only_roster_probe":true}"#,
		2 => r#"{"read_only_roster_probe":true,"probe_request":"0"}"#,
		_ => "{}",
	})
}
unsafe extern "C" fn log(_: *const FochHostApi, _: i32, _: FochStr) {}
unsafe extern "C" fn report(_: *const FochHostApi, _: *const FochStatus) {}
unsafe extern "C" fn dir(_: *const FochHostApi, _: i32) -> FochStr {
	FochStr::EMPTY
}
unsafe extern "C" fn plugin_state(_: *const FochHostApi, _: FochStr) -> i32 {
	state::INACTIVE
}
unsafe extern "C" fn find(_: *const FochHostApi, _: FochStr, _: *mut *mut c_void) -> i32 {
	FIND.fetch_add(1, Ordering::Relaxed);
	result::E_UNSUPPORTED
}
unsafe extern "C" fn create(
	_: *const FochHostApi,
	_: *mut c_void,
	_: *mut c_void,
	_: i32,
	_: *mut *const *const c_void,
	_: *mut *mut FochHook,
) -> i32 {
	HOOKS.fetch_add(1, Ordering::Relaxed);
	result::E_UNSUPPORTED
}
unsafe extern "C" fn enable(_: *const FochHostApi, _: *mut FochHook, _: i32) -> i32 {
	result::E_UNSUPPORTED
}
unsafe extern "C" fn patch(
	_: *const FochHostApi,
	_: *mut c_void,
	_: *const u8,
	_: usize,
	_: *mut *mut FochPatch,
) -> i32 {
	result::E_UNSUPPORTED
}
unsafe extern "C" fn patch_enable(_: *const FochHostApi, _: *mut FochPatch, _: i32) -> i32 {
	result::E_UNSUPPORTED
}

#[test]
fn native_probe_checks_the_actual_host_digest_before_resolving_or_installing_any_hook() {
	let mut host = FochHostApi {
		size: size_of::<FochHostApi>() as u32,
		abi_major: ABI_MAJOR,
		abi_minor: ABI_MINOR,
		plugin_id: FochStr::from_str("dev.foch.directed-pacts"),
		run_id: FochStr::from_str("unit"),
		game_version: FochStr::from_str("1.37.5"),
		exe_sha256: FochStr::EMPTY,
		exe_base: 0x140000000usize as *mut c_void,
		exe_size: 0x3000000,
		log,
		report,
		dir,
		config_json: config,
		plugin_state,
		find_pattern: find,
		hook_create: create,
		hook_set_enabled: enable,
		patch_create: patch,
		patch_set_enabled: patch_enable,
	};
	let api = unsafe { &*foch_plugin_get_api(ABI_MAJOR) };
	let read_status = || {
		let mut status = FochStatus::new(0);
		unsafe { (api.status)(&mut status) };
		status
	};
	assert_eq!(unsafe { (api.init)(&host) }, result::OK);
	assert_eq!(read_status().state, state::REFUSED);
	assert_eq!(read_status().reason_code, 201); // No explicit probe: never active.
	CONFIG.store(1, Ordering::Relaxed);
	assert_eq!(unsafe { (api.init)(&host) }, result::OK);
	assert_eq!(read_status().reason_code, 211); // Pact identity cannot activate diagnostics.
	assert_eq!(read_status().state, state::REFUSED);
	assert_eq!(FIND.load(Ordering::Relaxed), 0);
	host.plugin_id = FochStr::from_str("dev.foch.directed-pacts.roster-probe");
	let uppercase = foch_runtime::dataplane::pinned_exe_sha256().to_ascii_uppercase();
	for digest in ["", "1.37.5", "c9ec55a4", &uppercase] {
		host.exe_sha256 = FochStr::from_str(digest);
		assert_eq!(unsafe { (api.init)(&host) }, result::OK);
		assert_eq!(read_status().reason_code, 210);
		assert_eq!(read_status().state, state::REFUSED);
		assert_eq!(FIND.load(Ordering::Relaxed), 0);
		assert_eq!(HOOKS.load(Ordering::Relaxed), 0);
	}
	host.exe_sha256 = FochStr::from_str(foch_runtime::dataplane::pinned_exe_sha256());
	CONFIG.store(2, Ordering::Relaxed);
	assert_eq!(unsafe { (api.init)(&host) }, result::OK);
	assert_eq!(read_status().reason_code, 211);
	assert_eq!(FIND.load(Ordering::Relaxed), 0);
	CONFIG.store(1, Ordering::Relaxed);
	assert_eq!(unsafe { (api.init)(&host) }, result::OK);
	assert_eq!(read_status().reason_code, 212); // Resolver refusal is preserved.
	assert_eq!(read_status().state, state::REFUSED);
	assert_eq!(
		FIND.load(Ordering::Relaxed),
		usize::from(cfg!(all(windows, target_arch = "x86_64")))
	);
	assert_eq!(HOOKS.load(Ordering::Relaxed), 0);
	#[cfg(windows)]
	unsafe {
		distinct_dll_filenames_keep_the_actual_native_status_separate(&mut host)
	};
}

#[cfg(windows)]
unsafe fn distinct_dll_filenames_keep_the_actual_native_status_separate(host: &mut FochHostApi) {
	use std::os::windows::ffi::OsStrExt;
	use windows_sys::Win32::{
		Foundation::FreeLibrary,
		System::LibraryLoader::{GetProcAddress, LoadLibraryW},
	};
	let root = tempfile::tempdir().unwrap();
	let source = std::env::current_exe()
		.unwrap()
		.parent()
		.unwrap()
		.join("foch_plugin_directed_pacts.dll");
	assert!(
		source.is_file(),
		"build the actual plugin DLL before this loader check"
	);
	let load = |name: &str| {
		let path = root.path().join(name);
		std::fs::copy(&source, &path).unwrap();
		let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
		let module = unsafe { LoadLibraryW(path.as_ptr()) };
		assert!(!module.is_null());
		let entry = unsafe { GetProcAddress(module, ENTRY_NUL.as_ptr()) }.unwrap();
		let get_api: GetApi = unsafe { std::mem::transmute(entry) };
		let api = unsafe { &*get_api(ABI_MAJOR) };
		(module, api)
	};
	let (pact_module, pact_api) = load("eu4_directed_pacts.dll");
	let (probe_module, probe_api) = load("eu4_directed_pacts_roster_probe.dll");
	assert_ne!(pact_module, probe_module);
	CONFIG.store(0, Ordering::Relaxed);
	host.plugin_id = FochStr::from_str("dev.foch.directed-pacts");
	assert_eq!(unsafe { (pact_api.init)(host) }, result::OK);
	CONFIG.store(1, Ordering::Relaxed);
	host.plugin_id = FochStr::from_str("dev.foch.directed-pacts.roster-probe");
	host.exe_sha256 = FochStr::EMPTY; // This synthetic process is not the pinned game.
	assert_eq!(unsafe { (probe_api.init)(host) }, result::OK);
	for (api, expected_reason) in [(pact_api, 201), (probe_api, 210)] {
		let mut status = FochStatus::new(0);
		unsafe { (api.status)(&mut status) };
		assert_eq!(status.state, state::REFUSED);
		assert_eq!(status.reason_code, expected_reason);
	}
	// No hook was created; neither API retains a callback or game pointer.
	unsafe {
		FreeLibrary(probe_module);
		FreeLibrary(pact_module);
	}
}
