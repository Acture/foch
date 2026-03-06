use foch::check::eu4_builtin::{
	alias_keywords, builtin_effect_names, builtin_trigger_names, contextual_keywords,
	reserved_keywords,
};
use foch::check::model::SymbolKind;
use foch::check::parser::{AstStatement, AstValue, ScalarValue};
use foch::check::semantic_index::{build_semantic_index, parse_script_file};
use serde::Deserialize;
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
	Literal,
	Builtin,
	Workspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionContext {
	Default,
	FlagValue,
}

#[derive(Clone, Debug)]
struct CompletionCandidate {
	label: String,
	insert_text: String,
	kind: CompletionItemKind,
	detail: String,
	source: CandidateSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TargetRole {
	Game,
	Mod,
}

#[derive(Clone, Debug)]
struct ScanTarget {
	path: PathBuf,
	role: TargetRole,
}

#[derive(Clone, Debug, Deserialize)]
struct EnvScanTarget {
	path: String,
	role: TargetRole,
}

#[derive(Default)]
struct ServerState {
	docs: HashMap<Url, String>,
	targets: Vec<ScanTarget>,
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
		let targets = { self.state.read().await.targets.clone() };
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || build_workspace_candidates(&targets)).await;
		match built {
			Ok(candidates) => {
				let count = candidates.len();
				let mut state = self.state.write().await;
				state.workspace_candidates = candidates;
				client
					.log_message(
						MessageType::INFO,
						format!("foch lsp workspace symbol candidates loaded: {count}"),
					)
					.await;
			}
			Err(err) => {
				self.client
					.log_message(
						MessageType::ERROR,
						format!("foch lsp failed to build workspace candidates: {err}"),
					)
					.await;
			}
		}
	}
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
	async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
		let targets = resolve_scan_targets(&params);

		{
			let mut state = self.state.write().await;
			state.targets = targets;
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
			.log_message(MessageType::INFO, "foch lsp initialized")
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
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let prefix = extract_completion_prefix(text, position);
		let context = detect_completion_context(text, position);
		let prefix_lower = prefix.to_ascii_lowercase();

		let mut candidates = select_completion_candidates(
			&state.static_candidates,
			&state.workspace_candidates,
			context,
			&prefix_lower,
		);

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
	for key in ["yes", "no", "true", "false"] {
		out.push(CompletionCandidate {
			label: key.to_string(),
			insert_text: key.to_string(),
			kind: CompletionItemKind::VALUE,
			detail: "boolean literal".to_string(),
			source: CandidateSource::Literal,
		});
	}
	for snippet in ["always = yes", "always = no"] {
		out.push(CompletionCandidate {
			label: snippet.to_string(),
			insert_text: snippet.to_string(),
			kind: CompletionItemKind::SNIPPET,
			detail: "common trigger pattern".to_string(),
			source: CandidateSource::Literal,
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

fn build_workspace_candidates(roots: &[ScanTarget]) -> Vec<CompletionCandidate> {
	let mut parsed = Vec::new();
	for target in roots {
		let files = collect_semantic_script_files(&target.path);
		for file in files {
			let mod_id = match target.role {
				TargetRole::Game => "__lsp_game__",
				TargetRole::Mod => "__lsp_mod__",
			};
			if let Some(item) = parse_script_file(mod_id, &target.path, &file) {
				parsed.push(item);
			}
		}
	}

	let index = build_semantic_index(&parsed);
	let mut seen = HashMap::<String, CompletionCandidate>::new();
	for def in &index.definitions {
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
	for usage in &index.key_usages {
		if !is_workspace_key_candidate(&usage.key) {
			continue;
		}
		let label = usage.key.clone();
		seen.entry(format!("workspace-key::{label}"))
			.or_insert(CompletionCandidate {
				label: label.clone(),
				insert_text: label,
				kind: CompletionItemKind::KEYWORD,
				detail: "workspace key".to_string(),
				source: CandidateSource::Workspace,
			});
	}
	for scalar in collect_workspace_scalars(&parsed) {
		seen.entry(format!("workspace-scalar::{scalar}"))
			.or_insert(CompletionCandidate {
				label: scalar.clone(),
				insert_text: scalar,
				kind: CompletionItemKind::CONSTANT,
				detail: "workspace scalar value".to_string(),
				source: CandidateSource::Workspace,
			});
	}
	for (flag_kind, flag_value) in collect_workspace_flag_values(&index) {
		seen.entry(format!("workspace-flag::{flag_kind}::{flag_value}"))
			.or_insert(CompletionCandidate {
				label: flag_value.clone(),
				insert_text: flag_value,
				kind: CompletionItemKind::VARIABLE,
				detail: format!("workspace {flag_kind} flag"),
				source: CandidateSource::Workspace,
			});
	}

	let mut out: Vec<CompletionCandidate> = seen.into_values().collect();
	out.sort_by(|a, b| a.label.cmp(&b.label));
	out
}

fn collect_workspace_flag_values(
	index: &foch::check::model::SemanticIndex,
) -> Vec<(&'static str, String)> {
	let mut out = Vec::new();
	for usage in &index.scalar_assignments {
		let Some(flag_kind) = flag_value_kind(usage.key.as_str()) else {
			continue;
		};
		if !is_workspace_scalar_candidate(usage.value.as_str()) {
			continue;
		}
		out.push((flag_kind, usage.value.clone()));
	}
	out.sort_by(|lhs, rhs| lhs.1.cmp(&rhs.1).then(lhs.0.cmp(rhs.0)));
	out.dedup_by(|lhs, rhs| lhs.0 == rhs.0 && lhs.1 == rhs.1);
	out
}

fn flag_value_kind(key: &str) -> Option<&'static str> {
	match key {
		"set_global_flag" | "has_global_flag" | "clr_global_flag" | "had_global_flag" => {
			Some("global")
		}
		"set_country_flag" | "has_country_flag" | "clr_country_flag" | "had_country_flag" => {
			Some("country")
		}
		"set_province_flag"
		| "set_permanent_province_flag"
		| "has_province_flag"
		| "clr_province_flag"
		| "had_province_flag" => Some("province"),
		"set_ruler_flag" | "has_ruler_flag" | "clr_ruler_flag" | "had_ruler_flag" => Some("ruler"),
		"set_heir_flag" | "has_heir_flag" | "clr_heir_flag" | "had_heir_flag" => Some("heir"),
		"set_consort_flag" | "has_consort_flag" | "clr_consort_flag" | "had_consort_flag" => {
			Some("consort")
		}
		_ => None,
	}
}

fn is_workspace_key_candidate(key: &str) -> bool {
	if key.is_empty() || key.len() > 128 {
		return false;
	}
	let mut chars = key.chars();
	let Some(first) = chars.next() else {
		return false;
	};
	if !matches!(first, 'A'..='Z' | 'a'..='z' | '_') {
		return false;
	}
	chars.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | ':' | '@' | '.'))
}

fn collect_workspace_scalars(
	files: &[foch::check::semantic_index::ParsedScriptFile],
) -> Vec<String> {
	let mut out = Vec::new();
	for file in files {
		for stmt in &file.ast.statements {
			collect_scalars_from_statement(stmt, &mut out);
		}
	}
	out.sort();
	out.dedup();
	out
}

fn collect_scalars_from_statement(stmt: &AstStatement, out: &mut Vec<String>) {
	match stmt {
		AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
			collect_scalars_from_value(value, out)
		}
		AstStatement::Comment { .. } => {}
	}
}

fn collect_scalars_from_value(value: &AstValue, out: &mut Vec<String>) {
	match value {
		AstValue::Scalar { value, .. } => {
			if let ScalarValue::Identifier(text) = value
				&& is_workspace_scalar_candidate(text)
			{
				out.push(text.clone());
			}
		}
		AstValue::Block { items, .. } => {
			for item in items {
				collect_scalars_from_statement(item, out);
			}
		}
	}
}

fn is_workspace_scalar_candidate(value: &str) -> bool {
	if value.is_empty() || value.len() > 128 || value == "<parse-error>" {
		return false;
	}
	if !value
		.chars()
		.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | ':' | '@' | '.'))
	{
		return false;
	}
	let has_separator = value.contains('_') || value.contains('.');
	let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
	has_separator || has_upper
}

fn resolve_scan_targets(params: &InitializeParams) -> Vec<ScanTarget> {
	match scan_targets_from_env() {
		Ok(targets) if !targets.is_empty() => targets,
		Ok(_) | Err(_) => scan_targets_from_workspace(params),
	}
}

fn scan_targets_from_env() -> std::result::Result<Vec<ScanTarget>, String> {
	let raw = match std::env::var("FOCH_LSP_TARGETS_JSON") {
		Ok(value) => value,
		Err(std::env::VarError::NotPresent) => return Ok(Vec::new()),
		Err(err) => return Err(format!("read FOCH_LSP_TARGETS_JSON failed: {err}")),
	};

	parse_scan_targets_json(&raw)
}

fn parse_scan_targets_json(raw: &str) -> std::result::Result<Vec<ScanTarget>, String> {
	let parsed: Vec<EnvScanTarget> = serde_json::from_str(raw)
		.map_err(|err| format!("parse FOCH_LSP_TARGETS_JSON failed: {err}"))?;
	let mut targets = Vec::new();
	for item in parsed {
		let path = PathBuf::from(item.path);
		if path.is_dir() {
			targets.push(ScanTarget {
				path,
				role: item.role,
			});
		}
	}
	Ok(dedup_scan_targets(targets))
}

fn scan_targets_from_workspace(params: &InitializeParams) -> Vec<ScanTarget> {
	let mut targets = Vec::new();
	if let Some(folders) = params.workspace_folders.as_ref() {
		for folder in folders {
			if let Ok(path) = folder.uri.to_file_path() {
				targets.push(ScanTarget {
					path,
					role: TargetRole::Mod,
				});
			}
		}
	}
	if targets.is_empty()
		&& let Some(root_uri) = params.root_uri.as_ref()
		&& let Ok(path) = root_uri.to_file_path()
	{
		targets.push(ScanTarget {
			path,
			role: TargetRole::Mod,
		});
	}

	dedup_scan_targets(targets)
}

fn dedup_scan_targets(targets: Vec<ScanTarget>) -> Vec<ScanTarget> {
	let mut seen = HashMap::<String, TargetRole>::new();
	let mut out = Vec::new();
	for item in targets {
		let key = item.path.to_string_lossy().replace('\\', "/");
		if seen.contains_key(&key) {
			continue;
		}
		seen.insert(key, item.role);
		out.push(item);
	}
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

fn select_completion_candidates(
	static_candidates: &[CompletionCandidate],
	workspace_candidates: &[CompletionCandidate],
	context: CompletionContext,
	prefix_lower: &str,
) -> Vec<CompletionCandidate> {
	if prefix_lower.is_empty() {
		return match context {
			CompletionContext::FlagValue => workspace_candidates
				.iter()
				.filter(|item| is_flag_completion_candidate(item))
				.cloned()
				.collect(),
			CompletionContext::Default => static_candidates
				.iter()
				.filter(|item| {
					item.source == CandidateSource::Keyword
						|| item.source == CandidateSource::Literal
				})
				.cloned()
				.collect(),
		};
	}

	if prefix_lower.len() < 2 {
		let iter: Box<dyn Iterator<Item = &CompletionCandidate>> = match context {
			CompletionContext::FlagValue => Box::new(
				static_candidates
					.iter()
					.chain(workspace_candidates.iter())
					.filter(|item| {
						is_flag_completion_candidate(item)
							|| item.source == CandidateSource::Literal
							|| item.source == CandidateSource::Keyword
					}),
			),
			CompletionContext::Default => Box::new(static_candidates.iter()),
		};
		return iter
			.filter(|item| item.label.to_ascii_lowercase().starts_with(prefix_lower))
			.cloned()
			.collect();
	}

	static_candidates
		.iter()
		.chain(workspace_candidates.iter())
		.filter(|item| item.label.to_ascii_lowercase().starts_with(prefix_lower))
		.cloned()
		.collect()
}

fn is_flag_completion_candidate(item: &CompletionCandidate) -> bool {
	item.detail.starts_with("workspace ") && item.detail.ends_with(" flag")
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

fn detect_completion_context(text: &str, position: Position) -> CompletionContext {
	let line = text
		.lines()
		.nth(position.line as usize)
		.map(str::to_string)
		.unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	let Some(key) = current_assignment_key(&upto) else {
		return CompletionContext::Default;
	};
	if flag_value_kind(key).is_some() {
		CompletionContext::FlagValue
	} else {
		CompletionContext::Default
	}
}

fn current_assignment_key(line_prefix: &str) -> Option<&str> {
	let eq = line_prefix.rfind('=')?;
	let before = line_prefix[..eq].trim_end();
	if before.is_empty() {
		return None;
	}
	let mut start = before.len();
	let bytes = before.as_bytes();
	while start > 0 {
		let ch = bytes[start - 1] as char;
		if is_identifier_char(ch) {
			start -= 1;
		} else {
			break;
		}
	}
	if start == before.len() {
		return None;
	}
	Some(&before[start..])
}

fn is_identifier_char(ch: char) -> bool {
	ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '$' | '@' | '-')
}

#[cfg(test)]
mod tests {
	use super::{
		CandidateSource, CompletionCandidate, CompletionContext, TargetRole,
		detect_completion_context, extract_completion_prefix, parse_scan_targets_json,
		select_completion_candidates,
	};
	use tower_lsp::lsp_types::CompletionItemKind;
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

	#[test]
	fn env_targets_parse_json() {
		let targets = parse_scan_targets_json(
			r#"[{"path":"/tmp","role":"game"},{"path":"/Users/nope","role":"mod"}]"#,
		)
		.expect("parse targets json");
		assert!(!targets.is_empty());
		assert_eq!(targets[0].role, TargetRole::Game);
	}

	#[test]
	fn detects_flag_completion_context() {
		let text = "has_country_flag = ";
		let context = detect_completion_context(
			text,
			Position {
				line: 0,
				character: text.len() as u32,
			},
		);
		assert_eq!(context, CompletionContext::FlagValue);
	}

	#[test]
	fn empty_prefix_in_flag_context_returns_workspace_flags() {
		let static_candidates = vec![CompletionCandidate {
			label: "always = yes".to_string(),
			insert_text: "always = yes".to_string(),
			kind: CompletionItemKind::SNIPPET,
			detail: "common trigger pattern".to_string(),
			source: CandidateSource::Literal,
		}];
		let workspace_candidates = vec![
			CompletionCandidate {
				label: "CTRLMA_open_config_menu_flag".to_string(),
				insert_text: "CTRLMA_open_config_menu_flag".to_string(),
				kind: CompletionItemKind::VARIABLE,
				detail: "workspace country flag".to_string(),
				source: CandidateSource::Workspace,
			},
			CompletionCandidate {
				label: "CTRLMA_config_events.0".to_string(),
				insert_text: "CTRLMA_config_events.0".to_string(),
				kind: CompletionItemKind::EVENT,
				detail: "workspace event id".to_string(),
				source: CandidateSource::Workspace,
			},
		];

		let selected = select_completion_candidates(
			&static_candidates,
			&workspace_candidates,
			CompletionContext::FlagValue,
			"",
		);

		assert_eq!(selected.len(), 1);
		assert_eq!(selected[0].label, "CTRLMA_open_config_menu_flag");
	}
}
