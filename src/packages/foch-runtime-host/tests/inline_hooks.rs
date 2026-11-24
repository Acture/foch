//! Bounded disposable C/ASM processes through the real DLL host and SDK.
#![cfg(all(windows, target_arch = "x86_64"))]

use serde_json::{Value, json};
use std::{
	fs,
	path::Path,
	process::{Command, Stdio},
	time::{Duration, Instant},
};

#[test]
fn sdk_chains_complete_windows_and_preserves_windows_x64_arguments() {
	let scratch = tempfile::tempdir().unwrap();
	let root = scratch.path();
	let package = Path::new(env!("CARGO_MANIFEST_DIR"));
	let fixture = package.join("tests/fixtures");
	let compiler = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe").unwrap();
	let assembler = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "ml64.exe").unwrap();
	let mut asm = assembler.to_command();
	asm.current_dir(root)
		.args(["/nologo", "/c", "/Foinline_sites.obj"])
		.arg(fixture.join("inline_sites.asm"));
	run_build(&mut asm);
	let mut plugin = compiler.to_command();
	plugin
		.current_dir(root)
		.args(["/nologo", "/LD", "/W4", "/utf-8"])
		.arg(format!(
			"/I{}",
			package.join("../foch-plugin-sdk/include").display()
		))
		.arg(fixture.join("inline_plugin.c"))
		.args(["/Foinline_plugin.obj", "/Feinline_plugin.dll"]);
	run_build(&mut plugin);
	let mut game = compiler.to_command();
	game.current_dir(root)
		.args(["/nologo", "/W4", "/utf-8"])
		.arg(fixture.join("inline_game.c"))
		.args([
			"/Foinline_game.obj",
			"/Feeu4.exe",
			"inline_sites.obj",
			"version.lib",
			"/link",
			"/EXPORT:fixture_inline14",
			"/EXPORT:fixture_inline15",
			"/EXPORT:fixture_inline17",
		]);
	run_build(&mut game);
	let host = std::env::current_exe()
		.unwrap()
		.parent()
		.unwrap()
		.join("foch_runtime_host.dll");
	assert!(
		host.is_file(),
		"normal Cargo library build must supply {}",
		host.display()
	);
	foch_runtime::host::validate_host(&fs::read(&host).unwrap()).unwrap();
	fs::copy(host, root.join("VERSION.dll")).unwrap();
	fs::create_dir(root.join("foch-host")).unwrap();
	fs::create_dir(root.join("plugins")).unwrap();
	fs::rename(
		root.join("inline_plugin.dll"),
		root.join("plugins/inline_plugin.dll"),
	)
	.unwrap();
	fs::write(root.join("foch-runtime"), "1\n").unwrap();
	fs::write(
		root.join("foch-host/plan.json"),
		serde_json::to_vec(&json!({
			"format":1, "run_id":"inline-window", "game_version":"1.37.5.0",
			"events":root.join("foch-host/events.jsonl"),
			"plugins":[{"id":"inline", "version":"0.1.0", "kind":"native", "phase":"entry",
				"abi_major":0, "path":root.join("plugins/inline_plugin.dll")}]
		}))
		.unwrap(),
	)
	.unwrap();
	let mut child = Command::new(root.join("eu4.exe"))
		.current_dir(root)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	let deadline = Instant::now() + Duration::from_secs(15);
	while child.try_wait().unwrap().is_none() {
		if Instant::now() >= deadline {
			let _ = child.kill();
			panic!("disposable inline fixture exceeded 15 seconds");
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	let output = child.wait_with_output().unwrap();
	assert!(
		output.status.success(),
		"{:?}: {} {}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	let events = fs::read_to_string(root.join("foch-host/events.jsonl")).unwrap();
	assert!(
		events
			.lines()
			.map(|line| serde_json::from_str::<Value>(line).unwrap())
			.any(|event| event["plugin_id"] == "inline" && event["state"] == "active"),
		"{events}"
	);
	let proof = fs::read_to_string(root.join("foch-host/inline-chain.txt")).unwrap();
	assert!(
		proof.contains("windows=14,15,17\n")
			&& proof.contains("results=128,28,18\n")
			&& proof.contains("stack-home=preserved\n"),
		"{proof}"
	);
}

fn run_build(command: &mut Command) {
	let output = command.output().unwrap();
	assert!(
		output.status.success(),
		"{} {}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
}
