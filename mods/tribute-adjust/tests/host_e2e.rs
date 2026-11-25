//! End-to-end against the real foch host, mirroring foch-eu4-host's loader
//! tests: a fake eu4.exe (optionally carrying the signature-table pattern
//! bytes), the real host as VERSION.dll, and this package's plugin DLL.
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

// The three verified 1.37.5 patterns from signatures/signatures-1.37.5.toml,
// padded with a plain `ret` so each could even be called; here nothing calls
// them, they only need to live in an executable section.
const TRIBUTE_EXECUTOR: &[u8] = &[
	0x48, 0x89, 0x5C, 0x24, 0x18, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57,
	0x48, 0x8D, 0x6C, 0x24, 0xD9, 0x48, 0x81, 0xEC, 0x00, 0x01, 0x00, 0x00, 0x48, 0x8B, 0xF2, 0x4C,
	0x8B, 0xF1, 0x48, 0x8B, 0x05, 0xA7, 0x10, 0x82, 0x01, 0x48, 0x8B, 0x90, 0x18, 0x01, 0x00, 0x00,
	0xC3,
];
const ADD_INCOME: &[u8] = &[
	0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x20, 0x55, 0x57, 0x41, 0x54, 0x41, 0x56,
	0x41, 0x57, 0x48, 0x8B, 0xEC, 0x48, 0x81, 0xEC, 0x80, 0x00, 0x00, 0x00, 0xC3,
];
const CHANGE_TREASURY: &[u8] = &[
	0x48, 0x89, 0x5C, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20, 0x57, 0x48, 0x83, 0xEC, 0x30, 0x48,
	0x8B, 0xF2, 0x48, 0x8B, 0xF9, 0xC3,
];

fn workspace() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.ancestors()
		.nth(2)
		.unwrap()
		.to_path_buf()
}

fn payload_c_source() -> String {
	let mut payload = String::new();
	for bytes in [TRIBUTE_EXECUTOR, ADD_INCOME, CHANGE_TREASURY] {
		for byte in bytes {
			payload.push_str(&format!("0x{byte:02X},"));
		}
	}
	format!(
		"#pragma section(\".text$pay\", execute)
__declspec(allocate(\".text$pay\")) const unsigned char pay_code[] = {{{payload}}};\n
"
	)
}

struct Artifacts {
	root: TempDir,
	host: PathBuf,
	plugin: PathBuf,
}

fn artifacts() -> &'static Artifacts {
	static BUILD: OnceLock<Artifacts> = OnceLock::new();
	BUILD.get_or_init(|| {
		let status = Command::new(env!("CARGO"))
			.current_dir(workspace())
			.args(["build", "-p", "foch-runtime-host", "-p", "foch-eu4-tribute"])
			.status()
			.expect("cargo build");
		assert!(status.success());
		let target = workspace().join("target/debug");
		let host = target.join("foch_runtime_host.dll");
		let plugin = target.join("foch_eu4_tribute.dll");
		assert!(host.is_file(), "missing {}", host.display());
		assert!(plugin.is_file(), "missing {}", plugin.display());
		let root = tempfile::tempdir().unwrap();
		let tool = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
			.expect("MSVC build tools required");
		// The host's own game fixture drives the proxy DLL through every
		// initialization path; the payload object adds the pattern bytes.
		let game_c = root.path().join("game.c");
		fs::copy(
			workspace().join("src/packages/foch-runtime-host/tests/fixtures/game.c"),
			&game_c,
		)
		.unwrap();
		let payload_c = root.path().join("payload.c");
		fs::write(&payload_c, payload_c_source()).unwrap();
		// The host's own game fixture drives the proxy DLL through every
		// initialization path; the payload object adds the pattern bytes.
		let game_c = root.path().join("game.c");
		fs::copy(
			workspace().join("src/packages/foch-runtime-host/tests/fixtures/game.c"),
			&game_c,
		)
		.unwrap();
		let payload_c = root.path().join("payload.c");
		fs::write(&payload_c, payload_c_source()).unwrap();
		let output = tool
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/W4", "/utf-8", "/c"])
			.arg(&game_c)
			.args(["/Fogame.obj"])
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"game.obj: {} {}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		);
		// game.c now references the assembly-defined fixture sites, so the
		// payload object and the assembly are both linked into the fixtures.
		let payload = tool
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/W4", "/utf-8", "/c"])
			.arg(&payload_c)
			.args(["/Fopayload.obj"])
			.output()
			.unwrap();
		assert!(
			payload.status.success(),
			"payload.obj: {} {}",
			String::from_utf8_lossy(&payload.stdout),
			String::from_utf8_lossy(&payload.stderr)
		);
		let ml = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "ml64.exe")
			.expect("Windows DLL integration tests require the MSVC assembler");
		let assembled = ml
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/c", "/Fosites.obj"])
			.arg(workspace().join("src/packages/foch-runtime-host/tests/fixtures/sites.asm"))
			.output()
			.unwrap();
		assert!(
			assembled.status.success(),
			"sites: {} {}",
			String::from_utf8_lossy(&assembled.stdout),
			String::from_utf8_lossy(&assembled.stderr)
		);
		for (name, objects) in [
			("eu4_plain", "game.obj sites.obj"),
			("eu4_patterns", "game.obj payload.obj sites.obj"),
		] {
			let output = tool
				.to_command()
				.current_dir(root.path())
				.args(["/nologo", "/W4", "/utf-8"])
				.args(
					objects
						.split_whitespace()
						.map(|o| root.path().join(o))
						.map(|p| p.to_str().unwrap().to_string()),
				)
				.args([format!("/Fe{name}.exe"), "version.lib".to_string()])
				.output()
				.unwrap();
			assert!(
				output.status.success(),
				"{name}: {} {}",
				String::from_utf8_lossy(&output.stdout),
				String::from_utf8_lossy(&output.stderr)
			);
		}
		Artifacts { root, host, plugin }
	})
}

fn runtime(with_patterns: bool) -> TempDir {
	let built = artifacts();
	let temp = tempfile::Builder::new()
		.prefix("foch-tribute-")
		.tempdir()
		.unwrap();
	fs::create_dir(temp.path().join("foch-host")).unwrap();
	fs::write(temp.path().join("foch-runtime"), "1\n").unwrap();
	fs::create_dir(temp.path().join("plugins")).unwrap();
	let exe = if with_patterns {
		"eu4_patterns.exe"
	} else {
		"eu4_plain.exe"
	};
	fs::copy(built.root.path().join(exe), temp.path().join("eu4.exe")).unwrap();
	fs::copy(&built.host, temp.path().join("VERSION.dll")).unwrap();
	fs::copy(
		&built.plugin,
		temp.path().join("plugins/foch_eu4_tribute.dll"),
	)
	.unwrap();
	let plugin_path = temp.path().join("plugins/foch_eu4_tribute.dll");
	fs::write(
		temp.path().join("foch-host/plan.json"),
		serde_json::to_vec_pretty(&json!({
			"format": 1,
			"run_id": "tribute-e2e",
			"game_version": "1.37.5.0",
			"events": temp.path().join("foch-host/events.jsonl"),
			"plugins": [{
				"id": "dev.foch.eu4-tribute-adjust",
				"version": "0.1.0",
				"kind": "native",
				"phase": "entry",
				"path": plugin_path,
				"config_json": "{\"enabled\":true,\"global_percent\":50,\"tag_overrides\":\"{}\"}",
				"abi_major": 0
			}]
		}))
		.unwrap(),
	)
	.unwrap();
	temp
}

fn run(directory: &std::path::Path) -> Vec<Value> {
	let child = Command::new(directory.join("eu4.exe"))
		.arg("plain")
		.current_dir(directory)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	let deadline = Instant::now() + Duration::from_secs(30);
	let events = loop {
		let events = read_events(directory);
		let terminal = final_state_opt(&events).is_some_and(|state| {
			matches!(
				state,
				"active" | "inactive" | "refused" | "failed" | "unknown"
			)
		});
		if terminal {
			// game.c waits for this marker before exiting cleanly.
			fs::write(
				directory.join("foch-host/fixture-complete"),
				"1
",
			)
			.unwrap();
			break events;
		}
		assert!(Instant::now() < deadline, "timed out; events={events:?}");
		std::thread::sleep(Duration::from_millis(20));
	};
	let output = child.wait_with_output().unwrap();
	assert!(
		output.status.success(),
		"fixture failed: {:?}; {} {}; events={:?}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr),
		events
	);
	events
}

fn final_state_opt(events: &[Value]) -> Option<&str> {
	events
		.iter()
		.filter(|v| v["plugin_id"] == "dev.foch.eu4-tribute-adjust" && v["event"] == "state")
		.filter_map(|v| v["state"].as_str())
		.next_back()
}

fn read_events(directory: &std::path::Path) -> Vec<Value> {
	fs::read_to_string(directory.join("foch-host/events.jsonl"))
		.unwrap_or_default()
		.split_inclusive('\n')
		.filter(|line| line.ends_with('\n'))
		.filter_map(|line| serde_json::from_str(line).ok())
		.collect()
}

fn final_state(events: &[Value]) -> &str {
	events
		.iter()
		.filter(|v| v["plugin_id"] == "dev.foch.eu4-tribute-adjust" && v["event"] == "state")
		.filter_map(|v| v["state"].as_str())
		.next_back()
		.expect("no state event")
}

#[test]
fn the_plugin_hooks_when_the_patterns_are_present() {
	let dir = runtime(true);
	let events = run(dir.path());
	assert_eq!(
		final_state(&events),
		"active",
		"expected active against the pattern-carrying exe; events={events:?}"
	);
}

#[test]
fn the_plugin_refuses_when_the_patterns_are_absent() {
	let dir = runtime(false);
	let events = run(dir.path());
	assert_eq!(
		final_state(&events),
		"refused",
		"expected refused against the plain exe; events={events:?}"
	);
}
