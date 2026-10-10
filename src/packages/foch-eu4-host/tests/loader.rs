//! Real DLL/EXE fixtures; no installed game or source mod is modified.
#![cfg(all(windows, target_arch = "x86_64"))]

use serde_json::{Value, json};
use std::{
	fs,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	sync::OnceLock,
	time::{Duration, Instant},
};
use tempfile::TempDir;

fn fixtures() -> &'static TempDir {
	static BUILD: OnceLock<TempDir> = OnceLock::new();
	BUILD.get_or_init(|| {
		let root = tempfile::tempdir().unwrap();
		let package = Path::new(env!("CARGO_MANIFEST_DIR"));
		let tool = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
			.expect("Windows DLL integration tests require the MSVC build tools");
		for (name, source, mode) in [
			("native", "native.c", 0),
			("bad_abi", "native.c", 1),
			("short_api", "native.c", 2),
			("null_init", "native.c", 3),
			("init_error", "native.c", 4),
			("null_api", "native.c", 5),
			("async_native", "native.c", 6),
			("null_status", "native.c", 7),
			("bad_status", "native.c", 8),
			("data_callback", "native.c", 9),
			("null_detail", "native.c", 10),
			("invalid_utf8", "native.c", 11),
			("unreadable_detail", "native.c", 12),
			("oversized_detail", "native.c", 13),
			("unaligned_report", "native.c", 14),
			("unreadable_report", "native.c", 15),
			("invalid_report_detail", "native.c", 16),
			("short_report", "native.c", 17),
			("sync", "legacy.c", 0),
			("poll", "legacy.c", 1),
			("timeout", "legacy.c", 2),
			("refused", "legacy.c", 3),
		] {
			let mut command = tool.to_command();
			command
				.current_dir(root.path())
				.args(["/nologo", "/LD", "/W4", "/utf-8"])
				.arg(format!("/DFIXTURE_MODE={mode}"))
				.arg(format!("/DFIXTURE_NAME=\"{name}\""))
				.arg(format!(
					"/I{}",
					package.join("../foch-plugin-abi/include").display()
				))
				.arg(package.join("tests/fixtures").join(source))
				.arg(format!("/Fo{name}.obj"))
				.arg(format!("/Fe{name}.dll"));
			let output = command.output().unwrap();
			assert!(
				output.status.success(),
				"{name}: {} {}",
				String::from_utf8_lossy(&output.stdout),
				String::from_utf8_lossy(&output.stderr)
			);
		}
		let output = tool
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/W4", "/utf-8"])
			.arg(package.join("tests/fixtures/game.c"))
			.args(["/FoGame.obj", "/Feeu4.exe", "version.lib"])
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"game: {} {}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		);
		root
	})
}

struct Runtime(TempDir);
impl Runtime {
	fn new() -> Self {
		let temp = tempfile::Builder::new()
			.prefix("foch-加载-")
			.tempdir()
			.unwrap();
		fs::create_dir(temp.path().join("foch-host")).unwrap();
		fs::write(temp.path().join("foch-runtime"), "1\n").unwrap();
		fs::create_dir(temp.path().join("plugins")).unwrap();
		fs::copy(
			fixtures().path().join("eu4.exe"),
			temp.path().join("eu4.exe"),
		)
		.unwrap();
		let dll = std::env::current_exe()
			.unwrap()
			.parent()
			.unwrap()
			.join("foch_eu4_host.dll");
		assert!(dll.is_file(), "host artifact missing: {}", dll.display());
		fs::copy(dll, temp.path().join("VERSION.dll")).unwrap();
		fs::copy(
			fixtures().path().join("native.dll"),
			temp.path().join("plugins/unexpected.dll"),
		)
		.unwrap();
		Self(temp)
	}
	fn path(&self, path: &str) -> PathBuf {
		self.0.path().join(path)
	}
	fn plugin(&self, name: &str, kind: &str, phase: &str) -> Value {
		let path = self.path(&format!("plugins/{name}.dll"));
		fs::copy(fixtures().path().join(format!("{name}.dll")), &path).unwrap();
		json!({"id":name, "version":"0.1.0", "kind":kind, "phase":phase,
			"path":path.to_str().unwrap(), "config_json":"{\"greeting\":\"hi\"}",
			"abi_major":if kind == "native" {Some(1)} else {None}})
	}
	fn plan(&self, plugins: Vec<Value>) {
		fs::write(
			self.path("foch-host/plan.json"),
			serde_json::to_vec_pretty(&json!({
				"format":1, "run_id":"fixture-run", "game_version":"1.37.5.0",
				"events":self.path("foch-host/events.jsonl"), "plugins":plugins
			}))
			.unwrap(),
		)
		.unwrap();
	}
	fn run(&self, mode: &str) -> Vec<Value> {
		run_runtime(self.0.path(), mode)
	}
}

fn run_runtime(directory: &Path, mode: &str) -> Vec<Value> {
	let expected: Vec<String> = fs::read(directory.join("foch-host/plan.json"))
		.ok()
		.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
		.and_then(|plan| plan["plugins"].as_array().cloned())
		.unwrap_or_default()
		.iter()
		.filter_map(|plugin| plugin["id"].as_str().map(str::to_owned))
		.collect();
	let mut child = Command::new(directory.join("eu4.exe"))
		.arg(mode)
		.env(
			"FOCH_FIXTURE_DELAY_MS",
			if mode == "delayed" { "1200" } else { "40" },
		)
		.current_dir(directory)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	let deadline = Instant::now() + Duration::from_secs(10);
	while child.try_wait().unwrap().is_none() {
		let events = read_events(directory);
		let callbacks_complete = expected.iter().all(|id| {
			!matches!(
				id.as_str(),
				"async_native"
					| "unaligned_report"
					| "unreadable_report"
					| "invalid_report_detail"
					| "short_report"
			) || directory
				.join(format!("foch-host/callback-finished-{id}"))
				.exists()
		});
		if callbacks_complete
			&& expected.iter().all(|id| {
				states(&events, id).last().is_some_and(|state| {
					matches!(
						*state,
						"active" | "inactive" | "refused" | "failed" | "unknown"
					)
				})
			}) {
			fs::write(directory.join("foch-host/fixture-complete"), "1").unwrap();
		}
		if Instant::now() > deadline {
			let _ = child.kill();
			panic!("loader deadlocked");
		}
		std::thread::sleep(Duration::from_millis(20));
	}
	let output = child.wait_with_output().unwrap();
	assert!(
		output.status.success(),
		"fixture failed: {:?}; {} {}; diagnostic={:?}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr),
		fs::read_to_string(directory.join("foch-host/host-error.txt"))
	);
	read_events(directory)
}

fn read_events(directory: &Path) -> Vec<Value> {
	fs::read_to_string(directory.join("foch-host/events.jsonl"))
		.unwrap_or_default()
		.split_inclusive('\n')
		.filter(|line| line.ends_with('\n'))
		.map(|line| serde_json::from_str(line).unwrap())
		.collect()
}

fn states<'a>(events: &'a [Value], id: &str) -> Vec<&'a str> {
	events
		.iter()
		.filter(|v| v["plugin_id"] == id && v["event"] == "state")
		.filter_map(|v| v["state"].as_str())
		.collect()
}

#[test]
fn proxy_forwards_without_a_plan_and_ignores_unlisted_dlls() {
	let rt = Runtime::new();
	assert!(rt.run("plain").is_empty());
	fs::remove_file(rt.path("foch-runtime")).unwrap();
	rt.plan(Vec::new());
	assert!(rt.run("forward-only").is_empty());
	assert!(!rt.path("foch-host/events.jsonl").exists());
	assert!(!rt.path("foch-host/host-error.txt").exists());
}

#[test]
fn native_initializes_before_main_and_reports_configuration_and_state() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("native", "native", "entry")]);
	let events = rt.run("native");
	assert_eq!(
		&states(&events, "native")[..3],
		&["loading", "dll_loaded", "initializing"]
	);
	assert!(states(&events, "native").contains(&"active"));
	assert!(
		events
			.iter()
			.any(|v| v["event"] == "log" && v["message"] == "fixture init")
	);
	assert!(events.iter().all(|v| v["run_id"] == "fixture-run"));
}

#[test]
fn legacy_sync_poll_refusal_and_timeout_are_actual_states() {
	let rt = Runtime::new();
	let mut plugins = Vec::new();
	for (name, mode) in [
		("sync", "sync"),
		("poll", "poll"),
		("refused", "sync"),
		("timeout", "poll"),
	] {
		let mut plugin = rt.plugin(
			name,
			"legacy",
			if name == "sync" { "entry" } else { "deferred" },
		);
		plugin["status"] = json!({"export":"ProbeState", "mode":mode, "timeout_ms":if name == "timeout" {150} else {3000},
			"pending":[0], "values":[{"value":1,"state":"active"}, {"value":-3,"state":"refused","reason":"foreign modification"}]});
		plugins.push(plugin);
	}
	rt.plan(plugins);
	let events = rt.run("plain");
	assert_eq!(states(&events, "sync").last(), Some(&"active"));
	assert_eq!(states(&events, "poll").last(), Some(&"active"));
	assert_eq!(states(&events, "refused").last(), Some(&"refused"));
	assert_eq!(states(&events, "timeout").last(), Some(&"unknown"));
	let order: Vec<_> = events
		.iter()
		.filter(|v| v["state"] == "loading")
		.map(|v| v["plugin_id"].as_str().unwrap())
		.collect();
	assert_eq!(order, ["sync", "poll", "refused", "timeout"]);
}

#[test]
fn rejects_bad_native_tables_without_calling_init_and_continues() {
	let rt = Runtime::new();
	rt.plan(
		[
			"bad_abi",
			"short_api",
			"null_init",
			"null_api",
			"init_error",
			"native",
		]
		.into_iter()
		.map(|name| rt.plugin(name, "native", "entry"))
		.collect(),
	);
	let events = rt.run("native");
	for id in ["bad_abi", "short_api", "null_init", "null_api"] {
		assert_eq!(states(&events, id).last(), Some(&"refused"), "{id}");
	}
	assert_eq!(states(&events, "init_error").last(), Some(&"failed"));
	assert_eq!(states(&events, "native").last(), Some(&"active"));
}

#[test]
fn missing_exports_and_digest_mismatch_do_not_become_active() {
	let rt = Runtime::new();
	let mut missing = rt.plugin("sync", "legacy", "entry");
	missing["status"] = json!({"export":"MissingExport","mode":"sync","values":[]});
	let mut mismatch = rt.plugin("native", "native", "entry");
	mismatch["sha256"] = "0".repeat(64).into();
	rt.plan(vec![missing, mismatch]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "sync").last(), Some(&"unknown"));
	assert_eq!(states(&events, "native").last(), Some(&"refused"));
	assert!(!states(&events, "native").contains(&"dll_loaded"));
}

#[test]
fn invalid_plan_leaves_proxy_operational_and_writes_a_diagnostic() {
	let rt = Runtime::new();
	fs::write(rt.path("foch-host/plan.json"), "{broken").unwrap();
	assert!(rt.run("plain").is_empty());
	assert!(rt.path("foch-host/host-error.txt").is_file());
}

#[test]
fn event_files_cannot_redirect_host_writes_outside_the_runtime() {
	let original = "{\"guard\":\"original bytes\"}\n";
	for mode in ["existing", "missing", "hardlink"] {
		let rt = Runtime::new();
		let outside = tempfile::tempdir().unwrap();
		let destination = outside.path().join("guarded-events.jsonl");
		if mode != "missing" {
			fs::write(&destination, original).unwrap();
		}
		rt.plan(vec![rt.plugin("native", "native", "entry")]);
		if mode == "hardlink" {
			fs::hard_link(&destination, rt.path("foch-host/events.jsonl")).unwrap();
		} else {
			let mut plan: Value =
				serde_json::from_slice(&fs::read(rt.path("foch-host/plan.json")).unwrap()).unwrap();
			plan["events"] = destination.to_str().unwrap().into();
			fs::write(
				rt.path("foch-host/plan.json"),
				serde_json::to_vec(&plan).unwrap(),
			)
			.unwrap();
		}
		rt.run("forward-only");
		if mode == "missing" {
			assert!(!destination.exists(), "external event file created");
		} else {
			assert_eq!(
				fs::read_to_string(&destination).unwrap(),
				original,
				"{mode}"
			);
		}
		assert!(rt.path("foch-host/host-error.txt").is_file(), "{mode}");
	}
}

#[test]
fn diagnostic_files_cannot_overwrite_an_external_hardlink() {
	let rt = Runtime::new();
	let outside = tempfile::tempdir().unwrap();
	let destination = outside.path().join("guarded-diagnostic.txt");
	fs::write(&destination, "original bytes").unwrap();
	fs::hard_link(&destination, rt.path("foch-host/host-error.txt")).unwrap();
	fs::write(rt.path("foch-host/plan.json"), "{broken").unwrap();
	assert!(rt.run("forward-only").is_empty());
	assert_eq!(fs::read_to_string(destination).unwrap(), "original bytes");
}

#[test]
fn asynchronous_native_callbacks_keep_the_host_context_alive() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("async_native", "native", "deferred")]);
	let events = rt.run("delayed");
	assert!(states(&events, "async_native").contains(&"initializing"));
	assert_eq!(states(&events, "async_native").last(), Some(&"active"));
}

#[test]
fn deferred_loading_waits_for_pending_entry_initialization() {
	let rt = Runtime::new();
	let mut deferred = rt.plugin("sync", "legacy", "deferred");
	deferred["status"] =
		json!({"export":"ProbeState", "mode":"sync", "values":[{"value":1,"state":"active"}]});
	rt.plan(vec![rt.plugin("async_native", "native", "entry"), deferred]);
	let events = rt.run("delayed");
	let entry_active = events
		.iter()
		.position(|event| event["plugin_id"] == "async_native" && event["state"] == "active")
		.unwrap();
	let deferred_loading = events
		.iter()
		.position(|event| event["plugin_id"] == "sync" && event["state"] == "loading")
		.unwrap();
	assert!(entry_active < deferred_loading);
}

#[test]
fn invalid_status_is_terminal_and_never_repeatedly_polled() {
	let rt = Runtime::new();
	rt.plan(vec![
		rt.plugin("bad_status", "native", "entry"),
		rt.plugin("null_status", "native", "entry"),
	]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "bad_status").last(), Some(&"failed"));
	assert_eq!(states(&events, "null_status").last(), Some(&"refused"));
	assert!(
		!events
			.iter()
			.any(|v| v["message"] == "polled invalid status")
	);
	// The module cannot be unloaded while it may still have threads/hooks.
	assert!(states(&events, "bad_status").contains(&"dll_loaded"));
}

#[test]
fn frozen_digest_accepts_the_selected_bytes_and_external_paths_are_refused() {
	use sha2::{Digest, Sha256};
	let rt = Runtime::new();
	let mut native = rt.plugin("native", "native", "entry");
	native["sha256"] = format!(
		"{:x}",
		Sha256::digest(fs::read(rt.path("plugins/native.dll")).unwrap())
	)
	.into();
	let mut external = rt.plugin("sync", "legacy", "entry");
	external["path"] = fixtures().path().join("sync.dll").to_str().unwrap().into();
	rt.plan(vec![native, external]);
	let events = rt.run("native");
	assert_eq!(states(&events, "native").last(), Some(&"active"));
	assert_eq!(states(&events, "sync").last(), Some(&"refused"));
}

#[test]
fn an_unrelated_executable_uses_only_version_forwarding() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("native", "native", "entry")]);
	fs::rename(rt.path("eu4.exe"), rt.path("other.exe")).unwrap();
	let output = Command::new(rt.path("other.exe"))
		.arg("forward-only")
		.current_dir(rt.0.path())
		.output()
		.unwrap();
	assert!(output.status.success());
	assert!(!rt.path("foch-host/events.jsonl").exists());
}

#[test]
fn forwarding_only_initializes_safely_on_concurrent_first_calls() {
	let rt = Runtime::new();
	fs::rename(rt.path("eu4.exe"), rt.path("other.exe")).unwrap();
	let output = Command::new(rt.path("other.exe"))
		.arg("proxy-threads")
		.current_dir(rt.0.path())
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"{:?}: {}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout)
	);
	assert!(!rt.path("foch-host/events.jsonl").exists());
}

#[test]
fn refuses_a_native_callback_that_points_into_data() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("data_callback", "native", "entry")]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "data_callback").last(), Some(&"refused"));
}

#[test]
fn malformed_status_details_fail_and_stop_initialization_polling() {
	let rt = Runtime::new();
	let names = [
		"null_detail",
		"invalid_utf8",
		"unreadable_detail",
		"oversized_detail",
	];
	rt.plan(
		names
			.iter()
			.map(|id| rt.plugin(id, "native", "entry"))
			.collect(),
	);
	let events = rt.run("plain");
	for id in names {
		assert_eq!(states(&events, id).last(), Some(&"failed"), "{id}");
	}
	assert!(
		!events
			.iter()
			.any(|v| v["message"] == "polled invalid status")
	);
}

#[test]
fn reports_copy_unaligned_status_and_reject_unreadable_or_invalid_data() {
	let rt = Runtime::new();
	rt.plan(
		[
			"unaligned_report",
			"unreadable_report",
			"invalid_report_detail",
		]
		.into_iter()
		.map(|id| rt.plugin(id, "native", "entry"))
		.collect(),
	);
	let events = rt.run("plain");
	assert_eq!(states(&events, "unaligned_report").last(), Some(&"active"));
	for id in ["unreadable_report", "invalid_report_detail"] {
		assert_eq!(states(&events, id).last(), Some(&"failed"), "{id}");
	}
}

#[test]
fn a_short_report_reads_only_the_size_prefix_before_rejecting_it() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("short_report", "native", "entry")]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "short_report").last(), Some(&"failed"));
	assert!(
		events
			.iter()
			.any(|event| event["plugin_id"] == "short_report"
				&& event["reason"] == "reported status structure is too small")
	);
}

#[test]
fn management_stages_a_wire_plan_that_the_real_host_executes() {
	use foch::plugin::{self, deployment, manifest::Manifest, store::ArchiveEntry};
	let source = tempfile::tempdir().unwrap();
	let output = tempfile::tempdir().unwrap();
	let store = output.path().join("store");
	fs::copy(
		fixtures().path().join("eu4.exe"),
		source.path().join("eu4.exe"),
	)
	.unwrap();
	fs::write(source.path().join("VERSION.dll"), "excluded old loader").unwrap();
	let mut selections = Vec::new();
	for (id, name, kind, phase) in [
		("z-native", "native", "native", "entry"),
		("a-legacy", "sync", "legacy", "deferred"),
	] {
		let status = if kind == "legacy" {
			"[status]\nexport = \"ProbeState\"\nmode = \"sync\"\nvalues = [{value=1,state=\"active\"}]\n"
		} else {
			"[config.greeting]\ntype=\"string\"\ndefault=\"hi\"\n"
		};
		let manifest = Manifest::parse(&format!("schema=1\n[plugin]\nid=\"{id}\"\nname=\"{id}\"\nversion=\"1.0.0\"\n[target]\ngame=\"eu4\"\nplatform=\"windows-x86_64\"\ngame_versions=\"=1.37.5\"\nabi_major=1\n[entry]\nkind=\"{kind}\"\nphase=\"{phase}\"\npath=\"plugins/{name}.dll\"\n{status}")).unwrap();
		let package = plugin::store::adapt(
			manifest,
			vec![ArchiveEntry {
				path: format!("plugins/{name}.dll"),
				data: fs::read(fixtures().path().join(format!("{name}.dll"))).unwrap(),
			}],
		)
		.unwrap();
		plugin::install(&store, &package).unwrap();
		selections.push(plugin::Selection {
			id: id.into(),
			version: "1.0.0".parse().unwrap(),
			enabled: true,
			config: json!({}),
		});
	}
	selections.push(plugin::Selection {
		id: "disabled".into(),
		version: "1.0.0".parse().unwrap(),
		enabled: false,
		config: json!({}),
	});
	let game = plugin::GameIdentity {
		game: "eu4".into(),
		version: "1.37.5".parse().unwrap(),
		platform: plugin::WINDOWS_X64.into(),
	};
	let selected = deployment::resolve(&game, &store, &selections).unwrap();
	let layer = foch_runner::runtime::RuntimeLayer::prepare(
		source.path(),
		&output.path().join("rt"),
		"wire-run",
	)
	.unwrap();
	let dll = std::env::current_exe()
		.unwrap()
		.parent()
		.unwrap()
		.join("foch_eu4_host.dll");
	fs::copy(dll, layer.directory.join("VERSION.dll")).unwrap();
	let prepared = selected
		.stage(
			&layer.directory,
			&output.path().join("data"),
			"wire-run",
			source.path(),
		)
		.unwrap();
	let wire = serde_json::to_string(&prepared.plan).unwrap();
	prepared.plan.validate().unwrap();
	let parsed = foch_eu4_host::plan::parse(&wire).unwrap();
	assert_eq!(
		parsed
			.plugins
			.iter()
			.map(|plugin| plugin.id.as_str())
			.collect::<Vec<_>>(),
		["z-native", "a-legacy"]
	);
	let events = run_runtime(&layer.directory, "native");
	assert_eq!(states(&events, "z-native").last(), Some(&"active"));
	assert_eq!(states(&events, "a-legacy").last(), Some(&"active"));
	let actual = deployment::states(&prepared.plan).unwrap();
	assert_eq!(actual["z-native"]["state"], "active");
	assert!(!actual.contains_key("disabled"));
	// Keep management's preflight aligned with the host's actual wire parser.
	for change in 0..8 {
		let mut invalid = serde_json::to_value(&prepared.plan).unwrap();
		match change {
			0 => invalid["plugins"][1]["path"] = invalid["plugins"][0]["path"].clone(),
			1 => invalid["plugins"][1]["status"]["mode"] = "poll".into(),
			2 => invalid["plugins"][1]["status"]["values"][0]["state"] = "healthy".into(),
			3 => invalid["plugins"][0]["abi_major"] = 9.into(),
			4 => invalid["plugins"][0]["config_json"] = "[]".into(),
			5 => invalid["plugins"][0]["dirs"]["plugin"] = "relative".into(),
			6 => invalid["plugins"][0]["path"] = "C:\\rt\\..\\bad.dll".into(),
			_ => invalid["plugins"].as_array_mut().unwrap().reverse(),
		}
		let managed: deployment::HostPlan = serde_json::from_value(invalid.clone()).unwrap();
		assert!(
			managed.validate().is_err(),
			"management accepted case {change}"
		);
		assert!(
			foch_eu4_host::plan::parse(&invalid.to_string()).is_err(),
			"host accepted case {change}"
		);
	}
	// Stored content drift is detected before making another runtime.
	let (versions, _) = plugin::store::installed_versions(&store);
	fs::write(
		versions[0].dir.join(&versions[0].manifest.entry.path),
		"changed",
	)
	.unwrap();
	assert!(deployment::resolve(&game, &store, &selections).is_err());
}
