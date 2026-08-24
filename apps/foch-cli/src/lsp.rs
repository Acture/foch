use foch::game::eu4::analysis::{AnalyzeOptions, analyze_visibility};
use foch::game::eu4::base::builtin::{
	alias_keywords, builtin_effect_names, builtin_trigger_names, contextual_keywords,
	reserved_keywords,
};
use foch::game::eu4::editor::schema::{
	EditorPosition, EditorRange, EditorSchema, SchemaCompletion, SchemaCompletionKind,
	SchemaDiagnostic as EditorSchemaDiagnostic, SchemaDocument, SchemaHover, SchemaLoadStatus,
	SchemaWorkspace,
};
use foch::game::eu4::editor::workspace::WorkspaceSession;
use foch::game::eu4::script::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use foch::game::eu4::script::{
	ParsedScriptFile, build_semantic_index, collect_localisation_definitions, parse_script_file,
	resolve_symbol_reference_targets,
};
use foch::input::{
	Config, InputRequest, InputSource, InputTargetRole, load_or_init_config, resolve_input_targets,
};
use foch::model::{
	AnalysisMode, DocumentFamily, DocumentRecord, Finding, LocalisationDefinition, SemanticIndex,
	Severity, SymbolDefinition, SymbolKind as FochSymbolKind,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
	CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
	CodeActionProviderCapability, CodeActionResponse, Command, CompletionItem, CompletionItemKind,
	CompletionOptions, CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity,
	DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
	DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
	Hover, HoverContents, HoverParams, InitializeParams, InitializeResult, InitializedParams,
	Location, MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf, Position, Range,
	ReferenceParams, ServerCapabilities, SymbolInformation, SymbolKind as LspSymbolKind,
	TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer, LspService, Server};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CandidateSource {
	Keyword,
	Literal,
	Schema,
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

#[derive(Clone, Debug, Default)]
struct WorkspaceSnapshot {
	candidates: Vec<CompletionCandidate>,
	schema_workspace: SchemaWorkspace,
	session: Option<WorkspaceSession>,
	diagnostics_by_path: HashMap<String, Vec<Diagnostic>>,
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
	workspace: Option<Arc<WorkspaceSnapshot>>,
}

struct Backend {
	client: Client,
	state: Arc<RwLock<ServerState>>,
	schema: Arc<RwLock<Option<EditorSchema>>>,
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
			schema: Arc::new(RwLock::new(None)),
		}
	}

	async fn refresh_workspace_snapshot(&self) {
		let targets = { self.state.read().await.targets.clone() };
		let schema = self.schema.read().await.clone();
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || {
			build_workspace_snapshot_with_schema(&targets, schema)
		})
		.await;
		match built {
			Ok(snapshot) => {
				let candidate_count = snapshot.candidates.len();
				let finding_count: usize =
					snapshot.diagnostics_by_path.values().map(Vec::len).sum();
				let snapshot = Arc::new(snapshot);
				let mut state = self.state.write().await;
				state.workspace = Some(snapshot.clone());
				drop(state);
				self.publish_workspace_diagnostics(snapshot.as_ref()).await;
				client
					.log_message(
						MessageType::INFO,
						format!(
							"foch lsp workspace snapshot loaded: {candidate_count} candidates, {finding_count} diagnostics"
						),
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

	async fn publish_workspace_diagnostics(&self, snapshot: &WorkspaceSnapshot) {
		let file_paths = snapshot
			.session
			.as_ref()
			.map(|s| s.file_paths.as_slice())
			.unwrap_or(&[]);
		for path in file_paths {
			let Some(uri) = Url::from_file_path(path).ok() else {
				continue;
			};
			let key = normalize_path(path);
			let diagnostics = snapshot
				.diagnostics_by_path
				.get(&key)
				.cloned()
				.unwrap_or_default();
			self.client
				.publish_diagnostics(uri, diagnostics, None)
				.await;
		}
	}

	async fn publish_document_diagnostics(&self, uri: &Url, text: &str) {
		let path = match uri.to_file_path() {
			Ok(path) => path,
			Err(_) => return,
		};
		let (snapshot, targets) = {
			let state = self.state.read().await;
			(state.workspace.clone(), state.targets.clone())
		};
		let schema = self.schema.read().await.clone();
		let relative_path = match_scan_target(&targets, &path).map(|(_, relative)| relative);
		let mut diagnostics = snapshot
			.as_ref()
			.and_then(|snapshot| {
				snapshot
					.diagnostics_by_path
					.get(&normalize_path(&path))
					.cloned()
			})
			.unwrap_or_default();
		diagnostics.extend(parse_diagnostics_for_text(&path, text));
		if let (Some(schema), Some(relative_path)) = (schema.as_ref(), relative_path.as_ref()) {
			diagnostics.extend(schema_diagnostics_for_text_with_index(
				schema,
				relative_path,
				text,
				snapshot.as_ref().map(|snapshot| &snapshot.schema_workspace),
			));
			if let Some(session) = snapshot
				.as_ref()
				.and_then(|snapshot| snapshot.session.as_ref())
			{
				diagnostics.extend(schema_localisation_diagnostics_for_text(
					schema,
					relative_path,
					text,
					&session.index.localisation_definitions,
				));
			}
		}
		sort_and_dedup_diagnostics(&mut diagnostics);
		self.client
			.publish_diagnostics(uri.clone(), diagnostics, None)
			.await;
	}
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
	async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
		let targets = resolve_scan_targets(&params);
		let started = Instant::now();
		let load_result = tokio::task::spawn_blocking(EditorSchema::active).await;
		let elapsed = started.elapsed();
		let schema = match load_result {
			Ok(Some(schema)) => {
				let info = schema.info();
				self.client
					.log_message(
						MessageType::INFO,
						format!(
							"foch lsp loaded compiled CWT rule pack: {} roots, {} aliases, {}, source {}, hash {:.1} ms, cache {}, compile {}, total {:.1} ms, task {:.1} ms",
							info.root_count,
							info.alias_count,
							schema_load_status_label(info.status),
							short_source_id(&info.source_id),
							duration_ms(info.timings.source_hash),
							optional_duration_ms(info.timings.cache_read),
							optional_duration_ms(info.timings.source_compile),
							duration_ms(info.timings.total),
							elapsed.as_secs_f64() * 1000.0
						),
					)
					.await;
				Some(schema)
			}
			Ok(None) => {
				self.client
					.log_message(
						MessageType::WARNING,
						"foch lsp missing vendored CWT schema directory; schema-aware features disabled",
					)
					.await;
				None
			}
			Err(err) => {
				self.client
					.log_message(
						MessageType::ERROR,
						format!("foch lsp schema load task failed: {err}"),
					)
					.await;
				None
			}
		};

		{
			let mut state = self.state.write().await;
			state.targets = targets;
		}
		*self.schema.write().await = schema;

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
				definition_provider: Some(OneOf::Left(true)),
				hover_provider: Some(tower_lsp::lsp_types::HoverProviderCapability::Simple(true)),
				references_provider: Some(OneOf::Left(true)),
				document_symbol_provider: Some(OneOf::Left(true)),
				workspace_symbol_provider: Some(OneOf::Left(true)),
				code_action_provider: Some(CodeActionProviderCapability::Options(
					CodeActionOptions {
						code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
						resolve_provider: Some(false),
						work_done_progress_options: Default::default(),
					},
				)),
				..ServerCapabilities::default()
			},
		})
	}

	async fn initialized(&self, _: InitializedParams) {
		self.client
			.log_message(MessageType::INFO, "foch lsp initialized")
			.await;
		self.refresh_workspace_snapshot().await;
	}

	async fn did_open(&self, params: DidOpenTextDocumentParams) {
		let uri = params.text_document.uri;
		let text = params.text_document.text;
		{
			let mut state = self.state.write().await;
			state.docs.insert(uri.clone(), text.clone());
		}
		self.publish_document_diagnostics(&uri, &text).await;
	}

	async fn did_change(&self, params: DidChangeTextDocumentParams) {
		if let Some(last) = params.content_changes.last() {
			{
				let mut state = self.state.write().await;
				state
					.docs
					.insert(params.text_document.uri.clone(), last.text.clone());
			}
			self.publish_document_diagnostics(&params.text_document.uri, &last.text)
				.await;
		}
	}

	async fn did_save(&self, params: DidSaveTextDocumentParams) {
		if let Some(text) = params.text {
			let mut state = self.state.write().await;
			state.docs.insert(params.text_document.uri, text);
		}
		self.refresh_workspace_snapshot().await;
	}

	async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
		let uri = &params.text_document_position_params.text_document.uri;
		let position = params.text_document_position_params.position;
		let targets = { self.state.read().await.targets.clone() };
		let (text, schema_workspace) = {
			let state = self.state.read().await;
			(
				state.docs.get(uri).cloned(),
				state
					.workspace
					.as_ref()
					.map(|snapshot| snapshot.schema_workspace.clone()),
			)
		};
		let Some(text) = text else {
			return Ok(None);
		};
		let Some(schema) = self.schema.read().await.clone() else {
			return Ok(None);
		};
		let path = match uri.to_file_path() {
			Ok(path) => path,
			Err(_) => return Ok(None),
		};
		let Some((_, relative_path)) = match_scan_target(&targets, &path) else {
			return Ok(None);
		};
		Ok(schema_hover(
			&schema,
			&relative_path,
			&text,
			position,
			schema_workspace.as_ref(),
		))
	}

	async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
		let schema = self.schema.read().await.clone();
		let state = self.state.read().await;
		let uri = &params.text_document_position.text_document.uri;
		let position = params.text_document_position.position;
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let prefix = extract_completion_prefix(text, position);
		let context = detect_completion_context(text, position);
		let prefix_lower = prefix.to_ascii_lowercase();

		let mut candidates = if let Some(schema) = schema.as_ref()
			&& let Ok(path) = uri.to_file_path()
			&& let Some((_, relative_path)) = match_scan_target(&state.targets, &path)
			&& let Some(candidates) = schema_completion_candidates_with_index(
				schema,
				&relative_path,
				text,
				position,
				&prefix_lower,
				state
					.workspace
					.as_ref()
					.map(|snapshot| &snapshot.schema_workspace),
			) {
			candidates
		} else {
			select_completion_candidates(
				&state.static_candidates,
				state
					.workspace
					.as_ref()
					.map(|snapshot| snapshot.candidates.as_slice())
					.unwrap_or(&[]),
				context,
				&prefix_lower,
			)
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

	async fn goto_definition(
		&self,
		params: GotoDefinitionParams,
	) -> Result<Option<GotoDefinitionResponse>> {
		let state = self.state.read().await;
		let Some(snapshot) = state.workspace.as_ref() else {
			return Ok(None);
		};
		let uri = &params.text_document_position_params.text_document.uri;
		let position = params.text_document_position_params.position;
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let Some(locations) =
			resolve_definition_locations(snapshot, &state.targets, uri, text, position)
		else {
			return Ok(None);
		};
		Ok(Some(GotoDefinitionResponse::Array(locations)))
	}

	async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
		let state = self.state.read().await;
		let Some(snapshot) = state.workspace.as_ref() else {
			return Ok(None);
		};
		let uri = &params.text_document_position.text_document.uri;
		let position = params.text_document_position.position;
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let Some(locations) = resolve_reference_locations(
			snapshot,
			&state.targets,
			uri,
			text,
			position,
			params.context.include_declaration,
		) else {
			return Ok(None);
		};
		Ok(Some(locations))
	}

	async fn document_symbol(
		&self,
		params: DocumentSymbolParams,
	) -> Result<Option<DocumentSymbolResponse>> {
		let state = self.state.read().await;
		let Some(snapshot) = state.workspace.as_ref() else {
			return Ok(None);
		};
		let Some(symbols) = document_symbols(snapshot, &state.targets, &params.text_document.uri)
		else {
			return Ok(None);
		};
		Ok(Some(DocumentSymbolResponse::Flat(symbols)))
	}

	async fn symbol(
		&self,
		params: tower_lsp::lsp_types::WorkspaceSymbolParams,
	) -> Result<Option<Vec<SymbolInformation>>> {
		let state = self.state.read().await;
		let Some(snapshot) = state.workspace.as_ref() else {
			return Ok(None);
		};
		let symbols = workspace_symbols(snapshot, &params.query);
		Ok(Some(symbols))
	}

	async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
		if !code_action_context_allows_quickfix(&params) {
			return Ok(None);
		}
		let state = self.state.read().await;
		let actions = localisation_stub_code_actions(&state.targets, &params);
		if actions.is_empty() {
			Ok(None)
		} else {
			Ok(Some(actions))
		}
	}

	async fn shutdown(&self) -> Result<()> {
		Ok(())
	}
}

/// Run the foch LSP server on stdio. Wrapper around the `tower_lsp` server
/// loop that spins up its own tokio runtime so the synchronous CLI dispatch
/// in `cli::handler::lsp` can call into it without becoming async itself.
pub fn run() -> i32 {
	let runtime = match tokio::runtime::Runtime::new() {
		Ok(rt) => rt,
		Err(err) => {
			eprintln!("foch lsp: failed to start tokio runtime: {err}");
			return 1;
		}
	};
	runtime.block_on(async {
		let stdin = tokio::io::stdin();
		let stdout = tokio::io::stdout();
		let (service, socket) = LspService::new(Backend::new);
		Server::new(stdin, stdout, socket).serve(service).await;
	});
	0
}

fn schema_load_status_label(status: SchemaLoadStatus) -> &'static str {
	match status {
		SchemaLoadStatus::CacheHit => "cache hit",
		SchemaLoadStatus::CompiledFromSource => "compiled from source",
	}
}

fn short_source_id(source_id: &str) -> &str {
	source_id.get(..12).unwrap_or(source_id)
}

fn duration_ms(duration: Duration) -> f64 {
	duration.as_secs_f64() * 1000.0
}

fn optional_duration_ms(duration: Option<Duration>) -> String {
	duration
		.map(|duration| format!("{:.1} ms", duration_ms(duration)))
		.unwrap_or_else(|| "n/a".to_string())
}

fn editor_position(position: Position) -> EditorPosition {
	EditorPosition {
		line: position.line,
		character: position.character,
	}
}

fn lsp_range_from_editor(range: EditorRange) -> Range {
	Range {
		start: Position {
			line: range.start.line,
			character: range.start.character,
		},
		end: Position {
			line: range.end.line,
			character: range.end.character,
		},
	}
}

fn schema_hover(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	position: Position,
	workspace: Option<&SchemaWorkspace>,
) -> Option<Hover> {
	let hover = schema.hover(file_path, text, editor_position(position), workspace)?;
	Some(schema_hover_view(hover))
}

fn schema_hover_view(hover: SchemaHover) -> Hover {
	Hover {
		contents: HoverContents::Markup(MarkupContent {
			kind: MarkupKind::Markdown,
			value: hover.markdown,
		}),
		range: Some(lsp_range_from_editor(hover.range)),
	}
}

fn schema_completion_candidates_with_index(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	position: Position,
	prefix_lower: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Option<Vec<CompletionCandidate>> {
	schema
		.completions(
			file_path,
			text,
			editor_position(position),
			prefix_lower,
			workspace,
		)
		.map(|items| items.into_iter().map(schema_completion_candidate).collect())
}

fn schema_completion_candidate(completion: SchemaCompletion) -> CompletionCandidate {
	CompletionCandidate {
		label: completion.label,
		insert_text: completion.insert_text,
		kind: match completion.kind {
			SchemaCompletionKind::Field => CompletionItemKind::FIELD,
			SchemaCompletionKind::Function => CompletionItemKind::FUNCTION,
			SchemaCompletionKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
			SchemaCompletionKind::Value => CompletionItemKind::VALUE,
			SchemaCompletionKind::Reference => CompletionItemKind::REFERENCE,
		},
		detail: completion.detail,
		source: CandidateSource::Schema,
	}
}

fn schema_diagnostics_for_text_with_index(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	workspace: Option<&SchemaWorkspace>,
) -> Vec<Diagnostic> {
	schema
		.diagnostics(file_path, text, workspace)
		.into_iter()
		.map(schema_diagnostic)
		.collect()
}

fn schema_localisation_diagnostics_for_text(
	schema: &EditorSchema,
	file_path: &Path,
	text: &str,
	definitions: &[LocalisationDefinition],
) -> Vec<Diagnostic> {
	schema
		.localisation_diagnostics(file_path, text, definitions)
		.into_iter()
		.map(schema_diagnostic)
		.collect()
}

fn schema_diagnostic(diagnostic: EditorSchemaDiagnostic) -> Diagnostic {
	Diagnostic {
		range: lsp_range_from_editor(diagnostic.range),
		severity: diagnostic.severity.map(|severity| match severity {
			Severity::Error => DiagnosticSeverity::ERROR,
			Severity::Warning => DiagnosticSeverity::WARNING,
			Severity::Info => DiagnosticSeverity::INFORMATION,
		}),
		code: diagnostic.code.map(NumberOrString::String),
		source: diagnostic.source,
		message: diagnostic.message,
		..Diagnostic::default()
	}
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

#[cfg(test)]
fn build_workspace_snapshot(roots: &[ScanTarget]) -> WorkspaceSnapshot {
	build_workspace_snapshot_with_schema(roots, None)
}

fn build_workspace_snapshot_with_schema(
	roots: &[ScanTarget],
	schema: Option<EditorSchema>,
) -> WorkspaceSnapshot {
	let mut parsed = Vec::new();
	let mut file_paths = Vec::new();
	let mut path_lookup = HashMap::new();
	let mut localisation_definitions = Vec::new();
	for target in roots {
		let files = collect_semantic_script_files(&target.path);
		let mod_id = scan_target_mod_id(target);
		for file in files {
			if let Some(item) = parse_script_file(&mod_id, &target.path, &file) {
				file_paths.push(item.path.clone());
				path_lookup.insert(
					path_lookup_key(&item.mod_id, &item.relative_path),
					item.path.clone(),
				);
				parsed.push(item);
			}
		}
		let definitions = collect_localisation_definitions(&mod_id, &target.path);
		for definition in &definitions {
			let path = target.path.join(&definition.path);
			file_paths.push(path.clone());
			path_lookup.insert(
				path_lookup_key(&definition.mod_id, &definition.path),
				path.clone(),
			);
		}
		localisation_definitions.extend(definitions);
	}

	let mut index = build_semantic_index(&parsed);
	let mut localisation_documents = HashSet::new();
	for definition in &localisation_definitions {
		if localisation_documents.insert(path_lookup_key(&definition.mod_id, &definition.path)) {
			index.documents.push(DocumentRecord {
				mod_id: definition.mod_id.clone(),
				path: definition.path.clone(),
				family: DocumentFamily::Localisation,
				parse_ok: true,
			});
		}
	}
	index
		.localisation_definitions
		.extend(localisation_definitions);
	let diagnostics = analyze_visibility(
		&index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
	);
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

	let mut candidates: Vec<CompletionCandidate> = seen.into_values().collect();
	candidates.sort_by(|a, b| a.label.cmp(&b.label));
	file_paths.sort();
	file_paths.dedup();

	let findings: Vec<Finding> = diagnostics
		.strict
		.into_iter()
		.chain(diagnostics.advisory)
		.collect();
	let schema_workspace = schema
		.as_ref()
		.map(|schema| {
			let documents = parsed
				.iter()
				.map(|file| SchemaDocument::new(&file.relative_path, &file.source))
				.collect::<Vec<_>>();
			schema.workspace(&documents)
		})
		.unwrap_or_default();
	let mut diagnostics_by_path = build_workspace_diagnostics(&index, &path_lookup, &findings);
	if let Some(schema) = schema.as_ref() {
		for file in &parsed {
			let schema_diagnostics = schema_diagnostics_for_text_with_index(
				schema,
				&file.relative_path,
				&file.source,
				Some(&schema_workspace),
			);
			let localisation_diagnostics = schema_localisation_diagnostics_for_text(
				schema,
				&file.relative_path,
				&file.source,
				&index.localisation_definitions,
			);
			if schema_diagnostics.is_empty() && localisation_diagnostics.is_empty() {
				continue;
			}
			let diagnostics = schema_diagnostics
				.into_iter()
				.chain(localisation_diagnostics)
				.collect::<Vec<_>>();
			diagnostics_by_path
				.entry(normalize_path(&file.path))
				.or_default()
				.extend(diagnostics);
		}
		for diagnostics in diagnostics_by_path.values_mut() {
			sort_and_dedup_diagnostics(diagnostics);
		}
	}

	let session = WorkspaceSession::from_analysis(index, file_paths, path_lookup, findings);

	WorkspaceSnapshot {
		candidates,
		schema_workspace,
		diagnostics_by_path,
		session: Some(session),
	}
}

fn build_workspace_diagnostics(
	index: &SemanticIndex,
	path_lookup: &HashMap<String, PathBuf>,
	findings: &[Finding],
) -> HashMap<String, Vec<Diagnostic>> {
	let mut diagnostics_by_path = HashMap::<String, Vec<Diagnostic>>::new();

	for issue in &index.parse_issues {
		let Some(path) = path_lookup.get(&path_lookup_key(&issue.mod_id, &issue.path)) else {
			continue;
		};
		diagnostics_by_path
			.entry(normalize_path(path))
			.or_default()
			.push(parse_issue_to_diagnostic(
				issue.line,
				issue.column,
				&issue.message,
			));
	}

	for finding in findings {
		let Some(relative_path) = finding.path.as_ref() else {
			continue;
		};
		let Some(mod_id) = finding.mod_id.as_deref() else {
			continue;
		};
		let Some(path) = path_lookup.get(&path_lookup_key(mod_id, relative_path)) else {
			continue;
		};
		diagnostics_by_path
			.entry(normalize_path(path))
			.or_default()
			.push(finding_to_diagnostic(finding));
	}

	for diagnostics in diagnostics_by_path.values_mut() {
		sort_and_dedup_diagnostics(diagnostics);
	}

	diagnostics_by_path
}

fn sort_and_dedup_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
	diagnostics.sort_by(|lhs, rhs| {
		range_start(&lhs.range)
			.cmp(&range_start(&rhs.range))
			.then_with(|| lhs.message.cmp(&rhs.message))
	});
	diagnostics.dedup_by(|lhs, rhs| {
		lhs.range == rhs.range && lhs.code == rhs.code && lhs.message == rhs.message
	});
}

fn scan_target_mod_id(target: &ScanTarget) -> String {
	let role = match target.role {
		TargetRole::Game => "game",
		TargetRole::Mod => "mod",
	};
	format!("__lsp_{role}__{}", normalize_path(&target.path))
}

fn path_lookup_key(mod_id: &str, relative_path: &Path) -> String {
	format!("{mod_id}|{}", normalize_path(relative_path))
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn collect_workspace_flag_values(index: &SemanticIndex) -> Vec<(&'static str, String)> {
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

fn collect_workspace_scalars(files: &[ParsedScriptFile]) -> Vec<String> {
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

fn parse_diagnostics_for_text(path: &Path, text: &str) -> Vec<Diagnostic> {
	let parsed = parse_clausewitz_content(path.to_path_buf(), text);
	parsed
		.diagnostics
		.into_iter()
		.map(|item| {
			parse_issue_to_diagnostic(item.span.start.line, item.span.start.column, &item.message)
		})
		.collect()
}

fn parse_issue_to_diagnostic(line: usize, column: usize, message: &str) -> Diagnostic {
	Diagnostic {
		range: lsp_range(line, column),
		severity: Some(DiagnosticSeverity::ERROR),
		code: Some(NumberOrString::String("PARSE".to_string())),
		source: Some("foch".to_string()),
		message: message.to_string(),
		..Diagnostic::default()
	}
}

fn finding_to_diagnostic(finding: &Finding) -> Diagnostic {
	let severity = match finding.severity {
		Severity::Error => DiagnosticSeverity::ERROR,
		Severity::Warning => DiagnosticSeverity::WARNING,
		Severity::Info => DiagnosticSeverity::INFORMATION,
	};
	let mut message = finding.message.clone();
	if let Some(evidence) = finding.evidence.as_ref()
		&& !evidence.is_empty()
	{
		message.push('\n');
		message.push_str(evidence);
	}
	Diagnostic {
		range: lsp_range(finding.line.unwrap_or(1), finding.column.unwrap_or(1)),
		severity: Some(severity),
		code: Some(NumberOrString::String(finding.rule_id.clone())),
		source: Some("foch".to_string()),
		message,
		..Diagnostic::default()
	}
}

fn lsp_range(line: usize, column: usize) -> Range {
	let line = line.saturating_sub(1) as u32;
	let start = column.saturating_sub(1) as u32;
	Range {
		start: Position {
			line,
			character: start,
		},
		end: Position {
			line,
			character: start.saturating_add(1),
		},
	}
}

fn range_start(range: &Range) -> (u32, u32) {
	(range.start.line, range.start.character)
}

fn resolve_definition_locations(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	uri: &Url,
	text: &str,
	position: Position,
) -> Option<Vec<Location>> {
	let session = snapshot.session.as_ref()?;
	let path = uri.to_file_path().ok()?;
	let (_, relative_path) = match_scan_target(targets, &path)?;
	let line = text.lines().nth(position.line as usize)?;
	let cursor = position.character as usize;
	let (token, _token_start, _) = extract_token_at_cursor(line, cursor)?;
	let (assignment_key, key_start, key_end, on_value_side) =
		assignment_context_at_cursor(line, cursor)?;
	let mut locations = Vec::new();

	if !on_value_side && cursor >= key_start && cursor <= key_end {
		let current_column = key_start + 1;
		for reference in &session.index.references {
			if reference.path != relative_path
				|| reference.line != position.line as usize + 1
				|| reference.column != current_column
				|| reference.name != assignment_key
			{
				continue;
			}
			for target in resolve_symbol_reference_targets(&session.index, reference) {
				if let Some(definition) = session.index.definitions.get(target)
					&& let Some(location) = definition_location(
						session,
						&definition.mod_id,
						&definition.path,
						definition.line,
						definition.column,
					) {
					locations.push(location);
				}
			}
		}
	} else if on_value_side {
		if assignment_key == "id" {
			for definition in session.find_definitions(&token, Some(FochSymbolKind::Event)) {
				if let Some(location) = definition_location(
					session,
					&definition.mod_id,
					&definition.path,
					definition.line,
					definition.column,
				) {
					locations.push(location);
				}
			}
		} else if is_localisation_reference_key(&assignment_key, &token) {
			for definition in &session.index.localisation_definitions {
				if definition.key != token {
					continue;
				}
				if let Some(location) = definition_location(
					session,
					&definition.mod_id,
					&definition.path,
					definition.line,
					definition.column,
				) {
					locations.push(location);
				}
			}
		} else if let Some(flag_kind) = flag_value_kind(assignment_key.as_str()) {
			let mut found_definition = false;
			for usage in &session.index.scalar_assignments {
				if usage.value != token || flag_value_kind(usage.key.as_str()) != Some(flag_kind) {
					continue;
				}
				if is_flag_definition_key(usage.key.as_str()) {
					found_definition = true;
					if let Some(location) = definition_location(
						session,
						&usage.mod_id,
						&usage.path,
						usage.line,
						usage.column,
					) {
						locations.push(location);
					}
				}
			}
			if !found_definition {
				for usage in &session.index.scalar_assignments {
					if usage.value != token
						|| flag_value_kind(usage.key.as_str()) != Some(flag_kind)
					{
						continue;
					}
					if let Some(location) = definition_location(
						session,
						&usage.mod_id,
						&usage.path,
						usage.line,
						usage.column,
					) {
						locations.push(location);
					}
				}
			}
		}
	}

	dedup_locations(&mut locations);
	if locations.is_empty() {
		None
	} else {
		Some(locations)
	}
}

fn resolve_reference_locations(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	uri: &Url,
	text: &str,
	position: Position,
	include_declaration: bool,
) -> Option<Vec<Location>> {
	let session = snapshot.session.as_ref()?;
	let path = uri.to_file_path().ok()?;
	let (_, relative_path) = match_scan_target(targets, &path)?;
	let line = text.lines().nth(position.line as usize)?;
	let cursor = position.character as usize;
	let (token, _token_start, _) = extract_token_at_cursor(line, cursor)?;
	let assignment = assignment_context_at_cursor(line, cursor);

	if let Some(locations) = flag_reference_locations(session, assignment.as_ref(), &token) {
		return Some(locations);
	}
	if let Some(locations) =
		localisation_reference_locations(session, assignment.as_ref(), &token, include_declaration)
	{
		return Some(locations);
	}

	let target_indices = symbol_target_indices_at_cursor(
		session,
		uri,
		&relative_path,
		position,
		&token,
		assignment.as_ref(),
	)?;
	let mut locations = Vec::new();
	if include_declaration {
		for target in &target_indices {
			let Some(definition) = session.index.definitions.get(*target) else {
				continue;
			};
			if let Some(location) = definition_location(
				session,
				&definition.mod_id,
				&definition.path,
				definition.line,
				definition.column,
			) {
				locations.push(location);
			}
		}
	}
	for reference in &session.index.references {
		let resolved = resolve_symbol_reference_targets(&session.index, reference);
		if resolved
			.iter()
			.any(|target| target_indices.contains(target))
			&& let Some(location) = definition_location(
				session,
				&reference.mod_id,
				&reference.path,
				reference.line,
				reference.column,
			) {
			locations.push(location);
		}
	}
	dedup_locations(&mut locations);
	if locations.is_empty() {
		None
	} else {
		Some(locations)
	}
}

fn symbol_target_indices_at_cursor(
	session: &WorkspaceSession,
	uri: &Url,
	relative_path: &Path,
	position: Position,
	token: &str,
	assignment: Option<&(String, usize, usize, bool)>,
) -> Option<HashSet<usize>> {
	let line_number = position.line as usize + 1;
	let cursor = position.character as usize;
	if let Some((assignment_key, _key_start, _key_end, true)) = assignment
		&& assignment_key == "id"
	{
		let targets = event_definition_indices(session, token);
		return (!targets.is_empty()).then_some(targets);
	}

	let mut targets = HashSet::new();
	for (idx, definition) in session.index.definitions.iter().enumerate() {
		if !definition_matches_cursor(session, definition, uri, line_number, cursor) {
			continue;
		}
		targets.insert(idx);
	}
	if !targets.is_empty() {
		return Some(targets);
	}

	let Some((assignment_key, key_start, _key_end, false)) = assignment else {
		return None;
	};
	let current_column = key_start + 1;
	for reference in &session.index.references {
		if reference.path != relative_path
			|| reference.line != line_number
			|| reference.column != current_column
			|| reference.name != *assignment_key
		{
			continue;
		}
		for target in resolve_symbol_reference_targets(&session.index, reference) {
			targets.insert(target);
		}
		if targets.is_empty() {
			for target in fallback_symbol_targets(session, reference.kind, &reference.name) {
				targets.insert(target);
			}
		}
	}
	(!targets.is_empty()).then_some(targets)
}

fn definition_matches_cursor(
	session: &WorkspaceSession,
	definition: &SymbolDefinition,
	uri: &Url,
	line_number: usize,
	cursor: usize,
) -> bool {
	if definition.line != line_number {
		return false;
	}
	let Some(location) = definition_location(
		session,
		&definition.mod_id,
		&definition.path,
		definition.line,
		definition.column,
	) else {
		return false;
	};
	if &location.uri != uri {
		return false;
	}
	let start = definition.column.saturating_sub(1);
	let end = start.saturating_add(definition.local_name.len().max(1));
	cursor >= start && cursor <= end
}

fn event_definition_indices(session: &WorkspaceSession, name: &str) -> HashSet<usize> {
	let mut targets = HashSet::new();
	for (idx, definition) in session.index.definitions.iter().enumerate() {
		if definition.kind != FochSymbolKind::Event {
			continue;
		}
		if event_name_matches(definition.name.as_str(), name) {
			targets.insert(idx);
		}
	}
	targets
}

fn event_name_matches(definition_name: &str, reference_name: &str) -> bool {
	if definition_name == reference_name {
		return true;
	}
	has_dotted_suffix(definition_name, reference_name)
		|| has_dotted_suffix(reference_name, definition_name)
}

fn has_dotted_suffix(full_name: &str, bare_name: &str) -> bool {
	full_name.len() > bare_name.len()
		&& full_name.ends_with(bare_name)
		&& full_name
			.as_bytes()
			.get(full_name.len() - bare_name.len() - 1)
			== Some(&b'.')
}

fn fallback_symbol_targets(
	session: &WorkspaceSession,
	kind: FochSymbolKind,
	name: &str,
) -> Vec<usize> {
	session
		.index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, definition)| {
			(definition.kind == kind && (definition.local_name == name || definition.name == name))
				.then_some(idx)
		})
		.collect()
}

fn flag_reference_locations(
	session: &WorkspaceSession,
	assignment: Option<&(String, usize, usize, bool)>,
	token: &str,
) -> Option<Vec<Location>> {
	let Some((assignment_key, _, _, true)) = assignment else {
		return None;
	};
	let flag_kind = flag_value_kind(assignment_key.as_str())?;
	let mut locations = Vec::new();
	for usage in &session.index.scalar_assignments {
		if usage.value != token || flag_value_kind(usage.key.as_str()) != Some(flag_kind) {
			continue;
		}
		if let Some(location) = definition_location(
			session,
			&usage.mod_id,
			&usage.path,
			usage.line,
			usage.column,
		) {
			locations.push(location);
		}
	}
	dedup_locations(&mut locations);
	(!locations.is_empty()).then_some(locations)
}

fn localisation_reference_locations(
	session: &WorkspaceSession,
	assignment: Option<&(String, usize, usize, bool)>,
	token: &str,
	include_declaration: bool,
) -> Option<Vec<Location>> {
	let Some((assignment_key, _, _, true)) = assignment else {
		return None;
	};
	if !is_localisation_reference_key(assignment_key, token) {
		return None;
	}
	let mut locations = Vec::new();
	if include_declaration {
		for definition in &session.index.localisation_definitions {
			if definition.key != token {
				continue;
			}
			if let Some(location) = definition_location(
				session,
				&definition.mod_id,
				&definition.path,
				definition.line,
				definition.column,
			) {
				locations.push(location);
			}
		}
	}
	for usage in &session.index.scalar_assignments {
		if usage.value != token || !is_localisation_reference_key(&usage.key, token) {
			continue;
		}
		if let Some(location) = definition_location(
			session,
			&usage.mod_id,
			&usage.path,
			usage.line,
			usage.column,
		) {
			locations.push(location);
		}
	}
	for reference in &session.index.resource_references {
		if reference.value != token {
			continue;
		}
		if let Some(location) = definition_location(
			session,
			&reference.mod_id,
			&reference.path,
			reference.line,
			reference.column,
		) {
			locations.push(location);
		}
	}
	dedup_locations(&mut locations);
	(!locations.is_empty()).then_some(locations)
}

fn document_symbols(
	snapshot: &WorkspaceSnapshot,
	targets: &[ScanTarget],
	uri: &Url,
) -> Option<Vec<SymbolInformation>> {
	let session = snapshot.session.as_ref()?;
	let path = uri.to_file_path().ok()?;
	match_scan_target(targets, &path)?;
	let mut symbols = collect_symbol_information(session)
		.into_iter()
		.filter(|symbol| symbol.location.uri == *uri)
		.collect::<Vec<_>>();
	sort_symbol_information(&mut symbols);
	Some(symbols)
}

fn workspace_symbols(snapshot: &WorkspaceSnapshot, query: &str) -> Vec<SymbolInformation> {
	let Some(session) = snapshot.session.as_ref() else {
		return Vec::new();
	};
	let query = query.to_ascii_lowercase();
	let mut symbols = collect_symbol_information(session)
		.into_iter()
		.filter(|symbol| symbol_matches_query(symbol, &query))
		.collect::<Vec<_>>();
	sort_symbol_information(&mut symbols);
	symbols.truncate(500);
	symbols
}

fn code_action_context_allows_quickfix(params: &CodeActionParams) -> bool {
	params
		.context
		.only
		.as_ref()
		.is_none_or(|kinds| kinds.contains(&CodeActionKind::QUICKFIX))
}

fn localisation_stub_code_actions(
	targets: &[ScanTarget],
	params: &CodeActionParams,
) -> CodeActionResponse {
	let Ok(path) = params.text_document.uri.to_file_path() else {
		return Vec::new();
	};
	let Some((target, _relative_path)) = match_scan_target_with_role(targets, &path) else {
		return Vec::new();
	};
	if target.role != TargetRole::Mod {
		return Vec::new();
	}

	let mut actions = Vec::new();
	let mut seen = HashSet::new();
	for diagnostic in &params.context.diagnostics {
		if !is_missing_localisation_diagnostic(diagnostic) {
			continue;
		}
		let Some(key) = missing_localisation_key(diagnostic) else {
			continue;
		};
		if !seen.insert(key.clone()) {
			continue;
		}
		actions.push(CodeActionOrCommand::CodeAction(CodeAction {
			title: format!("Create localisation stub for `{key}`"),
			kind: Some(CodeActionKind::QUICKFIX),
			diagnostics: Some(vec![diagnostic.clone()]),
			edit: None,
			command: Some(Command::new(
				format!("Create localisation stub for `{key}`"),
				"foch.createLocalisationStub".to_string(),
				Some(vec![
					serde_json::json!(params.text_document.uri.as_str()),
					serde_json::json!(key),
				]),
			)),
			is_preferred: Some(true),
			disabled: None,
			data: None,
		}));
	}
	actions
}

fn is_missing_localisation_diagnostic(diagnostic: &Diagnostic) -> bool {
	matches!(
		diagnostic.code.as_ref(),
		Some(NumberOrString::String(code)) if code == "missing-localisation"
	)
}

fn missing_localisation_key(diagnostic: &Diagnostic) -> Option<String> {
	let key = diagnostic
		.message
		.strip_prefix("localisation key not found: ")?
		.trim();
	(!key.is_empty()).then(|| key.to_string())
}

fn collect_symbol_information(session: &WorkspaceSession) -> Vec<SymbolInformation> {
	let mut symbols = Vec::new();
	for definition in &session.index.definitions {
		let Some(location) = definition_location(
			session,
			&definition.mod_id,
			&definition.path,
			definition.line,
			definition.column,
		) else {
			continue;
		};
		symbols.push(make_symbol_information(
			symbol_display_name(definition),
			lsp_symbol_kind_for_foch(definition.kind),
			location,
			Some(definition.kind.as_str().to_string()),
		));
	}
	for definition in &session.index.localisation_definitions {
		let Some(location) = definition_location(
			session,
			&definition.mod_id,
			&definition.path,
			definition.line,
			definition.column,
		) else {
			continue;
		};
		symbols.push(make_symbol_information(
			definition.key.clone(),
			LspSymbolKind::STRING,
			location,
			Some("localisation".to_string()),
		));
	}
	for definition in &session.index.ui_definitions {
		let Some(location) = definition_location(
			session,
			&definition.mod_id,
			&definition.path,
			definition.line,
			definition.column,
		) else {
			continue;
		};
		symbols.push(make_symbol_information(
			definition.name.clone(),
			LspSymbolKind::OBJECT,
			location,
			Some("ui".to_string()),
		));
	}
	symbols
}

#[allow(deprecated)]
fn make_symbol_information(
	name: String,
	kind: LspSymbolKind,
	location: Location,
	container_name: Option<String>,
) -> SymbolInformation {
	SymbolInformation {
		name,
		kind,
		tags: None,
		deprecated: None,
		location,
		container_name,
	}
}

fn lsp_symbol_kind_for_foch(kind: FochSymbolKind) -> LspSymbolKind {
	match kind {
		FochSymbolKind::ScriptedEffect | FochSymbolKind::ScriptedTrigger => LspSymbolKind::FUNCTION,
		FochSymbolKind::Event => LspSymbolKind::EVENT,
		FochSymbolKind::Decision | FochSymbolKind::DiplomaticAction => LspSymbolKind::METHOD,
		FochSymbolKind::TriggeredModifier => LspSymbolKind::VARIABLE,
	}
}

fn symbol_display_name(definition: &SymbolDefinition) -> String {
	if definition.kind == FochSymbolKind::Event {
		definition.name.clone()
	} else {
		definition.local_name.clone()
	}
}

fn symbol_matches_query(symbol: &SymbolInformation, query: &str) -> bool {
	if query.is_empty() {
		return true;
	}
	symbol.name.to_ascii_lowercase().contains(query)
		|| symbol
			.container_name
			.as_deref()
			.unwrap_or_default()
			.to_ascii_lowercase()
			.contains(query)
}

fn sort_symbol_information(symbols: &mut [SymbolInformation]) {
	symbols.sort_by(|left, right| {
		left.name
			.cmp(&right.name)
			.then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
			.then_with(|| {
				range_start(&left.location.range).cmp(&range_start(&right.location.range))
			})
	});
}

fn definition_location(
	session: &WorkspaceSession,
	mod_id: &str,
	relative_path: &Path,
	line: usize,
	column: usize,
) -> Option<Location> {
	let absolute_path = session.resolve_path(&path_lookup_key(mod_id, relative_path))?;
	let uri = Url::from_file_path(absolute_path).ok()?;
	Some(Location {
		uri,
		range: lsp_range(line, column),
	})
}

fn dedup_locations(locations: &mut Vec<Location>) {
	let mut seen = HashSet::new();
	locations.retain(|location| {
		let key = format!(
			"{}:{}:{}",
			location.uri, location.range.start.line, location.range.start.character
		);
		seen.insert(key)
	});
}

fn match_scan_target(targets: &[ScanTarget], path: &Path) -> Option<(PathBuf, PathBuf)> {
	match_scan_target_with_role(targets, path).map(|(target, relative)| (target.path, relative))
}

fn match_scan_target_with_role(
	targets: &[ScanTarget],
	path: &Path,
) -> Option<(ScanTarget, PathBuf)> {
	let mut best: Option<(usize, PathBuf, PathBuf)> = None;
	for target in targets {
		let Ok(relative) = path.strip_prefix(&target.path) else {
			continue;
		};
		let len = target.path.components().count();
		match &best {
			Some((best_len, ..)) if *best_len >= len => {}
			_ => {
				best = Some((len, target.path.clone(), relative.to_path_buf()));
			}
		}
	}
	best.and_then(|(_, root, relative)| {
		targets
			.iter()
			.find(|target| target.path == root)
			.cloned()
			.map(|target| (target, relative))
	})
}

fn assignment_context_at_cursor(line: &str, cursor: usize) -> Option<(String, usize, usize, bool)> {
	let (_, token_start, token_end) = extract_token_at_cursor(line, cursor)?;
	let chars: Vec<char> = line.chars().collect();

	let mut after = token_end;
	while after < chars.len() && chars[after].is_whitespace() {
		after += 1;
	}
	if after < chars.len() && chars[after] == '=' {
		let key = chars[token_start..token_end].iter().collect();
		return Some((key, token_start, token_end, false));
	}

	let eq_idx = chars[..token_start].iter().rposition(|ch| *ch == '=')?;
	let mut end = eq_idx;
	while end > 0 && chars[end - 1].is_whitespace() {
		end -= 1;
	}
	let mut start = end;
	while start > 0 && is_identifier_char(chars[start - 1]) {
		start -= 1;
	}
	if start == end {
		return None;
	}
	Some((chars[start..end].iter().collect(), start, end, true))
}

fn extract_token_at_cursor(line: &str, cursor: usize) -> Option<(String, usize, usize)> {
	let chars: Vec<char> = line.chars().collect();
	if chars.is_empty() {
		return None;
	}
	let mut idx = cursor.min(chars.len().saturating_sub(1));
	if !is_identifier_char(chars[idx]) {
		if idx == 0 || !is_identifier_char(chars[idx - 1]) {
			return None;
		}
		idx -= 1;
	}
	let mut start = idx;
	while start > 0 && is_identifier_char(chars[start - 1]) {
		start -= 1;
	}
	let mut end = idx + 1;
	while end < chars.len() && is_identifier_char(chars[end]) {
		end += 1;
	}
	Some((chars[start..end].iter().collect(), start, end))
}

#[cfg(test)]
fn assignment_key_on_line(line: &str) -> Option<(String, usize, usize, usize)> {
	let chars: Vec<char> = line.chars().collect();
	let eq_idx = chars.iter().position(|ch| *ch == '=')?;
	let mut end = eq_idx;
	while end > 0 && chars[end - 1].is_whitespace() {
		end -= 1;
	}
	let mut start = end;
	while start > 0 && is_identifier_char(chars[start - 1]) {
		start -= 1;
	}
	if start == end {
		return None;
	}
	Some((chars[start..end].iter().collect(), start, end, eq_idx))
}

fn is_flag_definition_key(key: &str) -> bool {
	matches!(
		key,
		"set_global_flag"
			| "set_country_flag"
			| "set_province_flag"
			| "set_permanent_province_flag"
			| "set_ruler_flag"
			| "set_heir_flag"
			| "set_consort_flag"
	)
}

fn is_localisation_reference_key(key: &str, value: &str) -> bool {
	match key {
		"tooltip" | "custom_tooltip" | "localisation_key" | "title" | "desc" => true,
		"name" => looks_like_localisation_name(value),
		_ => false,
	}
}

fn looks_like_localisation_name(value: &str) -> bool {
	value.contains('.')
		|| value.chars().any(|ch| ch.is_ascii_uppercase())
		|| value.ends_with("_title")
		|| value.ends_with("_desc")
		|| value.ends_with("_tt")
		|| value.ends_with("_tooltip")
}

fn resolve_scan_targets(params: &InitializeParams) -> Vec<ScanTarget> {
	if std::env::var_os("FOCH_LSP_PROJECT_MANIFEST").is_some() {
		return scan_targets_from_project_manifest_env().unwrap_or_default();
	}

	match scan_targets_from_env() {
		Ok(targets) if !targets.is_empty() => targets,
		Ok(_) | Err(_) => scan_targets_from_workspace(params),
	}
}

fn scan_targets_from_project_manifest_env() -> std::result::Result<Vec<ScanTarget>, String> {
	let raw = match std::env::var("FOCH_LSP_PROJECT_MANIFEST") {
		Ok(value) => value,
		Err(std::env::VarError::NotPresent) => return Ok(Vec::new()),
		Err(err) => return Err(format!("read FOCH_LSP_PROJECT_MANIFEST failed: {err}")),
	};
	let manifest_path = PathBuf::from(raw);
	let config = load_or_init_config()
		.map(|(config, _path)| config)
		.unwrap_or_else(|_| Config::default());
	scan_targets_from_project_manifest_path(manifest_path, config)
}

fn scan_targets_from_project_manifest_path(
	manifest_path: PathBuf,
	config: Config,
) -> std::result::Result<Vec<ScanTarget>, String> {
	let request = InputRequest::new(InputSource::Manifest(manifest_path), config);
	let targets = resolve_input_targets(&request, true)
		.map_err(|err| format!("resolve FOCH_LSP_PROJECT_MANIFEST failed: {err}"))?;
	Ok(dedup_scan_targets(
		targets
			.into_iter()
			.map(|target| ScanTarget {
				path: target.path,
				role: match target.role {
					InputTargetRole::Game => TargetRole::Game,
					InputTargetRole::Mod => TargetRole::Mod,
				},
			})
			.collect(),
	))
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
	kind: &FochSymbolKind,
	local_name: &str,
	full_name: &str,
) -> (String, CompletionItemKind, String) {
	match kind {
		FochSymbolKind::ScriptedEffect => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace scripted effect".to_string(),
		),
		FochSymbolKind::ScriptedTrigger => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace scripted trigger".to_string(),
		),
		FochSymbolKind::Event => (
			full_name.to_string(),
			CompletionItemKind::EVENT,
			"workspace event id".to_string(),
		),
		FochSymbolKind::Decision => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace decision".to_string(),
		),
		FochSymbolKind::DiplomaticAction => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace diplomatic action".to_string(),
		),
		FochSymbolKind::TriggeredModifier => (
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
		"common/scripted_triggers",
		"common/diplomatic_actions",
		"common/triggered_modifiers",
		"common/defines",
		"common/country_tags",
		"common/cultures",
		"customizable_localization",
		"interface",
		"common/interface",
		"gfx",
	];
	let file_targets = ["common/graphicalculturetype.txt"];

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
			let Some(ext) = path.extension() else {
				continue;
			};
			let ext = ext.to_string_lossy();
			if matches!(ext.to_ascii_lowercase().as_str(), "txt" | "lua") {
				files.push(path.to_path_buf());
			}
		}
	}
	for target in file_targets {
		let path = root.join(target);
		if path.is_file() {
			files.push(path);
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
		CandidateSource, CompletionCandidate, CompletionContext, ScanTarget, TargetRole,
		assignment_key_on_line, build_workspace_snapshot, build_workspace_snapshot_with_schema,
		detect_completion_context, document_symbols, extract_completion_prefix,
		localisation_stub_code_actions, parse_scan_targets_json, resolve_definition_locations,
		resolve_reference_locations, scan_targets_from_project_manifest_path,
		schema_completion_candidate, schema_diagnostic, schema_hover_view,
		select_completion_candidates, workspace_symbols,
	};
	use foch::game::eu4::editor::schema::{
		EditorPosition, EditorRange, EditorSchema, SchemaCompletion, SchemaCompletionKind,
		SchemaDiagnostic as EditorSchemaDiagnostic, SchemaHover,
	};
	use foch::input::Config;
	use foch::model::{Severity, test_support};
	use std::fs;
	use std::path::PathBuf;
	use tempfile::TempDir;
	use tower_lsp::lsp_types::CompletionItemKind;
	use tower_lsp::lsp_types::{
		CodeActionContext, CodeActionOrCommand, CodeActionParams, Diagnostic, DiagnosticSeverity,
		HoverContents, NumberOrString, PartialResultParams, Position, Range,
		TextDocumentIdentifier, Url, WorkDoneProgressParams,
	};

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
	fn schema_hover_mapping_preserves_markdown_and_range() {
		let hover = schema_hover_view(SchemaHover {
			markdown: "**country_event**".to_string(),
			range: EditorRange {
				start: EditorPosition {
					line: 3,
					character: 4,
				},
				end: EditorPosition {
					line: 3,
					character: 17,
				},
			},
		});
		let HoverContents::Markup(markup) = hover.contents else {
			panic!("expected markdown hover contents");
		};
		assert_eq!(markup.value, "**country_event**");
		assert_eq!(
			hover.range,
			Some(Range {
				start: Position {
					line: 3,
					character: 4,
				},
				end: Position {
					line: 3,
					character: 17,
				},
			})
		);
	}

	#[test]
	fn schema_completion_mapping_preserves_editor_semantics() {
		let cases = [
			(SchemaCompletionKind::Field, CompletionItemKind::FIELD),
			(SchemaCompletionKind::Function, CompletionItemKind::FUNCTION),
			(
				SchemaCompletionKind::EnumMember,
				CompletionItemKind::ENUM_MEMBER,
			),
			(SchemaCompletionKind::Value, CompletionItemKind::VALUE),
			(
				SchemaCompletionKind::Reference,
				CompletionItemKind::REFERENCE,
			),
		];
		for (schema_kind, lsp_kind) in cases {
			let candidate = schema_completion_candidate(SchemaCompletion {
				label: "sample".to_string(),
				insert_text: "sample = {}".to_string(),
				kind: schema_kind,
				detail: "schema detail".to_string(),
			});
			assert_eq!(candidate.label, "sample");
			assert_eq!(candidate.insert_text, "sample = {}");
			assert_eq!(candidate.kind, lsp_kind);
			assert_eq!(candidate.detail, "schema detail");
			assert_eq!(candidate.source, CandidateSource::Schema);
		}
	}

	#[test]
	fn schema_diagnostic_mapping_preserves_protocol_fields() {
		let diagnostic = schema_diagnostic(EditorSchemaDiagnostic {
			range: EditorRange {
				start: EditorPosition {
					line: 7,
					character: 2,
				},
				end: EditorPosition {
					line: 7,
					character: 8,
				},
			},
			severity: Some(Severity::Warning),
			code: Some("V007".to_string()),
			source: Some("cwt".to_string()),
			message: "scope mismatch".to_string(),
		});
		assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
		assert_eq!(
			diagnostic.code,
			Some(NumberOrString::String("V007".to_string()))
		);
		assert_eq!(diagnostic.source.as_deref(), Some("cwt"));
		assert_eq!(diagnostic.message, "scope mismatch");
		assert_eq!(diagnostic.range.start.line, 7);
		assert_eq!(diagnostic.range.end.character, 8);
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
		let tmp = TempDir::new().expect("temp dir");
		let tmp_path = tmp.path().to_string_lossy().replace('\\', "/");
		let json = format!(
			r#"[{{"path":"{tmp_path}","role":"game"}},{{"path":"/nonexistent/nope","role":"mod"}}]"#
		);
		let targets = parse_scan_targets_json(&json).expect("parse targets json");
		assert!(!targets.is_empty());
		assert_eq!(targets[0].role, TargetRole::Game);
	}

	#[test]
	fn project_manifest_targets_resolve_for_lsp() {
		let tmp = TempDir::new().expect("temp dir");
		let game_root = tmp.path().join("game-root");
		let mod_root = tmp.path().join("local-mod");
		fs::create_dir_all(&game_root).expect("create game root");
		fs::create_dir_all(mod_root.join("common").join("scripted_effects"))
			.expect("create mod root");
		fs::write(
			mod_root.join("descriptor.mod"),
			format!(
				"name=\"local-mod\"\npath=\"{}\"\n",
				mod_root.to_string_lossy().replace('\\', "/")
			),
		)
		.expect("write descriptor");
		let manifest = tmp.path().join("foch.toml");
		fs::write(
			&manifest,
			r#"
[project]
game = "eu4"
game_path = "game-root"

[[project.mods]]
id = "local_mod"
path = "local-mod"
"#,
		)
		.expect("write manifest");

		let targets = scan_targets_from_project_manifest_path(manifest, Config::default())
			.expect("resolve project manifest");
		assert_eq!(targets.len(), 2);
		assert!(targets.iter().any(|target| target.role == TargetRole::Game));
		assert!(targets.iter().any(|target| target.role == TargetRole::Mod));
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

	#[test]
	fn assignment_key_extracts_span() {
		let (key, start, end, eq) =
			assignment_key_on_line("\thas_country_flag = CTRLMA_open_config_menu_flag")
				.expect("assignment key");
		assert_eq!(key, "has_country_flag");
		assert_eq!(start, 1);
		assert_eq!(end, 17);
		assert_eq!(eq, 18);
	}

	fn init_scopes() {
		test_support::install_defaults();
	}

	#[test]
	fn definition_resolves_flag_value_to_setter() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("decisions")).expect("create decisions");
		fs::create_dir_all(root.join("events")).expect("create events");
		fs::write(
			root.join("decisions").join("a.txt"),
			"test_decision = { effect = { set_country_flag = CTRLMA_open_config_menu_flag } }\n",
		)
		.expect("write decision");
		fs::write(
			root.join("events").join("b.txt"),
			"namespace = test\ncountry_event = { id = test.1 trigger = { has_country_flag = CTRLMA_open_config_menu_flag } }\n",
		)
		.expect("write event");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let target_path = root.join("events").join("b.txt");
		let text = fs::read_to_string(&target_path).expect("read event");
		let line = text.lines().nth(1).expect("event line");
		let uri = Url::from_file_path(&target_path).expect("uri");
		let column = line
			.find("CTRLMA_open_config_menu_flag")
			.expect("flag token") as u32;

		let locations = resolve_definition_locations(
			&snapshot,
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&uri,
			&text,
			Position {
				line: 1,
				character: column,
			},
		)
		.expect("definition locations");

		assert_eq!(locations.len(), 1);
		assert_eq!(
			locations[0].uri,
			Url::from_file_path(root.join("decisions").join("a.txt")).expect("decision uri")
		);
	}

	#[test]
	fn definition_resolves_scripted_effect_call_to_definition() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(root.join("decisions")).expect("create decisions");
		fs::write(
			root.join("common").join("scripted_effects").join("a.txt"),
			"my_effect = { set_country_flag = TEST_FLAG }\n",
		)
		.expect("write effect");
		fs::write(
			root.join("decisions").join("b.txt"),
			"test_decision = { effect = { my_effect = { } } }\n",
		)
		.expect("write decision");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let target_path = root.join("decisions").join("b.txt");
		let text = fs::read_to_string(&target_path).expect("read decision");
		let uri = Url::from_file_path(&target_path).expect("uri");
		let column = text.find("my_effect").expect("effect token") as u32;

		let locations = resolve_definition_locations(
			&snapshot,
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&uri,
			&text,
			Position {
				line: 0,
				character: column,
			},
		)
		.expect("definition locations");

		assert_eq!(locations.len(), 1);
		assert_eq!(
			locations[0].uri,
			Url::from_file_path(root.join("common").join("scripted_effects").join("a.txt"))
				.expect("effect uri")
		);
	}

	#[test]
	fn definition_resolves_scripted_trigger_call_to_definition() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("common").join("scripted_triggers"))
			.expect("create scripted triggers");
		fs::create_dir_all(root.join("events")).expect("create events");
		fs::write(
			root.join("common").join("scripted_triggers").join("a.txt"),
			"my_trigger = { has_country_flag = TEST_FLAG }\n",
		)
		.expect("write trigger");
		fs::write(
			root.join("events").join("b.txt"),
			"namespace = test\ncountry_event = { id = test.1 trigger = { my_trigger = yes } }\n",
		)
		.expect("write event");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let target_path = root.join("events").join("b.txt");
		let text = fs::read_to_string(&target_path).expect("read event");
		let line = text.lines().nth(1).expect("event line");
		let uri = Url::from_file_path(&target_path).expect("uri");
		let column = line.find("my_trigger").expect("trigger token") as u32;

		let locations = resolve_definition_locations(
			&snapshot,
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&uri,
			&text,
			Position {
				line: 1,
				character: column,
			},
		)
		.expect("definition locations");

		assert_eq!(locations.len(), 1);
		assert_eq!(
			locations[0].uri,
			Url::from_file_path(root.join("common").join("scripted_triggers").join("a.txt"))
				.expect("trigger uri")
		);
	}

	#[test]
	fn references_resolve_scripted_effect_callsites() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(root.join("decisions")).expect("create decisions");
		fs::write(
			root.join("common").join("scripted_effects").join("a.txt"),
			"my_effect = { set_country_flag = TEST_FLAG }\n",
		)
		.expect("write effect");
		fs::write(
			root.join("decisions").join("b.txt"),
			"test_decision = { effect = { my_effect = { } } }\n",
		)
		.expect("write decision");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let target_path = root.join("decisions").join("b.txt");
		let text = fs::read_to_string(&target_path).expect("read decision");
		let uri = Url::from_file_path(&target_path).expect("uri");
		let column = text.find("my_effect").expect("effect token") as u32;

		let locations = resolve_reference_locations(
			&snapshot,
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&uri,
			&text,
			Position {
				line: 0,
				character: column,
			},
			true,
		)
		.expect("reference locations");

		assert_eq!(locations.len(), 2);
		assert!(locations.iter().any(|location| {
			location.uri
				== Url::from_file_path(root.join("common").join("scripted_effects").join("a.txt"))
					.expect("effect uri")
		}));
		assert!(locations.iter().any(|location| location.uri == uri));
	}

	#[test]
	fn document_symbols_include_current_file_definitions() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::create_dir_all(root.join("decisions")).expect("create decisions");
		let effect_path = root.join("common").join("scripted_effects").join("a.txt");
		fs::write(
			&effect_path,
			"my_effect = { set_country_flag = TEST_FLAG }\n",
		)
		.expect("write effect");
		fs::write(
			root.join("decisions").join("b.txt"),
			"test_decision = { effect = { my_effect = { } } }\n",
		)
		.expect("write decision");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let uri = Url::from_file_path(&effect_path).expect("uri");
		let symbols = document_symbols(
			&snapshot,
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&uri,
		)
		.expect("document symbols");

		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "my_effect");
	}

	#[test]
	fn workspace_symbols_filter_by_query() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("common").join("scripted_effects"))
			.expect("create scripted effects");
		fs::write(
			root.join("common").join("scripted_effects").join("a.txt"),
			"my_effect = { set_country_flag = TEST_FLAG }\nother_effect = { }\n",
		)
		.expect("write effect");

		let snapshot = build_workspace_snapshot(&[ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		}]);
		let symbols = workspace_symbols(&snapshot, "my_");

		assert_eq!(symbols.len(), 1);
		assert_eq!(symbols[0].name, "my_effect");
	}

	#[test]
	fn workspace_snapshot_indexes_localisation_files_for_navigation() {
		init_scopes();
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("events")).expect("create events");
		fs::create_dir_all(root.join("localisation").join("english")).expect("create localisation");
		let event_path = root.join("events").join("a.txt");
		fs::write(
			&event_path,
			"namespace = test\ncountry_event = { id = test.1 title = TEST_EVENT_TITLE }\n",
		)
		.expect("write event");
		let localisation_path = root
			.join("localisation")
			.join("english")
			.join("test_l_english.yml");
		fs::write(
			&localisation_path,
			"l_english:\n TEST_EVENT_TITLE:0 \"Title\"\n",
		)
		.expect("write localisation");

		let target = ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		};
		let snapshot = build_workspace_snapshot(std::slice::from_ref(&target));
		assert!(snapshot.session.as_ref().is_some_and(|session| {
			session
				.index
				.localisation_definitions
				.iter()
				.any(|definition| definition.key == "TEST_EVENT_TITLE")
		}));

		let text = fs::read_to_string(&event_path).expect("read event");
		let uri = Url::from_file_path(&event_path).expect("event uri");
		let line = text.lines().nth(1).expect("event line");
		let column = line.find("TEST_EVENT_TITLE").expect("title token") as u32;
		let locations = resolve_definition_locations(
			&snapshot,
			&[target],
			&uri,
			&text,
			Position {
				line: 1,
				character: column,
			},
		)
		.expect("localisation definition location");

		assert_eq!(locations.len(), 1);
		assert_eq!(
			locations[0].uri,
			Url::from_file_path(localisation_path).expect("localisation uri")
		);
	}

	#[test]
	fn code_action_creates_missing_localisation_stub_command() {
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		let source_uri = Url::from_file_path(root.join("events").join("a.txt")).expect("uri");
		let diagnostic = Diagnostic {
			range: Range {
				start: Position {
					line: 0,
					character: 0,
				},
				end: Position {
					line: 0,
					character: 1,
				},
			},
			code: Some(NumberOrString::String("missing-localisation".to_string())),
			message: "localisation key not found: TEST_EVENT_TITLE".to_string(),
			..Diagnostic::default()
		};
		let params = CodeActionParams {
			text_document: TextDocumentIdentifier {
				uri: source_uri.clone(),
			},
			range: diagnostic.range,
			context: CodeActionContext {
				diagnostics: vec![diagnostic],
				only: None,
				trigger_kind: None,
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: PartialResultParams::default(),
		};

		let actions = localisation_stub_code_actions(
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Mod,
			}],
			&params,
		);

		assert_eq!(actions.len(), 1);
		let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
			panic!("expected code action");
		};
		assert_eq!(
			action.title,
			"Create localisation stub for `TEST_EVENT_TITLE`"
		);
		let command = action.command.as_ref().expect("quickfix command");
		assert_eq!(command.command, "foch.createLocalisationStub");
		assert_eq!(
			command.arguments.as_ref().expect("arguments")[0],
			serde_json::json!(source_uri.as_str())
		);
		assert_eq!(
			command.arguments.as_ref().expect("arguments")[1],
			serde_json::json!("TEST_EVENT_TITLE")
		);
	}

	#[test]
	fn code_action_skips_missing_localisation_for_game_targets() {
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		let source_uri = Url::from_file_path(root.join("events").join("a.txt")).expect("uri");
		let diagnostic = Diagnostic {
			range: Range {
				start: Position {
					line: 0,
					character: 0,
				},
				end: Position {
					line: 0,
					character: 1,
				},
			},
			code: Some(NumberOrString::String("missing-localisation".to_string())),
			message: "localisation key not found: TEST_EVENT_TITLE".to_string(),
			..Diagnostic::default()
		};
		let params = CodeActionParams {
			text_document: TextDocumentIdentifier { uri: source_uri },
			range: diagnostic.range,
			context: CodeActionContext {
				diagnostics: vec![diagnostic],
				only: None,
				trigger_kind: None,
			},
			work_done_progress_params: WorkDoneProgressParams::default(),
			partial_result_params: PartialResultParams::default(),
		};

		let actions = localisation_stub_code_actions(
			&[ScanTarget {
				path: root.to_path_buf(),
				role: TargetRole::Game,
			}],
			&params,
		);

		assert!(actions.is_empty());
	}

	fn lsp_fixture_dir() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests")
			.join("fixtures")
			.join("lsp")
	}

	fn load_lsp_schema() -> EditorSchema {
		let cache = TempDir::new().expect("create test CWT rule cache");
		EditorSchema::load_from_directory_with_cache(
			&lsp_fixture_dir().join("schema"),
			Some(cache.path()),
		)
		.expect("load LSP editor schema")
	}

	fn load_inline_lsp_schema(schema: &str) -> EditorSchema {
		let schema_dir = TempDir::new().expect("create inline CWT schema dir");
		fs::write(schema_dir.path().join("inline.cwt"), schema).expect("write inline CWT schema");
		let cache = TempDir::new().expect("create inline CWT rule cache");
		EditorSchema::load_from_directory_with_cache(schema_dir.path(), Some(cache.path()))
			.expect("load inline LSP editor schema")
	}

	#[test]
	fn workspace_snapshot_includes_schema_diagnostics() {
		let engine = load_lsp_schema();
		let root = lsp_fixture_dir();
		let snapshot = build_workspace_snapshot_with_schema(
			&[ScanTarget {
				path: root.clone(),
				role: TargetRole::Mod,
			}],
			Some(engine),
		);
		let key = root.join("events").join("diagnostics.txt");
		let diagnostics = snapshot
			.diagnostics_by_path
			.get(&key.to_string_lossy().replace('\\', "/"))
			.expect("workspace diagnostics for fixture");
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V001".to_string()))
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V002".to_string()))
		}));
	}

	#[test]
	fn workspace_snapshot_uses_cwt_localisation_metadata_for_missing_keys() {
		let schema = r#"
		types = {
			type[event] = {
				path = "game/events"
				name_field = "id"
				localisation = {
					## required
					custom = "$_custom"
				}
			}
		}

		event = {
			id = scalar
		}
		"#;
		let engine = load_inline_lsp_schema(schema);
		let tmp = TempDir::new().expect("temp dir");
		let root = tmp.path();
		fs::create_dir_all(root.join("events")).expect("create events");
		let event_path = root.join("events").join("a.txt");
		fs::write(&event_path, "country_event = { id = test.1 }\n").expect("write event");

		let target = ScanTarget {
			path: root.to_path_buf(),
			role: TargetRole::Mod,
		};
		let snapshot = build_workspace_snapshot_with_schema(
			std::slice::from_ref(&target),
			Some(engine.clone()),
		);
		let diagnostics = snapshot
			.diagnostics_by_path
			.get(&event_path.to_string_lossy().replace('\\', "/"))
			.expect("event diagnostics");
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("missing-localisation".to_string()))
				&& diagnostic
					.message
					.contains("localisation key not found: test.1_custom")
		}));

		fs::create_dir_all(root.join("localisation").join("english")).expect("create localisation");
		fs::write(
			root.join("localisation")
				.join("english")
				.join("test_l_english.yml"),
			"l_english:\n test.1_custom:0 \"Custom\"\n",
		)
		.expect("write localisation");
		let snapshot = build_workspace_snapshot_with_schema(&[target], Some(engine));
		let diagnostics = snapshot
			.diagnostics_by_path
			.get(&event_path.to_string_lossy().replace('\\', "/"))
			.cloned()
			.unwrap_or_default();
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("missing-localisation".to_string()))
				&& diagnostic.message.contains("test.1_custom")
		}));
	}
}
