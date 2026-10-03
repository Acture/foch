use super::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

const RELATIVE: &str = "common/scripted_effects/test.txt";
const TEXT: &str = "# 中文😀\r\nshared = { add_prestige = 1 }\r\n";

struct Fixture {
	_root: TempDir,
	path: PathBuf,
	uri: Url,
	schema: EditorSchema,
}

impl Fixture {
	fn new() -> Self {
		let root = TempDir::new().unwrap();
		let path = root.path().join(RELATIVE);
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(&path, TEXT).unwrap();
		fs::create_dir(root.path().join(".foch")).unwrap();
		fs::write(
			root.path()
				.join(foch::model::MERGE_PROVENANCE_ARTIFACT_PATH),
			json!({RELATIVE: {"shared": ["940", "320"]}}).to_string(),
		)
		.unwrap();
		let schema_dir = root.path().join("schema");
		fs::create_dir(&schema_dir).unwrap();
		fs::write(
			schema_dir.join("effects.cwt"),
			r#"
			types = { type[effect] = { path = "game/common/scripted_effects" } }
			effect = { shared = { add_prestige = scalar } }
		"#,
		)
		.unwrap();
		let schema = EditorSchema::load_from_directory_with_cache(&schema_dir, None).unwrap();
		let uri = Url::from_file_path(&path).unwrap();
		Self {
			_root: root,
			path,
			uri,
			schema,
		}
	}

	async fn open(&self, backend: &Backend) {
		*backend.schema.write().await = Some(self.schema.clone());
		backend.state.write().await.targets = vec![ScanTarget {
			path: self._root.path().to_path_buf(),
			role: TargetRole::Mod,
		}];
		backend
			.did_open(
				serde_json::from_value(json!({
					"textDocument": {"uri": self.uri, "languageId": "paradox", "version": 1, "text": TEXT}
				}))
				.unwrap(),
			)
			.await;
	}

	async fn hover(&self, backend: &Backend, line: u32) -> Option<Hover> {
		backend
			.hover(
				serde_json::from_value(json!({
					"textDocument": {"uri": self.uri}, "position": {"line": line, "character": 0}
				}))
				.unwrap(),
			)
			.await
			.unwrap()
	}
}

fn markdown(hover: Hover) -> String {
	let HoverContents::Markup(content) = hover.contents else {
		panic!("Markdown hover")
	};
	content.value
}

#[tokio::test]
async fn provenance_hover_handler_composes_schema_and_cleans_up_on_close() {
	let fixture = Fixture::new();
	let (service, _socket) = LspService::new(Backend::new);
	let backend = service.inner();
	fixture.open(backend).await;
	let result = fixture.hover(backend, 1).await.unwrap();
	assert_eq!(result.range.unwrap().end, Position::new(1, 6));
	let rendered = markdown(result);
	assert!(rendered.contains("Type: `Block`"));
	assert!(rendered.contains("Mod IDs: 940, 320"));
	backend
		.did_close(serde_json::from_value(json!({"textDocument": {"uri": fixture.uri}})).unwrap())
		.await;
	assert!(fixture.hover(backend, 1).await.is_none());
	assert_eq!(fs::read_to_string(&fixture.path).unwrap(), TEXT);
}

#[tokio::test]
async fn provenance_hover_handler_applies_utf16_incremental_edits_and_keeps_schema() {
	let fixture = Fixture::new();
	let (service, _socket) = LspService::new(Backend::new);
	let backend = service.inner();
	fixture.open(backend).await;
	backend.did_change(serde_json::from_value(json!({
		"textDocument": {"uri": fixture.uri, "version": 2},
		"contentChanges": [
			{"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 6}}, "text": "🌍"},
			{"range": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 6}}, "text": "\r\n"}
		]
	})).unwrap()).await;
	let result = markdown(
		fixture
			.hover(backend, 2)
			.await
			.expect("schema survives dirty edits"),
	);
	assert!(result.contains("Type: `Block`"));
	assert!(!result.contains("Recorded merge sources"));
	// An old version must not resurrect the pre-edit source attribution.
	backend
		.did_change(
			serde_json::from_value(json!({
				"textDocument": {"uri": fixture.uri, "version": 1}, "contentChanges": [{"text": TEXT}]
			}))
			.unwrap(),
		)
		.await;
	assert!(fixture.hover(backend, 2).await.is_some());
	// A full replacement can restore the exact saved document.
	backend
		.did_change(
			serde_json::from_value(json!({
				"textDocument": {"uri": fixture.uri, "version": 3}, "contentChanges": [{"text": TEXT}]
			}))
			.unwrap(),
		)
		.await;
	assert!(markdown(fixture.hover(backend, 1).await.unwrap()).contains("Mod IDs: 940, 320"));
}

#[tokio::test]
async fn provenance_hover_handler_discards_invalid_edits_until_resynchronized() {
	let fixture = Fixture::new();
	let (service, _socket) = LspService::new(Backend::new);
	let backend = service.inner();
	fixture.open(backend).await;
	// Character 5 lies inside the emoji's UTF-16 surrogate pair.
	backend.did_change(serde_json::from_value(json!({
		"textDocument": {"uri": fixture.uri, "version": 2},
		"contentChanges": [{"range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}, "text": "x"}]
	})).unwrap()).await;
	assert!(fixture.hover(backend, 1).await.is_none());
	backend
		.did_change(
			serde_json::from_value(json!({
				"textDocument": {"uri": fixture.uri, "version": 3}, "contentChanges": [{"text": TEXT}]
			}))
			.unwrap(),
		)
		.await;
	assert!(markdown(fixture.hover(backend, 1).await.unwrap()).contains("Mod IDs: 940, 320"));
}

#[tokio::test]
async fn provenance_hover_handler_requires_a_matching_current_merge_record_after_save() {
	let fixture = Fixture::new();
	let sidecar = fixture
		._root
		.path()
		.join(foch::model::MERGE_PROVENANCE_ARTIFACT_PATH);
	let record = |text: &str| {
		json!({
			"version": 1,
			"files": {RELATIVE: {"content_hash": blake3::hash(text.as_bytes()).to_hex().as_str(), "definitions": {"shared": ["940"]}}},
			"mod_names": {"940": "Example Mod"}
		})
	};
	fs::write(&sidecar, record(TEXT).to_string()).unwrap();
	let (service, _socket) = LspService::new(Backend::new);
	let backend = service.inner();
	fixture.open(backend).await;
	assert!(
		markdown(fixture.hover(backend, 1).await.unwrap())
			.contains("Merged from Example Mod (940)")
	);
	let changed = TEXT.replace("= 1", "= 2");
	backend
		.did_change(
			serde_json::from_value(json!({
				"textDocument": {"uri": fixture.uri, "version": 2}, "contentChanges": [{"text": changed}]
			}))
			.unwrap(),
		)
		.await;
	fs::write(&fixture.path, &changed).unwrap();
	backend
		.did_save(serde_json::from_value(json!({"textDocument": {"uri": fixture.uri}})).unwrap())
		.await;
	let result = markdown(fixture.hover(backend, 1).await.unwrap());
	assert!(result.contains("Type: `Block`"));
	assert!(!result.contains("Merged from"));
	fs::write(&sidecar, record(&changed).to_string()).unwrap();
	assert!(
		markdown(fixture.hover(backend, 1).await.unwrap())
			.contains("Merged from Example Mod (940)")
	);
	fs::write(&sidecar, "broken").unwrap();
	assert!(!markdown(fixture.hover(backend, 1).await.unwrap()).contains("Merged from"));
}
