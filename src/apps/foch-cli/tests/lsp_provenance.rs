use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tower_lsp::lsp_types::Url;
use walkdir::WalkDir;

const MERGED_SCRIPT: &str = "common/scripted_effects/zzz_foch_scripted_effects.txt";
const SIDECAR: &str = ".foch/foch-provenance.json";

#[test]
fn merged_definition_hover_uses_recorded_sources_only_for_unchanged_content() {
	let scratch = TempDir::new().expect("LSP test scratch");
	let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../packages/foch/tests/fixtures/playsets/eu4_union_scripted_effect")
		.canonicalize()
		.expect("merge fixture");
	let original_sources = fixture_bytes(&fixture);
	let game = scratch.path().join("game");
	fs::create_dir_all(&game).expect("isolated game root");
	let config = foch::input::Config {
		steam_root_path: Some(scratch.path().join("missing-steam")),
		paradox_data_path: Some(scratch.path().join("paradox")),
		game_path: [("eu4".into(), game)].into_iter().collect(),
		extra_ignore_patterns: Vec::new(),
	};
	fs::create_dir_all(scratch.path().join("config")).expect("isolated config root");
	config
		.save_config(&scratch.path().join("config/config.toml"))
		.expect("fixture config");
	let output = scratch.path().join("merged");
	let merge = isolated_foch(scratch.path())
		.arg("merge")
		.arg(fixture.join("dlc_load.json"))
		.arg("--out")
		.arg(&output)
		.args([
			"--no-game-base",
			"--provenance",
			"--confirm",
			"--non-interactive",
		])
		.output()
		.expect("run production merge");
	assert!(
		merge.status.success(),
		"merge failed: {}\n{}",
		String::from_utf8_lossy(&merge.stdout),
		String::from_utf8_lossy(&merge.stderr)
	);
	let script_path = output.join(MERGED_SCRIPT);
	let original_script = fs::read(&script_path).expect("generated script");
	let text = String::from_utf8(original_script.clone()).expect("UTF-8 fixture output");
	let original_sidecar = fs::read(output.join(SIDECAR)).expect("generated provenance sidecar");
	let sidecar: Value = serde_json::from_slice(&original_sidecar).expect("provenance JSON");
	assert_eq!(sidecar["version"], 1);
	assert_eq!(
		sidecar["files"][MERGED_SCRIPT]["content_hash"],
		blake3::hash(&original_script).to_hex().to_string()
	);
	assert_eq!(
		sidecar["files"][MERGED_SCRIPT]["definitions"]["test_shared_effect"],
		json!(["330001", "330002"])
	);

	let schema = scratch.path().join("schema");
	fs::create_dir_all(&schema).expect("isolated schema root");
	fs::write(
		schema.join("scripted_effects.cwt"),
		include_str!("../../../packages/foch-lsp/tests/fixtures/lsp/schema/scripted_effects.cwt"),
	)
	.expect("write fixture schema");
	let uri = Url::from_file_path(&script_path)
		.expect("script URI")
		.to_string();
	let mut server = StdioLsp::start(scratch.path(), &output, &schema);
	let initialized = server.request(
		1,
		"initialize",
		json!({"processId": null, "rootUri": null, "capabilities": {}}),
	);
	assert_eq!(initialized["capabilities"]["hoverProvider"], true);
	server.notify("initialized", json!({}));
	server.receive_until(|message| {
		message["method"] == "window/logMessage"
			&& message["params"]["message"]
				.as_str()
				.is_some_and(|text| text.contains("workspace snapshot loaded"))
	});
	server.notify(
		"textDocument/didOpen",
		json!({"textDocument": {"uri": uri, "languageId": "paradox", "version": 1, "text": text}}),
	);
	server.wait_for_diagnostics(&uri);
	let definition = position_for(&text, "test_shared_effect = {");
	let field = position_for(&text, "add_prestige");
	let hover = server.hover(2, &uri, &definition);
	let markdown = hover["contents"]["value"].as_str().expect("source hover");
	assert!(markdown.contains("Merged from"), "{markdown}");
	let first = markdown
		.find("effect\\_a (330001)")
		.expect("first contributor");
	let second = markdown
		.find("effect\\_b (330002)")
		.expect("second contributor");
	assert!(
		first < second,
		"source precedence must be preserved: {markdown}"
	);
	assert_eq!(hover["range"]["start"], definition);

	let schema_hover = server.hover(3, &uri, &field);
	let schema_markdown = schema_hover["contents"]["value"]
		.as_str()
		.expect("schema hover");
	assert!(schema_markdown.contains("**add_prestige**"));
	assert!(schema_markdown.contains("Adds prestige directly from a scripted effect body."));
	assert!(!schema_markdown.contains("Merged from"));
	assert_eq!(fs::read(&script_path).unwrap(), original_script);
	assert_eq!(fs::read(output.join(SIDECAR)).unwrap(), original_sidecar);

	// A dirty buffer has the same definition key but no longer matches the merge.
	let changed = text.replace("add_prestige = 5", "add_prestige = 6");
	assert_ne!(changed, text, "fixture must exercise a real edit");
	server.change(&uri, 2, &changed);
	assert_no_provenance(&server.hover(4, &uri, &definition));
	assert_eq!(server.hover(5, &uri, &field), schema_hover);
	server.change(&uri, 3, &text);
	assert_eq!(server.hover(6, &uri, &definition), hover);

	// Matching the buffer to a changed disk file must still fail the sidecar hash.
	fs::write(&script_path, &changed).expect("edit generated output only");
	server.change(&uri, 4, &changed);
	assert_no_provenance(&server.hover(7, &uri, &definition));
	assert_eq!(server.hover(8, &uri, &field), schema_hover);
	assert_eq!(server.request(9, "shutdown", Value::Null), Value::Null);
	server.notify("exit", Value::Null);
	drop(server);
	assert_eq!(fs::read(&script_path).unwrap(), changed.as_bytes());
	assert_eq!(fs::read(output.join(SIDECAR)).unwrap(), original_sidecar);
	assert_eq!(
		fixture_bytes(&fixture),
		original_sources,
		"source mods are read-only"
	);
}

fn assert_no_provenance(hover: &Value) {
	assert!(!hover.to_string().contains("Merged from"), "{hover}");
}

fn fixture_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
	WalkDir::new(root)
		.into_iter()
		.map(|entry| entry.expect("fixture entry"))
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| (entry.path().to_path_buf(), fs::read(entry.path()).unwrap()))
		.collect()
}

fn isolated_foch(scratch: &Path) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
	command
		.current_dir(scratch)
		.env("FOCH_CONFIG_DIR", scratch.join("config"))
		.env("FOCH_DATA_DIR", scratch.join("data"))
		.env("FOCH_CACHE_ROOT", scratch.join("cache"))
		.env("HOME", scratch.join("home"))
		.env("USERPROFILE", scratch.join("home"))
		.env("XDG_DATA_HOME", scratch.join("xdg-data"))
		.env("APPDATA", scratch.join("appdata"))
		.env("LOCALAPPDATA", scratch.join("local-appdata"))
		.env_remove("FOCH_CWTOOLS_SCHEMA_DIR")
		.env_remove("FOCH_LSP_TARGETS_JSON");
	command
}

fn position_for(text: &str, token: &str) -> Value {
	let offset = text.find(token).expect("hover token in generated script");
	let before = &text[..offset];
	json!({
		"line": before.bytes().filter(|byte| *byte == b'\n').count(),
		"character": before.rsplit('\n').next().unwrap().encode_utf16().count(),
	})
}

struct StdioLsp {
	child: Child,
	stdin: ChildStdin,
	messages: Receiver<Result<Value, String>>,
	reader: Option<JoinHandle<()>>,
}

impl StdioLsp {
	fn start(scratch: &Path, output: &Path, schema: &Path) -> Self {
		let mut child = isolated_foch(scratch)
			.arg("lsp")
			.env("FOCH_CWTOOLS_SCHEMA_DIR", schema)
			.env(
				"FOCH_LSP_TARGETS_JSON",
				json!([{"path": output, "role": "mod"}]).to_string(),
			)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit())
			.spawn()
			.expect("spawn foch lsp");
		let stdin = child.stdin.take().expect("LSP stdin");
		let stdout = child.stdout.take().expect("LSP stdout");
		let (sender, messages) = mpsc::channel();
		let reader = thread::spawn(move || {
			let mut stdout = BufReader::new(stdout);
			loop {
				let message = read_message(&mut stdout).map_err(|err| err.to_string());
				let failed = message.is_err();
				if sender.send(message).is_err() || failed {
					break;
				}
			}
		});
		Self {
			child,
			stdin,
			messages,
			reader: Some(reader),
		}
	}

	fn send(&mut self, mut message: Value) {
		if message["params"].is_null() {
			message.as_object_mut().unwrap().remove("params");
		}
		let body = serde_json::to_vec(&message).expect("encode LSP message");
		write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("LSP header");
		self.stdin.write_all(&body).expect("LSP body");
		self.stdin.flush().expect("flush LSP request");
	}

	fn notify(&mut self, method: &str, params: Value) {
		self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
	}

	fn request(&mut self, id: u32, method: &str, params: Value) -> Value {
		self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
		let response = self.receive_until(|message| message["id"] == id);
		assert!(response.get("error").is_none(), "{method}: {response}");
		response.get("result").expect("LSP result").clone()
	}

	fn receive_until(&self, matches: impl Fn(&Value) -> bool) -> Value {
		let deadline = Instant::now() + Duration::from_secs(30);
		loop {
			let message = self
				.messages
				.recv_timeout(deadline.saturating_duration_since(Instant::now()))
				.expect("LSP response within 30 seconds")
				.expect("valid LSP frame");
			if matches(&message) {
				return message;
			}
		}
	}

	fn wait_for_diagnostics(&self, uri: &str) {
		self.receive_until(|message| {
			message["method"] == "textDocument/publishDiagnostics"
				&& message["params"]["uri"] == uri
		});
	}

	fn change(&mut self, uri: &str, version: u32, text: &str) {
		self.notify(
			"textDocument/didChange",
			json!({
				"textDocument": {"uri": uri, "version": version},
				"contentChanges": [{"text": text}],
			}),
		);
		self.wait_for_diagnostics(uri);
	}

	fn hover(&mut self, id: u32, uri: &str, position: &Value) -> Value {
		self.request(
			id,
			"textDocument/hover",
			json!({"textDocument": {"uri": uri}, "position": position}),
		)
	}
}

impl Drop for StdioLsp {
	fn drop(&mut self) {
		// Also runs when an assertion or timeout panics, so no server is left alive.
		let _ = self.child.kill();
		let _ = self.child.wait();
		if let Some(reader) = self.reader.take() {
			let _ = reader.join();
		}
	}
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Value> {
	let mut length = None;
	loop {
		let mut header = String::new();
		if reader.read_line(&mut header)? == 0 {
			return Err(std::io::Error::new(
				std::io::ErrorKind::UnexpectedEof,
				"LSP stdout closed",
			));
		}
		if header == "\r\n" {
			break;
		}
		if let Some(value) = header.strip_prefix("Content-Length:") {
			length = Some(
				value
					.trim()
					.parse::<usize>()
					.map_err(std::io::Error::other)?,
			);
		}
	}
	let length = length
		.filter(|length| *length <= 4 * 1024 * 1024)
		.ok_or_else(|| std::io::Error::other("missing or oversized LSP Content-Length"))?;
	let mut body = vec![0; length];
	reader.read_exact(&mut body)?;
	serde_json::from_slice(&body).map_err(std::io::Error::other)
}
