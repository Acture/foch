//! The frozen plan and all native callback storage live until process exit.

use super::{MODULE, gate};
use crate::plan::{self, Kind, Phase, Plan, PlannedPlugin, StatusMode};
use foch_plugin_abi::{self as abi, FochHostApi, FochStatus, FochStr};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
	collections::BTreeMap,
	ffi::{CString, OsString, c_void},
	fs::{File, OpenOptions},
	io::{Read, Write},
	mem::{offset_of, size_of},
	os::windows::{
		ffi::{OsStrExt, OsStringExt},
		fs::OpenOptionsExt,
	},
	path::{Path, PathBuf},
	sync::{
		Mutex,
		atomic::{AtomicBool, Ordering},
	},
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
	Foundation::HMODULE,
	Storage::FileSystem::FILE_SHARE_READ,
	System::{
		LibraryLoader::{
			GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
			GetModuleFileNameW, GetModuleHandleExW, GetProcAddress,
			LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
		},
		Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery},
		Threading::GetCurrentProcessId,
	},
};

struct Runtime {
	plan: Plan,
	root: PathBuf,
	events: Mutex<File>,
	states: Mutex<BTreeMap<String, i32>>,
	exe_hash: String,
	exe_size: usize,
}

#[repr(C)]
struct Context {
	api: FochHostApi,
	runtime: &'static Runtime,
	plugin: &'static PlannedPlugin,
	plugin_dir: String,
	last: Mutex<Option<(String, Option<String>, i32)>>,
	invalid_status: AtomicBool,
}

// SAFETY: the API table and its pointers/strings are immutable and retained
// for process lifetime; mutable state and event writes use mutexes.
unsafe impl Sync for Context {}

impl Context {
	fn event(&self, mut event: Value) {
		event["format"] = 1.into();
		event["run_id"] = self.runtime.plan.run_id.clone().into();
		event["plugin_id"] = self.plugin.id.clone().into();
		event["version"] = self.plugin.version.clone().into();
		event["phase"] = serde_json::to_value(self.plugin.phase).unwrap();
		event["pid"] = unsafe { GetCurrentProcessId() }.into();
		event["timestamp_ms"] = (SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis() as u64)
			.into();
		if let Some(path) = self.plugin.dirs.get("log") {
			event["log_path"] = path.clone().into();
		}
		let Ok(mut file) = self.runtime.events.lock() else {
			return;
		};
		let Ok(mut bytes) = serde_json::to_vec(&event) else {
			return;
		};
		bytes.push(b'\n');
		if let Err(error) = file.write_all(&bytes) {
			diagnostic(&self.runtime.root, &format!("event write failed: {error}"));
		}
	}

	fn state(&self, state: &str, reason: Option<String>, code: i32) {
		// Keep state changes and their event ordering consistent, including
		// concurrent reports made by a plugin's background thread.
		let Ok(mut last) = self.last.lock() else {
			return;
		};
		if self.invalid_status.load(Ordering::Acquire) && state != "failed" {
			return;
		}
		let current = (state.to_owned(), reason.clone(), code);
		if last.as_ref() == Some(&current) {
			return;
		}
		*self
			.runtime
			.states
			.lock()
			.unwrap()
			.entry(self.plugin.id.clone())
			.or_default() = state_number(state);
		self.event(json!({"event":"state", "state":state, "reason":reason, "reason_code":code}));
		*last = Some(current);
	}
}

fn state_number(state: &str) -> i32 {
	match state {
		"initializing" => abi::state::INITIALIZING,
		"active" => abi::state::ACTIVE,
		"inactive" => abi::state::INACTIVE,
		"refused" => abi::state::REFUSED,
		"failed" => abi::state::FAILED,
		_ => 0,
	}
}

fn state_name(state: i32) -> Option<&'static str> {
	match state {
		abi::state::INITIALIZING => Some("initializing"),
		abi::state::ACTIVE => Some("active"),
		abi::state::INACTIVE => Some("inactive"),
		abi::state::REFUSED => Some("refused"),
		abi::state::FAILED => Some("failed"),
		_ => None,
	}
}

pub(super) fn start(exe_size: usize) {
	let module = MODULE.load(Ordering::Acquire) as HMODULE;
	let Some(root) = module_path(module).and_then(|p| p.parent().map(Path::to_owned)) else {
		return;
	};
	if let Err(error) = prepare(&root, module, exe_size) {
		diagnostic(&root, &error);
	}
}

fn prepare(root: &Path, module: HMODULE, exe_size: usize) -> Result<(), String> {
	let path = root.join(plan::PLAN_FILE);
	let file = match File::open(path) {
		Ok(file) => file,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(format!("cannot read plan: {error}")),
	};
	let mut text = String::new();
	file.take(1_048_577)
		.read_to_string(&mut text)
		.map_err(|e| e.to_string())?;
	if text.len() > 1_048_576 {
		return Err("plan exceeds 1 MiB".into());
	}
	let plan = plan::parse(&text)?;
	let events = OpenOptions::new()
		.create(true)
		.append(true)
		.open(&plan.events)
		.map_err(|e| format!("cannot open event stream: {e}"))?;
	// Pin the host before exposing any callback. Plugin modules and their
	// threads are also never unloaded by this host.
	let mut pinned = std::ptr::null_mut();
	if unsafe {
		GetModuleHandleExW(
			GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
			module.cast(),
			&mut pinned,
		)
	} == 0
	{
		return Err("cannot retain host callback module".into());
	}
	let exe_hash = module_path(std::ptr::null_mut())
		.and_then(|p| File::open(p).ok())
		.and_then(|mut file| digest(&mut file).ok())
		.unwrap_or_default();
	let runtime: &'static Runtime = Box::leak(Box::new(Runtime {
		plan,
		root: root.canonicalize().map_err(|e| e.to_string())?,
		events: Mutex::new(events),
		states: Mutex::new(BTreeMap::new()),
		exe_hash,
		exe_size,
	}));
	let pending = load_phase(runtime, Phase::Entry);
	if pending.is_empty()
		&& !runtime
			.plan
			.plugins
			.iter()
			.any(|p| p.phase == Phase::Deferred)
	{
		return Ok(());
	}
	std::thread::Builder::new()
		.name("foch-plugins".into())
		.spawn(move || {
			let mut pending = pending;
			pending.extend(load_phase(runtime, Phase::Deferred));
			while !pending.is_empty() {
				pending.retain_mut(Pending::poll);
				if !pending.is_empty() {
					std::thread::sleep(Duration::from_millis(10));
				}
			}
		})
		.map_err(|error| format!("cannot start deferred plugin worker: {error}"))?;
	Ok(())
}

fn module_path(module: HMODULE) -> Option<PathBuf> {
	let mut path = vec![0u16; 32768];
	let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize;
	(len > 0 && len < path.len()).then(|| PathBuf::from(OsString::from_wide(&path[..len])))
}

fn diagnostic(root: &Path, error: &str) {
	// Do not create a directory inside an arbitrary executable's install.
	// The runtime manager must have created foch-host before launch.
	let _ = std::fs::write(root.join("foch-host/host-error.txt"), error);
}

fn digest(file: &mut File) -> std::io::Result<String> {
	let mut hash = Sha256::new();
	let mut bytes = [0u8; 8192];
	loop {
		let len = file.read(&mut bytes)?;
		if len == 0 {
			break;
		}
		hash.update(&bytes[..len]);
	}
	Ok(format!("{:x}", hash.finalize()))
}

fn context(runtime: &'static Runtime, plugin: &'static PlannedPlugin) -> &'static Context {
	let api = FochHostApi {
		size: size_of::<FochHostApi>() as u32,
		abi_major: abi::ABI_MAJOR,
		abi_minor: abi::ABI_MINOR,
		plugin_id: FochStr::from_str(&plugin.id),
		run_id: FochStr::from_str(&runtime.plan.run_id),
		game_version: FochStr::from_str(&runtime.plan.game_version),
		exe_sha256: FochStr::from_str(&runtime.exe_hash),
		exe_base: gate::image_base(),
		exe_size: runtime.exe_size,
		log,
		report,
		dir,
		config_json,
		plugin_state,
		find_pattern: unsupported_pattern,
		hook_create: unsupported_hook,
		hook_set_enabled: unsupported_hook_toggle,
		patch_create: unsupported_patch,
		patch_set_enabled: unsupported_patch_toggle,
	};
	Box::leak(Box::new(Context {
		api,
		runtime,
		plugin,
		plugin_dir: Path::new(&plugin.path)
			.parent()
			.unwrap_or(Path::new(""))
			.to_string_lossy()
			.into_owned(),
		last: Mutex::new(None),
		invalid_status: AtomicBool::new(false),
	}))
}

fn load_phase(runtime: &'static Runtime, phase: Phase) -> Vec<Pending> {
	let mut pending = Vec::new();
	for plugin in runtime.plan.plugins.iter().filter(|p| p.phase == phase) {
		let ctx = context(runtime, plugin);
		ctx.state("loading", None, 0);
		match load(ctx) {
			Ok(Some(mut item)) => {
				if item.poll() {
					pending.push(item);
				}
			}
			Ok(None) => {}
			Err(error) => ctx.state("refused", Some(error), 0),
		}
	}
	pending
}

fn load(ctx: &'static Context) -> Result<Option<Pending>, String> {
	let path = Path::new(&ctx.plugin.path)
		.canonicalize()
		.map_err(|e| format!("entry DLL unavailable: {e}"))?;
	if !path.starts_with(&ctx.runtime.root) {
		return Err("entry DLL resolves outside the runtime layer".into());
	}
	// Deny concurrent replacement while hashing and loading the selected DLL.
	let mut file = OpenOptions::new()
		.read(true)
		.share_mode(FILE_SHARE_READ)
		.open(&path)
		.map_err(|e| e.to_string())?;
	if let Some(expected) = &ctx.plugin.sha256 {
		let actual = digest(&mut file).map_err(|e| e.to_string())?;
		if !actual.eq_ignore_ascii_case(expected) {
			return Err("entry DLL SHA-256 does not match the frozen plan".into());
		}
	}
	let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
	let module = unsafe {
		LoadLibraryExW(
			wide.as_ptr(),
			std::ptr::null_mut(),
			LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
		)
	};
	if module.is_null() {
		return Err(format!(
			"LoadLibraryExW failed: {}",
			std::io::Error::last_os_error()
		));
	}
	ctx.state("dll_loaded", None, 0);
	match ctx.plugin.kind {
		Kind::Legacy => {
			let Some(rule) = &ctx.plugin.status else {
				ctx.state(
					"unknown",
					Some("DLL loaded without a status interface".into()),
					0,
				);
				return Ok(None);
			};
			let name = CString::new(rule.export.as_str()).unwrap();
			let Some(function) = (unsafe { GetProcAddress(module, name.as_ptr().cast()) }) else {
				ctx.state(
					"unknown",
					Some(format!("status export {} is missing", rule.export)),
					0,
				);
				return Ok(None);
			};
			if !executable(function as *const c_void) {
				ctx.state(
					"unknown",
					Some("legacy status export is not executable".into()),
					0,
				);
				return Ok(None);
			}
			let status: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(function) };
			Ok(Some(Pending {
				ctx,
				source: Source::Legacy(status),
				since: Instant::now(),
			}))
		}
		Kind::Native => {
			let function = unsafe { GetProcAddress(module, abi::ENTRY_NUL.as_ptr()) }
				.ok_or("native API export is missing")?;
			if !executable(function as *const c_void) {
				return Err("native API export is not executable".into());
			}
			let get_api: abi::GetApi = unsafe { std::mem::transmute(function) };
			let (init, status) = unsafe { native_api(get_api(abi::ABI_MAJOR)) }?;
			ctx.state("initializing", None, 0);
			let code = unsafe { init(&ctx.api) };
			if code != abi::result::OK {
				ctx.state("failed", Some(format!("native init returned {code}")), code);
				return Ok(None);
			}
			Ok(Some(Pending {
				ctx,
				source: Source::Native(status),
				since: Instant::now(),
			}))
		}
	}
}

type Init = unsafe extern "C" fn(*const FochHostApi) -> i32;
type Status = unsafe extern "C" fn(*mut FochStatus);

/// Read the negotiated prefix without forming a Rust reference containing
/// invalid non-nullable function pointers from a malformed C table.
unsafe fn native_api(api: *const abi::FochPluginApi) -> Result<(Init, Status), String> {
	let ptr = api.cast::<u8>();
	if !readable(ptr, 4) {
		return Err("native API table is null or unreadable".into());
	}
	let size = unsafe { ptr.cast::<u32>().read_unaligned() } as usize;
	let required = offset_of!(abi::FochPluginApi, status) + size_of::<usize>();
	if size < required || !readable(ptr, required) {
		return Err("native API table is too short".into());
	}
	let major = unsafe { ptr.add(4).cast::<u32>().read_unaligned() };
	if major != abi::ABI_MAJOR {
		return Err(format!("native ABI major {major} is unsupported"));
	}
	let init = unsafe {
		ptr.add(offset_of!(abi::FochPluginApi, init))
			.cast::<Option<Init>>()
			.read_unaligned()
	}
	.ok_or("native init is null")?;
	let status = unsafe {
		ptr.add(offset_of!(abi::FochPluginApi, status))
			.cast::<Option<Status>>()
			.read_unaligned()
	}
	.ok_or("native status is null")?;
	if !executable(init as *const c_void) || !executable(status as *const c_void) {
		return Err("native callback does not point to executable memory".into());
	}
	Ok((init, status))
}

fn executable(ptr: *const c_void) -> bool {
	let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
	let queried = unsafe { VirtualQuery(ptr, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
	queried != 0
		&& info.State == MEM_COMMIT
		&& info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
		&& info.Protect & 0xf0 != 0
}

fn readable(ptr: *const u8, len: usize) -> bool {
	if ptr.is_null() {
		return false;
	}
	let Some(end) = (ptr as usize).checked_add(len) else {
		return false;
	};
	let mut at = ptr as usize;
	while at < end {
		let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
		if unsafe {
			VirtualQuery(
				at as *const c_void,
				&mut info,
				size_of::<MEMORY_BASIC_INFORMATION>(),
			)
		} == 0 || info.State != MEM_COMMIT
			|| info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
		{
			return false;
		}
		let Some(next) = (info.BaseAddress as usize).checked_add(info.RegionSize) else {
			return false;
		};
		if next <= at {
			return false;
		}
		at = next;
	}
	true
}

enum Source {
	Native(Status),
	Legacy(unsafe extern "C" fn() -> i32),
}

struct Pending {
	ctx: &'static Context,
	source: Source,
	since: Instant,
}

impl Pending {
	/// Initialization polling only; terminal plugins can still report later
	/// transitions asynchronously through their retained host API.
	fn poll(&mut self) -> bool {
		if self.ctx.invalid_status.load(Ordering::Acquire) {
			return false;
		}
		match self.source {
			Source::Native(status) => {
				let mut out = FochStatus::new(abi::state::INITIALIZING);
				unsafe {
					status(&mut out);
				}
				publish(self.ctx, &out) && out.state == abi::state::INITIALIZING
			}
			Source::Legacy(status) => {
				let value = unsafe { status() };
				let rule = self.ctx.plugin.status.as_ref().unwrap();
				if rule.mode == StatusMode::Poll && rule.pending.contains(&value) {
					if self.since.elapsed() >= Duration::from_millis(rule.timeout_ms) {
						self.ctx.state(
							"unknown",
							Some(format!(
								"{} remained pending after {} ms",
								rule.export, rule.timeout_ms
							)),
							value,
						);
						return false;
					}
					self.ctx.state("initializing", None, value);
					return true;
				}
				let (state, reason) = rule.interpret(value);
				self.ctx.state(state, reason, value);
				false
			}
		}
	}
}

fn invalid_status(ctx: &Context, reason: &str) {
	ctx.invalid_status.store(true, Ordering::Release);
	ctx.state("failed", Some(reason.into()), abi::result::E_INVALID);
}

fn publish(ctx: &Context, status: &FochStatus) -> bool {
	if ctx.invalid_status.load(Ordering::Acquire) {
		return false;
	}
	if status.size < size_of::<FochStatus>() as u32 || state_name(status.state).is_none() {
		invalid_status(ctx, "plugin returned an invalid status structure");
		return false;
	}
	let detail = match unsafe { text(status.detail) } {
		Ok(detail) => detail,
		Err(error) => {
			invalid_status(ctx, error);
			return false;
		}
	};
	ctx.state(
		state_name(status.state).unwrap(),
		detail,
		status.reason_code,
	);
	!ctx.invalid_status.load(Ordering::Acquire)
}

unsafe fn text(value: FochStr) -> Result<Option<String>, &'static str> {
	if value.len == 0 {
		return Ok(None);
	}
	if value.len > 65536 || !readable(value.ptr, value.len) {
		return Err("plugin string is oversized, null or unreadable");
	}
	std::str::from_utf8(unsafe { core::slice::from_raw_parts(value.ptr, value.len) })
		.map(|text| Some(text.to_owned()))
		.map_err(|_| "plugin string is not valid UTF-8")
}

// SAFETY: ABI callers pass the exact process-lifetime table supplied by init.
unsafe fn from_host(host: *const FochHostApi) -> Option<&'static Context> {
	if host.is_null() {
		None
	} else {
		Some(unsafe { &*host.cast::<Context>() })
	}
}

unsafe extern "C" fn log(host: *const FochHostApi, level: i32, message: FochStr) {
	if let Some(ctx) = unsafe { from_host(host) } {
		ctx.event(
			json!({"event":"log", "level":level, "message":unsafe { text(message) }
				.unwrap_or_else(|error| Some(error.into())).unwrap_or_default()}),
		);
	}
}

unsafe extern "C" fn report(host: *const FochHostApi, status: *const FochStatus) {
	if let Some(ctx) = unsafe { from_host(host) } {
		if !readable(status.cast(), size_of::<FochStatus>()) {
			invalid_status(ctx, "reported status is null or unreadable");
			return;
		}
		// The C ABI may supply a byte buffer rather than an aligned struct.
		// Copy before validation, without forming an unaligned Rust reference.
		publish(ctx, &unsafe { status.read_unaligned() });
	}
}

unsafe extern "C" fn dir(host: *const FochHostApi, kind: i32) -> FochStr {
	let Some(ctx) = (unsafe { from_host(host) }) else {
		return FochStr::EMPTY;
	};
	let key = match kind {
		abi::dir::DATA => "data",
		abi::dir::CACHE => "cache",
		abi::dir::LOG => "log",
		abi::dir::PLUGIN => "plugin",
		_ => return FochStr::EMPTY,
	};
	ctx.plugin
		.dirs
		.get(key)
		.map(|path| FochStr::from_str(path))
		.unwrap_or_else(|| {
			if kind == abi::dir::PLUGIN {
				FochStr::from_str(&ctx.plugin_dir)
			} else {
				FochStr::EMPTY
			}
		})
}

unsafe extern "C" fn config_json(host: *const FochHostApi) -> FochStr {
	unsafe { from_host(host) }
		.map(|ctx| FochStr::from_str(&ctx.plugin.config_json))
		.unwrap_or(FochStr::EMPTY)
}

unsafe extern "C" fn plugin_state(host: *const FochHostApi, id: FochStr) -> i32 {
	let Some(ctx) = (unsafe { from_host(host) }) else {
		return 0;
	};
	let Ok(Some(id)) = (unsafe { text(id) }) else {
		return 0;
	};
	ctx.runtime
		.states
		.lock()
		.ok()
		.and_then(|states| states.get(&id).copied())
		.unwrap_or(0)
}

// The draft SDK reserved shared-hook slots. Keep its published layout; the
// first-version design excludes these services and returns E_UNSUPPORTED.
unsafe extern "C" fn unsupported_pattern(
	_: *const FochHostApi,
	_: FochStr,
	address: *mut *mut c_void,
) -> i32 {
	if !address.is_null() {
		unsafe {
			*address = std::ptr::null_mut();
		}
	}
	abi::result::E_UNSUPPORTED
}
unsafe extern "C" fn unsupported_hook(
	_: *const FochHostApi,
	_: *mut c_void,
	_: *mut c_void,
	_: i32,
	next: *mut *const *const c_void,
	hook: *mut *mut abi::FochHook,
) -> i32 {
	if !next.is_null() {
		unsafe {
			*next = std::ptr::null();
		}
	}
	if !hook.is_null() {
		unsafe {
			*hook = std::ptr::null_mut();
		}
	}
	abi::result::E_UNSUPPORTED
}
unsafe extern "C" fn unsupported_hook_toggle(
	_: *const FochHostApi,
	_: *mut abi::FochHook,
	_: i32,
) -> i32 {
	abi::result::E_UNSUPPORTED
}
unsafe extern "C" fn unsupported_patch(
	_: *const FochHostApi,
	_: *mut c_void,
	_: *const u8,
	_: usize,
	patch: *mut *mut abi::FochPatch,
) -> i32 {
	if !patch.is_null() {
		unsafe {
			*patch = std::ptr::null_mut();
		}
	}
	abi::result::E_UNSUPPORTED
}
unsafe extern "C" fn unsupported_patch_toggle(
	_: *const FochHostApi,
	_: *mut abi::FochPatch,
	_: i32,
) -> i32 {
	abi::result::E_UNSUPPORTED
}
