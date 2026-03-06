use foch::check::eu4_builtin::{
	alias_keywords, builtin_effect_names, builtin_trigger_names, contextual_keywords,
	reserved_keywords,
};
use foch::check::model::SymbolKind;
use foch::check::semantic_index::{build_semantic_index, parse_script_file};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
	CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
	DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
	InitializeParams, InitializeResult, InitializedParams, MessageType, Position,
	ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CandidateSource {
	Keyword,
	Builtin,
	Workspace,
}

#[derive(Clone, Debug)]
struct CompletionCandidate {
	label: String,
	insert_text: String,
	kind: CompletionItemKind,
	detail: String,
	source: CandidateSource,
}

#[derive(Default)]
struct ServerState {
	docs: HashMap<Url, String>,
	roots: Vec<PathBuf>,
	static_candidates: Vec<CompletionCandidate>,
	workspace_candidates: Vec<CompletionCandidate>,
}

struct Backend {
	client: Client,
	state: Arc<RwLock<ServerState>>,
}

impl Backend {
	fn new(client: Client) -> Self {
		let state = ServerState {
			static_candidates: build_static_candidates(),
			..ServerState::default()
		};
		Self {
			client,
			state: Arc::new(RwLock::new(state)),
		}
	}

	async fn refresh_workspace_candidates(&self) {
		let roots = { self.state.read().await.roots.clone() };
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || build_workspace_candidates(&roots)).await;
		match built {
			Ok(candidates) => {
				let count = candidates.len();
				let mut state = self.state.write().await;
				state.workspace_candidates = candidates;
				client
					.log_message(
						MessageType::INFO,
						format!("foch-lsp workspace symbol candidates loaded: {count}"),
					)
					.await;
			}
			Err(err) => {
				self.client
					.log_message(
						MessageType::ERROR,
						format!("foch-lsp failed to build workspace candidates: {err}"),
					)
					.await;
			}
		}
	}
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
	async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
		let mut roots = Vec::new();
		if let Some(folders) = params.workspace_folders {
			for folder in folders {
				if let Ok(path) = folder.uri.to_file_path() {
					roots.push(path);
				}
			}
		}
		if roots.is_empty()
			&& let Some(root_uri) = params.root_uri
			&& let Ok(path) = root_uri.to_file_path()
		{
			roots.push(path);
		}

		{
			let mut state = self.state.write().await;
			state.roots = roots;
		}

		Ok(InitializeResult {
			server_info: None,
			capabilities: ServerCapabilities {
				text_document_sync: Some(TextDocumentSyncCapability::Kind(
					TextDocumentSyncKind::INCREMENTAL,
				)),
				completion_provider: Some(CompletionOptions {
					resolve_provider: Some(false),
					trigger_characters: Some(vec![
						".".to_string(),
						"_".to_string(),
						":".to_string(),
					]),
					all_commit_characters: None,
					work_done_progress_options: Default::default(),
					completion_item: None,
				}),
				..ServerCapabilities::default()
			},
		})
	}

	async fn initialized(&self, _: InitializedParams) {
		self.client
			.log_message(MessageType::INFO, "foch-lsp initialized")
			.await;
		self.refresh_workspace_candidates().await;
	}

	async fn did_open(&self, params: DidOpenTextDocumentParams) {
		let mut state = self.state.write().await;
		state
			.docs
			.insert(params.text_document.uri, params.text_document.text);
	}

	async fn did_change(&self, params: DidChangeTextDocumentParams) {
		let mut state = self.state.write().await;
		if let Some(last) = params.content_changes.last() {
			state
				.docs
				.insert(params.text_document.uri.clone(), last.text.clone());
		}
	}

	async fn did_save(&self, params: DidSaveTextDocumentParams) {
		if let Some(text) = params.text {
			let mut state = self.state.write().await;
			state.docs.insert(params.text_document.uri, text);
		}
	}

	async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
		let state = self.state.read().await;
		let uri = &params.text_document_position.text_document.uri;
		let position = params.text_document_position.position;
		let prefix = state
			.docs
			.get(uri)
			.map(|text| extract_completion_prefix(text, position))
			.unwrap_or_default();
		let prefix_lower = prefix.to_ascii_lowercase();

		let mut candidates: Vec<CompletionCandidate> = if prefix_lower.len() < 2 {
			state
				.static_candidates
				.iter()
				.filter(|item| item.source == CandidateSource::Keyword)
				.cloned()
				.collect()
		} else {
			state
				.static_candidates
				.iter()
				.chain(state.workspace_candidates.iter())
				.filter(|item| item.label.to_ascii_lowercase().starts_with(&prefix_lower))
				.cloned()
				.collect()
		};

		candidates.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.label.cmp(&b.label)));
		candidates.truncate(200);

		let items: Vec<CompletionItem> = candidates
			.into_iter()
			.map(|item| CompletionItem {
				label: item.label,
				kind: Some(item.kind),
				detail: Some(item.detail),
				insert_text: Some(item.insert_text),
				..CompletionItem::default()
			})
			.collect();

		Ok(Some(CompletionResponse::Array(items)))
	}

	async fn shutdown(&self) -> Result<()> {
		Ok(())
	}
}

#[tokio::main]
async fn main() {
	let stdin = tokio::io::stdin();
	let stdout = tokio::io::stdout();
	let (service, socket) = LspService::new(Backend::new);
	Server::new(stdin, stdout, socket).serve(service).await;
}

fn build_static_candidates() -> Vec<CompletionCandidate> {
	let mut out = Vec::new();

	for key in reserved_keywords() {
		out.push(CompletionCandidate {
			label: key.clone(),
			insert_text: key.clone(),
			kind: CompletionItemKind::KEYWORD,
			detail: "reserved keyword".to_string(),
			source: CandidateSource::Keyword,
		});
	}
	for key in contextual_keywords() {
		out.push(CompletionCandidate {
			label: key.clone(),
			insert_text: key.clone(),
			kind: CompletionItemKind::KEYWORD,
			detail: "contextual keyword".to_string(),
			source: CandidateSource::Keyword,
		});
	}
	for key in alias_keywords() {
		out.push(CompletionCandidate {
			label: key.clone(),
			insert_text: key.clone(),
			kind: CompletionItemKind::VARIABLE,
			detail: "scope alias".to_string(),
			source: CandidateSource::Keyword,
		});
	}
	for key in builtin_trigger_names() {
		out.push(CompletionCandidate {
			label: key.clone(),
			insert_text: key.clone(),
			kind: CompletionItemKind::FUNCTION,
			detail: "builtin trigger".to_string(),
			source: CandidateSource::Builtin,
		});
	}
	for key in builtin_effect_names() {
		out.push(CompletionCandidate {
			label: key.clone(),
			insert_text: key.clone(),
			kind: CompletionItemKind::FUNCTION,
			detail: "builtin effect".to_string(),
			source: CandidateSource::Builtin,
		});
	}

	out.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.label.cmp(&b.label)));
	out.dedup_by(|a, b| a.label == b.label && a.source == b.source);
	out
}

fn build_workspace_candidates(roots: &[PathBuf]) -> Vec<CompletionCandidate> {
	let mut parsed = Vec::new();
	for root in roots {
		let files = collect_semantic_script_files(root);
		for file in files {
			if let Some(item) = parse_script_file("__workspace__", root, &file) {
				parsed.push(item);
			}
		}
	}

	let index = build_semantic_index(&parsed);
	let mut seen = HashMap::<String, CompletionCandidate>::new();
	for def in index.definitions {
		let (label, kind, detail) =
			completion_from_definition(&def.kind, &def.local_name, &def.name);
		if label.is_empty() {
			continue;
		}
		seen.entry(format!("{}::{label}", detail))
			.or_insert(CompletionCandidate {
				label: label.clone(),
				insert_text: label,
				kind,
				detail,
				source: CandidateSource::Workspace,
			});
	}

	let mut out: Vec<CompletionCandidate> = seen.into_values().collect();
	out.sort_by(|a, b| a.label.cmp(&b.label));
	out
}

fn completion_from_definition(
	kind: &SymbolKind,
	local_name: &str,
	full_name: &str,
) -> (String, CompletionItemKind, String) {
	match kind {
		SymbolKind::ScriptedEffect => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace scripted effect".to_string(),
		),
		SymbolKind::Event => (
			full_name.to_string(),
			CompletionItemKind::EVENT,
			"workspace event id".to_string(),
		),
		SymbolKind::Decision => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace decision".to_string(),
		),
		SymbolKind::DiplomaticAction => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace diplomatic action".to_string(),
		),
		SymbolKind::TriggeredModifier => (
			local_name.to_string(),
			CompletionItemKind::VARIABLE,
			"workspace triggered modifier".to_string(),
		),
	}
}

fn collect_semantic_script_files(root: &Path) -> Vec<PathBuf> {
	let targets = [
		"events",
		"decisions",
		"common/scripted_effects",
		"common/diplomatic_actions",
		"common/triggered_modifiers",
		"common/defines",
	];

	let mut files = Vec::new();
	for target in targets {
		let dir = root.join(target);
		if !dir.is_dir() {
			continue;
		}
		for entry in WalkDir::new(dir).into_iter().filter_map(|entry| entry.ok()) {
			if !entry.file_type().is_file() {
				continue;
			}
			let path = entry.path();
			let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
				continue;
			};
			if matches!(ext.to_ascii_lowercase().as_str(), "txt" | "lua") {
				files.push(path.to_path_buf());
			}
		}
	}

	files.sort();
	files.dedup();
	files
}

fn extract_completion_prefix(text: &str, position: Position) -> String {
	let line = text
		.lines()
		.nth(position.line as usize)
		.map(str::to_string)
		.unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	let chars: Vec<char> = upto.chars().collect();
	let mut start = chars.len();
	while start > 0 && is_identifier_char(chars[start - 1]) {
		start -= 1;
	}
	chars[start..].iter().collect()
}

fn is_identifier_char(ch: char) -> bool {
	ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '$' | '@' | '-')
}

#[cfg(test)]
mod tests {
	use super::extract_completion_prefix;
	use tower_lsp::lsp_types::Position;

	#[test]
	fn completion_prefix_extracts_identifier_tail() {
		let text = "add_country_mod";
		let prefix = extract_completion_prefix(
			text,
			Position {
				line: 0,
				character: 15,
			},
		);
		assert_eq!(prefix, "add_country_mod");
	}

	#[test]
	fn completion_prefix_stops_on_whitespace() {
		let text = "trigger = has_co";
		let prefix = extract_completion_prefix(
			text,
			Position {
				line: 0,
				character: 16,
			},
		);
		assert_eq!(prefix, "has_co");
	}
}
