use foch_core::model::{
	AnalysisMode, Finding, SemanticIndex, Severity, SymbolDefinition, SymbolKind as FochSymbolKind,
};
use foch_cwt::{
	CompiledAlias, CompiledAliasCategory, CompiledBindFieldMatch, CompiledFieldAttributes,
	CompiledRuleField, CompiledRuleValue, CompiledSeverity, RuleContext, RuleEngine,
	RuleEngineLoad, RuleEngineLoadStatus, SchemaBinding, SchemaSource,
	default_compiled_rule_cache_dir, load_rule_engine_from_dir,
};
use foch_engine::WorkspaceSession;
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use foch_language::analyzer::eu4_builtin::{
	alias_keywords, builtin_effect_names, builtin_trigger_names, contextual_keywords,
	reserved_keywords,
};
use foch_language::analyzer::parser::{
	AstStatement, AstValue, ScalarValue, SpanRange, parse_clausewitz_content,
};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, build_semantic_index, parse_script_file, resolve_symbol_reference_targets,
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
	rule_engine: Arc<RwLock<Option<Arc<RuleEngine>>>>,
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
			rule_engine: Arc::new(RwLock::new(None)),
		}
	}

	async fn refresh_workspace_snapshot(&self) {
		let targets = { self.state.read().await.targets.clone() };
		let rule_engine = self.rule_engine.read().await.clone();
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || {
			build_workspace_snapshot_with_schema(&targets, rule_engine)
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
		let rule_engine = self.rule_engine.read().await.clone();
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
		if let (Some(engine), Some(relative_path)) = (rule_engine.as_ref(), relative_path.as_ref())
		{
			diagnostics.extend(schema_diagnostics_for_text(
				engine.as_ref(),
				relative_path,
				text,
			));
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
		let rule_engine = match find_vendored_schema_dir() {
			Some(schema_dir) => {
				let started = Instant::now();
				let load_result =
					tokio::task::spawn_blocking(move || load_rule_engine(&schema_dir)).await;
				let elapsed = started.elapsed();
				match load_result {
					Ok(Ok(loaded)) => {
						let engine = loaded.engine.clone();
						self.client
							.log_message(
								MessageType::INFO,
								format!(
									"foch lsp loaded compiled CWT rule pack: {} roots, {} aliases, {}, source {}, hash {:.1} ms, cache {}, compile {}, total {:.1} ms, task {:.1} ms",
									engine.root_count(),
									engine.alias_count(),
									rule_engine_load_status_label(loaded.status),
									short_source_id(&loaded.source_id.to_hex()),
									duration_ms(loaded.timings.source_hash),
									optional_duration_ms(loaded.timings.cache_read),
									optional_duration_ms(loaded.timings.source_compile),
									duration_ms(loaded.timings.total),
									elapsed.as_secs_f64() * 1000.0
								),
							)
							.await;
						Some(engine)
					}
					Ok(Err(err)) => {
						self.client
							.log_message(
								MessageType::ERROR,
								format!("foch lsp failed to load CWT schema graph: {err}"),
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
				}
			}
			None => {
				self.client
					.log_message(
						MessageType::WARNING,
						"foch lsp missing vendored CWT schema directory; schema-aware features disabled",
					)
					.await;
				None
			}
		};

		{
			let mut state = self.state.write().await;
			state.targets = targets;
		}
		*self.rule_engine.write().await = rule_engine;

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
		let text = {
			let state = self.state.read().await;
			state.docs.get(uri).cloned()
		};
		let Some(text) = text else {
			return Ok(None);
		};
		let Some(engine) = self.rule_engine.read().await.clone() else {
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
			engine.as_ref(),
			&relative_path,
			&text,
			position,
		))
	}

	async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
		let rule_engine = self.rule_engine.read().await.clone();
		let state = self.state.read().await;
		let uri = &params.text_document_position.text_document.uri;
		let position = params.text_document_position.position;
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let prefix = extract_completion_prefix(text, position);
		let context = detect_completion_context(text, position);
		let prefix_lower = prefix.to_ascii_lowercase();

		let mut candidates = if let Some(engine) = rule_engine.as_ref()
			&& let Ok(path) = uri.to_file_path()
			&& let Some((_, relative_path)) = match_scan_target(&state.targets, &path)
			&& let Some(candidates) = schema_completion_candidates(
				engine.as_ref(),
				&relative_path,
				text,
				position,
				&prefix_lower,
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

fn find_vendored_schema_dir() -> Option<PathBuf> {
	if let Ok(current_dir) = std::env::current_dir()
		&& let Some(found) = find_vendored_schema_dir_from(&current_dir)
	{
		return Some(found);
	}

	let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	find_vendored_schema_dir_from(&manifest_root)
}

fn find_vendored_schema_dir_from(start: &Path) -> Option<PathBuf> {
	let mut current = Some(start);
	while let Some(dir) = current {
		let candidate = dir.join("vendor").join("cwtools-eu4-config");
		if candidate.is_dir() {
			return Some(candidate);
		}
		current = dir.parent();
	}
	None
}

fn load_rule_engine(schema_dir: &Path) -> std::result::Result<RuleEngineLoad, String> {
	let cache_dir = default_compiled_rule_cache_dir();
	load_rule_engine_with_cache_dir(schema_dir, Some(&cache_dir))
}

fn load_rule_engine_with_cache_dir(
	schema_dir: &Path,
	cache_dir: Option<&Path>,
) -> std::result::Result<RuleEngineLoad, String> {
	load_rule_engine_from_dir(
		schema_dir,
		SchemaSource::UserProvided {
			path: schema_dir.to_path_buf(),
		},
		cache_dir,
	)
	.map_err(|err| err.to_string())
}

fn rule_engine_load_status_label(status: RuleEngineLoadStatus) -> &'static str {
	match status {
		RuleEngineLoadStatus::CacheHit => "cache hit",
		RuleEngineLoadStatus::CompiledFromSource => "compiled from source",
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyPathTarget {
	parent_path: Vec<String>,
	key: String,
	range: Range,
}

fn schema_hover(
	engine: &RuleEngine,
	file_path: &Path,
	text: &str,
	position: Position,
) -> Option<Hover> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	let target = find_hover_target(&parsed.ast.statements, position, &[])?;
	let mut ast_path = target
		.parent_path
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>();
	ast_path.push(target.key.as_str());
	let SchemaBinding::Bound { .. } = engine.bind_chain(file_path, &ast_path) else {
		return None;
	};
	let parent_path = target
		.parent_path
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>();
	let parent_context = engine.bind_context(file_path, &parent_path)?;
	let field_match = engine.bind_field_match(parent_context, &target.key)?;
	Some(Hover {
		contents: HoverContents::Markup(MarkupContent {
			kind: MarkupKind::Markdown,
			value: render_schema_hover_markdown(engine, &target.key, &field_match),
		}),
		range: Some(target.range),
	})
}

fn find_hover_target(
	statements: &[AstStatement],
	position: Position,
	parent_path: &[String],
) -> Option<KeyPathTarget> {
	for statement in statements {
		let AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} = statement
		else {
			continue;
		};
		if span_contains_position(key_span, position) {
			return Some(KeyPathTarget {
				parent_path: parent_path.to_vec(),
				key: key.clone(),
				range: lsp_range_from_span(key_span),
			});
		}
		if let AstValue::Block { items, span } = value
			&& span_contains_position(span, position)
		{
			let mut child_path = parent_path.to_vec();
			child_path.push(key.clone());
			if let Some(target) = find_hover_target(items, position, &child_path) {
				return Some(target);
			}
		}
	}
	None
}

fn render_schema_hover_markdown(
	engine: &RuleEngine,
	key: &str,
	field_match: &CompiledBindFieldMatch<'_>,
) -> String {
	let value = schema_match_value(field_match);
	let mut sections = vec![
		format!("**{key}**"),
		format!("Type: `{}`", rule_value_kind(value)),
	];
	if let Some(values) = schema_allowed_values(engine, value) {
		sections.push(format!(
			"Value set: `{}` `{}`",
			values.kind.label(),
			values.name
		));
		sections.push(format!(
			"Allowed values: {}",
			format_allowed_values(values.values)
		));
	}
	if let Some(description) = schema_hover_description(field_match) {
		sections.push(description.to_string());
	}
	if let Some(scope_context) = schema_hover_scope_context(field_match) {
		sections.push(format!("Scope context: {scope_context}"));
	}
	if let Some(cardinality) = schema_hover_cardinality(field_match) {
		sections.push(format!("Cardinality: `{cardinality}`"));
	}
	sections.join("\n\n")
}

fn rule_value_kind(value: &CompiledRuleValue) -> &'static str {
	match value {
		CompiledRuleValue::Scalar(_) => "Scalar",
		CompiledRuleValue::Block(_) => "Block",
		CompiledRuleValue::Marker(marker) if schema_allowed_value_marker(marker).is_some() => {
			"Scalar"
		}
		CompiledRuleValue::Marker(_) => "Marker",
	}
}

fn schema_hover_description<'a>(field_match: &'a CompiledBindFieldMatch<'a>) -> Option<&'a str> {
	field_match
		.field()
		.attributes
		.description
		.as_deref()
		.or_else(|| {
			field_match
				.alias()
				.and_then(|alias| alias.attributes.description.as_deref())
		})
}

fn schema_hover_scope_context(field_match: &CompiledBindFieldMatch<'_>) -> Option<String> {
	let field_attributes = &field_match.field().attributes;
	let alias_attributes = field_match.alias().map(|alias| &alias.attributes);
	let mut parts = Vec::new();
	if let Some(push_scope) = field_attributes
		.push_scope
		.as_deref()
		.or_else(|| alias_attributes.and_then(|attributes| attributes.push_scope.as_deref()))
	{
		parts.push(format!("push_scope=`{push_scope}`"));
	}
	let scope = if !field_attributes.scope.is_empty() {
		Some(field_attributes.scope.as_slice())
	} else {
		alias_attributes.and_then(|attributes| {
			(!attributes.scope.is_empty()).then_some(attributes.scope.as_slice())
		})
	};
	if let Some(scope) = scope {
		parts.push(format!("scope={}", format_scope_values(scope)));
	}
	let replace_scope = if !field_attributes.replace_scope.is_empty() {
		Some(&field_attributes.replace_scope)
	} else {
		alias_attributes.and_then(|attributes| {
			(!attributes.replace_scope.is_empty()).then_some(&attributes.replace_scope)
		})
	};
	if let Some(replace_scope) = replace_scope {
		let mut entries = replace_scope
			.iter()
			.map(|(source, target)| format!("`{source}`→`{target}`"))
			.collect::<Vec<_>>();
		entries.sort();
		parts.push(format!("replace_scope={}", entries.join(", ")));
	}
	(!parts.is_empty()).then(|| parts.join("; "))
}

fn format_scope_values(values: &[String]) -> String {
	values
		.iter()
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>()
		.join(", ")
}

fn format_scope_refs(values: &[String]) -> String {
	values
		.iter()
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>()
		.join(", ")
}

fn schema_hover_cardinality(field_match: &CompiledBindFieldMatch<'_>) -> Option<String> {
	schema_match_cardinality(field_match).map(format_cardinality)
}

fn format_cardinality(cardinality: (u32, Option<u32>)) -> String {
	match cardinality {
		(minimum, Some(maximum)) => format!("{minimum}..{maximum}"),
		(minimum, None) => format!("{minimum}..inf"),
	}
}

fn lsp_range_from_span(span: &SpanRange) -> Range {
	Range {
		start: Position {
			line: span.start.line.saturating_sub(1) as u32,
			character: span.start.column.saturating_sub(1) as u32,
		},
		end: Position {
			line: span.end.line.saturating_sub(1) as u32,
			character: span.end.column.saturating_sub(1) as u32,
		},
	}
}

fn span_contains_position(span: &SpanRange, position: Position) -> bool {
	let line = position.line as usize + 1;
	let column = position.character as usize + 1;
	(line > span.start.line || (line == span.start.line && column >= span.start.column))
		&& (line < span.end.line || (line == span.end.line && column < span.end.column))
}

fn schema_completion_candidates(
	engine: &RuleEngine,
	file_path: &Path,
	text: &str,
	position: Position,
	prefix_lower: &str,
) -> Option<Vec<CompletionCandidate>> {
	if !is_schema_key_completion_position(text, position) {
		let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
		return schema_value_completion_candidates(
			engine,
			file_path,
			&parsed.ast.statements,
			text,
			position,
			prefix_lower,
		);
	}
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	let parent_path = find_completion_parent_path(&parsed.ast.statements, position, &[])?;
	let parent_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = engine.bind_context(file_path, &parent_path)?;
	let active_scopes = schema_active_scopes_for_path(engine, file_path, &parent_path);
	let mut candidates = Vec::new();
	for field in completion_rule_fields(parent_context) {
		candidates.extend(schema_completion_entries_for_field(
			engine,
			field,
			&active_scopes,
			prefix_lower,
		));
	}
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	candidates.dedup_by(|left, right| left.label == right.label);
	Some(candidates)
}

fn schema_value_completion_candidates(
	engine: &RuleEngine,
	file_path: &Path,
	statements: &[AstStatement],
	text: &str,
	position: Position,
	prefix_lower: &str,
) -> Option<Vec<CompletionCandidate>> {
	let line = text.lines().nth(position.line as usize).unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	let key = current_assignment_key(&upto)?;
	let parent_path = find_completion_parent_path(statements, position, &[])?;
	let parent_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = engine.bind_context(file_path, &parent_path)?;
	let field_match = engine.bind_field_match(parent_context, key)?;
	let values = schema_allowed_values(engine, schema_match_value(&field_match))?;
	let kind = values.kind.completion_kind();
	let mut candidates = values
		.values
		.iter()
		.filter_map(|value| {
			let value_lower = value.to_ascii_lowercase();
			if !prefix_lower.is_empty() && !value_lower.starts_with(prefix_lower) {
				return None;
			}
			Some(CompletionCandidate {
				label: value.clone(),
				insert_text: value.clone(),
				kind,
				detail: format!("cwt {} {}", values.kind.label(), values.name),
				source: CandidateSource::Schema,
			})
		})
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	Some(candidates)
}

fn is_schema_key_completion_position(text: &str, position: Position) -> bool {
	let line = text
		.lines()
		.nth(position.line as usize)
		.map(str::to_string)
		.unwrap_or_default();
	let upto: String = line.chars().take(position.character as usize).collect();
	current_assignment_key(&upto).is_none()
}

fn find_completion_parent_path(
	statements: &[AstStatement],
	position: Position,
	parent_path: &[String],
) -> Option<Vec<String>> {
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				span,
			} => {
				if span_contains_position(key_span, position) {
					return Some(parent_path.to_vec());
				}
				if let AstValue::Block { items, span } = value
					&& span_contains_position(span, position)
				{
					let mut child_path = parent_path.to_vec();
					child_path.push(key.clone());
					return find_completion_parent_path(items, position, &child_path)
						.or(Some(child_path));
				}
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
			AstStatement::Item { value, span } => {
				if let AstValue::Block {
					items,
					span: block_span,
				} = value && span_contains_position(block_span, position)
				{
					return find_completion_parent_path(items, position, parent_path)
						.or_else(|| Some(parent_path.to_vec()));
				}
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
			AstStatement::Comment { span, .. } => {
				if span_contains_position(span, position) {
					return Some(parent_path.to_vec());
				}
			}
		}
	}
	Some(parent_path.to_vec())
}

fn completion_rule_fields(context: RuleContext<'_>) -> Vec<&CompiledRuleField> {
	match context {
		RuleContext::RootType(root) => root.rules.iter().collect(),
		RuleContext::Subtype(root, subtype) => {
			subtype.rules.iter().chain(root.rules.iter()).collect()
		}
		RuleContext::RuleField(field) => match &field.value {
			CompiledRuleValue::Block(children) => children.iter().collect(),
			_ => Vec::new(),
		},
		RuleContext::AliasRules(rules) => rules.iter().collect(),
	}
}

fn schema_completion_entries_for_field(
	engine: &RuleEngine,
	field: &CompiledRuleField,
	active_scopes: &[String],
	prefix_lower: &str,
) -> Vec<CompletionCandidate> {
	let Some((head, payload)) = parse_schema_marker(&field.key) else {
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	};
	if head != "alias_name" {
		return direct_schema_completion_entry(field, prefix_lower)
			.into_iter()
			.collect();
	}
	let category = CompiledAliasCategory::from_name(payload);
	let mut candidates = engine
		.aliases()
		.iter()
		.filter(|alias| alias.category == category)
		.filter(|alias| schema_alias_scope_matches(engine, alias, active_scopes))
		.filter_map(|alias| {
			let alias_name = &alias.name;
			let alias_name_lower = alias_name.to_ascii_lowercase();
			if !prefix_lower.is_empty() && !alias_name_lower.starts_with(prefix_lower) {
				return None;
			}
			Some(CompletionCandidate {
				label: alias_name.clone(),
				insert_text: alias_name.clone(),
				kind: CompletionItemKind::FUNCTION,
				detail: schema_completion_detail(
					alias
						.attributes
						.description
						.as_deref()
						.or(field.attributes.description.as_deref()),
				),
				source: CandidateSource::Schema,
			})
		})
		.collect::<Vec<_>>();
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	candidates
}

fn schema_active_scopes_for_path(
	engine: &RuleEngine,
	file_path: &Path,
	path: &[&str],
) -> Vec<String> {
	let mut active_scopes = Vec::new();
	for index in 0..=path.len() {
		let Some(context) = engine.bind_context(file_path, &path[..index]) else {
			continue;
		};
		let scopes = schema_context_own_active_scopes(context);
		if !scopes.is_empty() {
			active_scopes = scopes;
		}
	}
	active_scopes
}

fn schema_context_own_active_scopes(context: RuleContext<'_>) -> Vec<String> {
	match context {
		RuleContext::RootType(root) => root.push_scope.iter().cloned().collect(),
		RuleContext::Subtype(root, subtype) => {
			let scopes = schema_active_scopes_from_attributes(&subtype.attributes);
			if scopes.is_empty() {
				root.push_scope.iter().cloned().collect()
			} else {
				scopes
			}
		}
		RuleContext::RuleField(field) => schema_active_scopes_from_attributes(&field.attributes),
		RuleContext::AliasRules(_) => Vec::new(),
	}
}

fn schema_active_scopes_from_attributes(attributes: &CompiledFieldAttributes) -> Vec<String> {
	if let Some(push_scope) = attributes.push_scope.as_ref() {
		return vec![push_scope.clone()];
	}
	if let Some(this_scope) = attributes.replace_scope.get("this") {
		return vec![this_scope.clone()];
	}
	attributes.scope.clone()
}

fn schema_alias_scope_matches(
	engine: &RuleEngine,
	alias: &CompiledAlias,
	active_scopes: &[String],
) -> bool {
	if active_scopes.is_empty() || alias.attributes.scope.is_empty() {
		return true;
	}
	alias.attributes.scope.iter().any(|scope| {
		active_scopes
			.iter()
			.any(|active| engine.scope_matches(scope, active))
	})
}

fn direct_schema_completion_entry(
	field: &CompiledRuleField,
	prefix_lower: &str,
) -> Option<CompletionCandidate> {
	if is_schema_dynamic_key_marker(&field.key) {
		return None;
	}
	let field_key_lower = field.key.to_ascii_lowercase();
	if !prefix_lower.is_empty() && !field_key_lower.starts_with(prefix_lower) {
		return None;
	}
	Some(CompletionCandidate {
		label: field.key.clone(),
		insert_text: field.key.clone(),
		kind: CompletionItemKind::FIELD,
		detail: schema_completion_detail(field.attributes.description.as_deref()),
		source: CandidateSource::Schema,
	})
}

fn schema_completion_detail(description: Option<&str>) -> String {
	description
		.and_then(|text| text.lines().next())
		.map(|line| format!("cwt: {line}"))
		.unwrap_or_else(|| "cwt schema field".to_string())
}

fn parse_schema_marker(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

fn is_schema_dynamic_key_marker(key: &str) -> bool {
	key.len() > 2
		&& key.starts_with('<')
		&& key.ends_with('>')
		&& !key.chars().any(char::is_whitespace)
}

fn schema_diagnostics_for_text(
	engine: &RuleEngine,
	file_path: &Path,
	text: &str,
) -> Vec<Diagnostic> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	schema_diagnostics_for_ast(engine, file_path, &parsed.ast.statements)
}

fn schema_diagnostics_for_ast(
	engine: &RuleEngine,
	file_path: &Path,
	statements: &[AstStatement],
) -> Vec<Diagnostic> {
	let mut diagnostics = Vec::new();
	collect_schema_diagnostics(engine, file_path, statements, &[], None, &mut diagnostics);
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

fn collect_schema_diagnostics(
	engine: &RuleEngine,
	file_path: &Path,
	statements: &[AstStatement],
	parent_path: &[String],
	parent_range: Option<Range>,
	diagnostics: &mut Vec<Diagnostic>,
) {
	let context_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = engine.bind_context(file_path, &context_path);
	let skip_unknown = matches!(parent_context, Some(RuleContext::AliasRules(_)));
	let active_scopes = schema_active_scopes_for_path(engine, file_path, &context_path);
	let mut cardinality_ranges = HashMap::<String, (u32, DiagnosticSeverity, Vec<Range>)>::new();
	let mut present_key_counts = HashMap::<String, u32>::new();
	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				let key_range = lsp_range_from_span(key_span);
				let field_match =
					parent_context.and_then(|context| engine.bind_field_match(context, key));
				if parent_context.is_some() && field_match.is_none() && !skip_unknown {
					diagnostics.push(schema_unknown_key_diagnostic(key_range, key));
				}
				if let Some(field_match) = field_match
					&& let Some(diagnostic) = schema_alias_scope_diagnostic(
						engine,
						&field_match,
						key_range,
						key,
						&active_scopes,
					) {
					diagnostics.push(diagnostic);
				}
				if let Some(field_match) = field_match
					&& let Some(diagnostic) =
						schema_value_shape_diagnostic(&field_match, key, value)
				{
					diagnostics.push(diagnostic);
				}
				if let Some(field_match) = field_match
					&& let AstValue::Scalar {
						value: scalar,
						span,
					} = value
				{
					if let Some(diagnostic) =
						schema_invalid_value_diagnostic(engine, &field_match, key, scalar, span)
					{
						diagnostics.push(diagnostic);
					}
					if let Some(diagnostic) =
						schema_scalar_type_diagnostic(&field_match, key, scalar, span)
					{
						diagnostics.push(diagnostic);
					}
				}
				if let Some(field_match) = field_match {
					*present_key_counts
						.entry(field_match.field().key.clone())
						.or_default() += 1;
				}
				if let Some(field_match) = field_match
					&& let Some(upper_bound) = schema_cardinality_upper(&field_match)
				{
					let severity =
						schema_match_diagnostic_severity(&field_match, DiagnosticSeverity::WARNING);
					let entry = cardinality_ranges
						.entry(key.clone())
						.or_insert_with(|| (upper_bound, severity, Vec::new()));
					if upper_bound > entry.0 {
						entry.0 = upper_bound;
						entry.1 = severity;
					}
					entry.2.push(key_range);
				}
				if let AstValue::Block {
					items,
					span: block_span,
				} = value
				{
					let mut child_path = parent_path.to_vec();
					child_path.push(key.clone());
					collect_schema_diagnostics(
						engine,
						file_path,
						items,
						&child_path,
						Some(lsp_range_from_span(block_span)),
						diagnostics,
					);
				}
			}
			AstStatement::Item {
				value: AstValue::Block {
					items,
					span: block_span,
				},
				..
			} => collect_schema_diagnostics(
				engine,
				file_path,
				items,
				parent_path,
				Some(lsp_range_from_span(block_span)),
				diagnostics,
			),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => {}
		}
	}
	for (key, (upper_bound, severity, ranges)) in cardinality_ranges {
		if ranges.len() <= upper_bound as usize {
			continue;
		}
		for range in ranges.into_iter().skip(upper_bound as usize) {
			diagnostics.push(schema_cardinality_diagnostic(
				range,
				&key,
				upper_bound,
				severity,
			));
		}
	}
	if let Some(parent_context) = parent_context
		&& !skip_unknown
	{
		let range = schema_context_diagnostic_range(parent_range, statements);
		for (field, minimum) in schema_required_fields(parent_context) {
			let present = present_key_counts
				.get(&field.key)
				.copied()
				.unwrap_or_default();
			if present < minimum {
				let severity = schema_attributes_diagnostic_severity(
					&field.attributes,
					DiagnosticSeverity::ERROR,
				);
				diagnostics.push(schema_required_key_diagnostic(
					range, &field.key, minimum, severity,
				));
			}
		}
	}
}

fn schema_cardinality_upper(field_match: &CompiledBindFieldMatch<'_>) -> Option<u32> {
	schema_match_cardinality(field_match).and_then(|(_, upper_bound)| upper_bound)
}

fn schema_match_cardinality(
	field_match: &CompiledBindFieldMatch<'_>,
) -> Option<(u32, Option<u32>)> {
	field_match
		.field()
		.attributes
		.cardinality
		.or_else(|| schema_required_cardinality(&field_match.field().attributes))
		.or_else(|| {
			field_match
				.alias()
				.and_then(|alias| schema_field_attributes_cardinality(&alias.attributes))
		})
}

fn schema_required_fields(context: RuleContext<'_>) -> Vec<(&CompiledRuleField, u32)> {
	let mut fields = completion_rule_fields(context)
		.into_iter()
		.filter(|field| parse_schema_marker(&field.key).is_none())
		.filter(|field| !is_schema_dynamic_key_marker(&field.key))
		.filter_map(|field| {
			schema_field_attributes_cardinality(&field.attributes)
				.map(|(minimum, _)| (field, minimum))
		})
		.filter(|(_, minimum)| *minimum > 0)
		.collect::<Vec<_>>();
	fields.sort_by(|(left, _), (right, _)| left.key.cmp(&right.key));
	fields.dedup_by(|(left, _), (right, _)| left.key == right.key);
	fields
}

fn schema_field_attributes_cardinality(
	attributes: &CompiledFieldAttributes,
) -> Option<(u32, Option<u32>)> {
	attributes
		.cardinality
		.or_else(|| schema_required_cardinality(attributes))
}

fn schema_required_cardinality(attributes: &CompiledFieldAttributes) -> Option<(u32, Option<u32>)> {
	attributes
		.raw
		.iter()
		.any(|(key, value)| key == "required" && value.is_empty())
		.then_some((1, None))
}

fn schema_match_diagnostic_severity(
	field_match: &CompiledBindFieldMatch<'_>,
	default: DiagnosticSeverity,
) -> DiagnosticSeverity {
	field_match
		.alias()
		.and_then(|alias| alias.attributes.severity)
		.or(field_match.field().attributes.severity)
		.map(cwt_diagnostic_severity)
		.unwrap_or(default)
}

fn schema_attributes_diagnostic_severity(
	attributes: &CompiledFieldAttributes,
	default: DiagnosticSeverity,
) -> DiagnosticSeverity {
	attributes
		.severity
		.map(cwt_diagnostic_severity)
		.unwrap_or(default)
}

fn cwt_diagnostic_severity(severity: CompiledSeverity) -> DiagnosticSeverity {
	match severity {
		CompiledSeverity::Error => DiagnosticSeverity::ERROR,
		CompiledSeverity::Warning => DiagnosticSeverity::WARNING,
		CompiledSeverity::Info => DiagnosticSeverity::INFORMATION,
	}
}

fn schema_context_diagnostic_range(
	parent_range: Option<Range>,
	statements: &[AstStatement],
) -> Range {
	parent_range.unwrap_or_else(|| {
		statements
			.first()
			.map(ast_statement_range)
			.unwrap_or_else(zero_range)
	})
}

fn ast_statement_range(statement: &AstStatement) -> Range {
	match statement {
		AstStatement::Assignment { span, .. }
		| AstStatement::Item { span, .. }
		| AstStatement::Comment { span, .. } => lsp_range_from_span(span),
	}
}

fn zero_range() -> Range {
	Range {
		start: Position {
			line: 0,
			character: 0,
		},
		end: Position {
			line: 0,
			character: 0,
		},
	}
}

fn schema_invalid_value_diagnostic(
	engine: &RuleEngine,
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	scalar: &ScalarValue,
	span: &SpanRange,
) -> Option<Diagnostic> {
	let values = schema_allowed_values(engine, schema_match_value(field_match))?;
	let text = scalar.as_text();
	if values.values.iter().any(|value| value == &text) {
		return None;
	}
	Some(Diagnostic {
		range: lsp_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			DiagnosticSeverity::ERROR,
		)),
		code: Some(NumberOrString::String("V003".to_string())),
		source: Some("foch".to_string()),
		message: format!(
			"value `{text}` for `{key}` is not in schema {} `{}` (allowed: {})",
			values.kind.label(),
			values.name,
			format_allowed_values(values.values)
		),
		..Diagnostic::default()
	})
}

fn schema_scalar_type_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	scalar: &ScalarValue,
	span: &SpanRange,
) -> Option<Diagnostic> {
	let expected = schema_scalar_type(schema_match_value(field_match))?;
	if expected.matches(scalar) {
		return None;
	}
	Some(Diagnostic {
		range: lsp_range_from_span(span),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			DiagnosticSeverity::ERROR,
		)),
		code: Some(NumberOrString::String("V005".to_string())),
		source: Some("foch".to_string()),
		message: format!(
			"value `{}` for `{key}` does not match schema type `{}`",
			scalar.as_text(),
			expected.label()
		),
		..Diagnostic::default()
	})
}

fn schema_value_shape_diagnostic(
	field_match: &CompiledBindFieldMatch<'_>,
	key: &str,
	value: &AstValue,
) -> Option<Diagnostic> {
	let expected = schema_value_shape(schema_match_value(field_match));
	let actual = SchemaValueShape::from_ast_value(value);
	if expected == actual {
		return None;
	}
	Some(Diagnostic {
		range: lsp_range_from_span(value.span()),
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			DiagnosticSeverity::ERROR,
		)),
		code: Some(NumberOrString::String("V006".to_string())),
		source: Some("foch".to_string()),
		message: format!(
			"value for `{key}` is a schema {}, but this assignment uses a {}",
			expected.label(),
			actual.label()
		),
		..Diagnostic::default()
	})
}

fn schema_alias_scope_diagnostic(
	engine: &RuleEngine,
	field_match: &CompiledBindFieldMatch<'_>,
	range: Range,
	key: &str,
	active_scopes: &[String],
) -> Option<Diagnostic> {
	let alias = field_match.alias()?;
	if schema_alias_scope_matches(engine, alias, active_scopes) {
		return None;
	}
	Some(Diagnostic {
		range,
		severity: Some(schema_match_diagnostic_severity(
			field_match,
			DiagnosticSeverity::ERROR,
		)),
		code: Some(NumberOrString::String("V007".to_string())),
		source: Some("foch".to_string()),
		message: format!(
			"alias `{key}` is scoped to {}, but current schema scope is {}",
			format_scope_values(&alias.attributes.scope),
			format_scope_refs(active_scopes)
		),
		..Diagnostic::default()
	})
}

fn schema_scalar_type(value: &CompiledRuleValue) -> Option<SchemaScalarType> {
	let value = match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => value.as_str(),
		CompiledRuleValue::Block(_) => return None,
	};
	match value {
		"int" => Some(SchemaScalarType::Int { range: None }),
		"float" => Some(SchemaScalarType::Float { range: None }),
		"bool" => Some(SchemaScalarType::Bool),
		_ => match parse_schema_marker(value) {
			Some(("int", range)) => parse_schema_int_range(range)
				.map(|range| SchemaScalarType::Int { range: Some(range) }),
			Some(("float", range)) => parse_schema_float_range(range)
				.map(|range| SchemaScalarType::Float { range: Some(range) }),
			_ => None,
		},
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SchemaScalarType {
	Int { range: Option<SchemaIntRange> },
	Float { range: Option<SchemaFloatRange> },
	Bool,
}

impl SchemaScalarType {
	fn label(&self) -> String {
		match self {
			Self::Int { range: None } => "int".to_string(),
			Self::Int { range: Some(range) } => range.label("int"),
			Self::Float { range: None } => "float".to_string(),
			Self::Float { range: Some(range) } => range.label("float"),
			Self::Bool => "bool".to_string(),
		}
	}

	fn matches(&self, scalar: &ScalarValue) -> bool {
		match self {
			Self::Int { range } => match scalar {
				ScalarValue::Number(value) => value
					.parse::<i64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number))),
				_ => false,
			},
			Self::Float { range } => match scalar {
				ScalarValue::Number(value) => value
					.parse::<f64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number))),
				_ => false,
			},
			Self::Bool => matches!(scalar, ScalarValue::Bool(_)),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchemaIntRange {
	minimum: i64,
	maximum: Option<i64>,
}

impl SchemaIntRange {
	fn contains(self, value: i64) -> bool {
		value >= self.minimum && self.maximum.is_none_or(|maximum| value <= maximum)
	}

	fn label(self, kind: &str) -> String {
		format!(
			"{kind}[{}..{}]",
			self.minimum,
			self.maximum
				.map(|value| value.to_string())
				.unwrap_or_else(|| "inf".to_string())
		)
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SchemaFloatRange {
	minimum: f64,
	maximum: Option<f64>,
}

impl SchemaFloatRange {
	fn contains(self, value: f64) -> bool {
		value >= self.minimum && self.maximum.is_none_or(|maximum| value <= maximum)
	}

	fn label(self, kind: &str) -> String {
		format!(
			"{kind}[{}..{}]",
			self.minimum,
			self.maximum
				.map(|value| value.to_string())
				.unwrap_or_else(|| "inf".to_string())
		)
	}
}

fn parse_schema_int_range(value: &str) -> Option<SchemaIntRange> {
	let (minimum, maximum) = value.split_once("..")?;
	let minimum = minimum.trim().parse::<i64>().ok()?;
	let maximum = match maximum.trim() {
		"inf" => None,
		value => Some(value.parse::<i64>().ok()?),
	};
	Some(SchemaIntRange { minimum, maximum })
}

fn parse_schema_float_range(value: &str) -> Option<SchemaFloatRange> {
	let (minimum, maximum) = value.split_once("..")?;
	let minimum = minimum.trim().parse::<f64>().ok()?;
	let maximum = match maximum.trim() {
		"inf" => None,
		value => Some(value.parse::<f64>().ok()?),
	};
	Some(SchemaFloatRange { minimum, maximum })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaValueShape {
	Scalar,
	Block,
}

impl SchemaValueShape {
	fn from_ast_value(value: &AstValue) -> Self {
		match value {
			AstValue::Scalar { .. } => Self::Scalar,
			AstValue::Block { .. } => Self::Block,
		}
	}

	fn label(self) -> &'static str {
		match self {
			Self::Scalar => "scalar",
			Self::Block => "block",
		}
	}
}

fn schema_value_shape(value: &CompiledRuleValue) -> SchemaValueShape {
	match value {
		CompiledRuleValue::Scalar(_) | CompiledRuleValue::Marker(_) => SchemaValueShape::Scalar,
		CompiledRuleValue::Block(_) => SchemaValueShape::Block,
	}
}

fn schema_match_value<'p>(field_match: &CompiledBindFieldMatch<'p>) -> &'p CompiledRuleValue {
	field_match
		.alias()
		.map(|alias| &alias.value)
		.unwrap_or_else(|| &field_match.field().value)
}

fn schema_allowed_values<'a>(
	engine: &'a RuleEngine,
	value: &'a CompiledRuleValue,
) -> Option<SchemaAllowedValues<'a>> {
	let value = match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => value,
		CompiledRuleValue::Block(_) => return None,
	};
	let (head, name) = schema_allowed_value_marker(value)?;
	let (kind, values) = match head {
		"enum" => (SchemaAllowedValueKind::Enum, engine.enum_values(name)?),
		"value_set" => (
			SchemaAllowedValueKind::ValueSet,
			engine.value_set_values(name)?,
		),
		_ => return None,
	};
	(!values.is_empty()).then_some(SchemaAllowedValues { kind, name, values })
}

fn schema_allowed_value_marker(text: &str) -> Option<(&str, &str)> {
	match parse_schema_marker(text) {
		Some(("enum", name)) => Some(("enum", name)),
		Some(("value_set", name)) => Some(("value_set", name)),
		_ => None,
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchemaAllowedValues<'a> {
	kind: SchemaAllowedValueKind,
	name: &'a str,
	values: &'a [String],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaAllowedValueKind {
	Enum,
	ValueSet,
}

impl SchemaAllowedValueKind {
	fn label(self) -> &'static str {
		match self {
			Self::Enum => "enum",
			Self::ValueSet => "value_set",
		}
	}

	fn completion_kind(self) -> CompletionItemKind {
		match self {
			Self::Enum => CompletionItemKind::ENUM_MEMBER,
			Self::ValueSet => CompletionItemKind::VALUE,
		}
	}
}

fn format_allowed_values(values: &[String]) -> String {
	const MAX_VALUES: usize = 12;
	let mut formatted = values
		.iter()
		.take(MAX_VALUES)
		.map(|value| format!("`{value}`"))
		.collect::<Vec<_>>();
	if values.len() > MAX_VALUES {
		formatted.push(format!("... {} more", values.len() - MAX_VALUES));
	}
	formatted.join(", ")
}

fn schema_unknown_key_diagnostic(range: Range, key: &str) -> Diagnostic {
	Diagnostic {
		range,
		severity: Some(DiagnosticSeverity::WARNING),
		code: Some(NumberOrString::String("V001".to_string())),
		source: Some("foch".to_string()),
		message: format!("schema does not allow key `{key}` in this context"),
		..Diagnostic::default()
	}
}

fn schema_cardinality_diagnostic(
	range: Range,
	key: &str,
	upper_bound: u32,
	severity: DiagnosticSeverity,
) -> Diagnostic {
	Diagnostic {
		range,
		severity: Some(severity),
		code: Some(NumberOrString::String("V002".to_string())),
		source: Some("foch".to_string()),
		message: format!("key `{key}` exceeds schema cardinality upper bound of {upper_bound}"),
		..Diagnostic::default()
	}
}

fn schema_required_key_diagnostic(
	range: Range,
	key: &str,
	minimum: u32,
	severity: DiagnosticSeverity,
) -> Diagnostic {
	Diagnostic {
		range,
		severity: Some(severity),
		code: Some(NumberOrString::String("V004".to_string())),
		source: Some("foch".to_string()),
		message: format!("schema requires key `{key}` at least {minimum} time(s) in this context"),
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
	rule_engine: Option<Arc<RuleEngine>>,
) -> WorkspaceSnapshot {
	let mut parsed = Vec::new();
	let mut file_paths = Vec::new();
	let mut path_lookup = HashMap::new();
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
	}

	let index = build_semantic_index(&parsed);
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
	let mut diagnostics_by_path = build_workspace_diagnostics(&index, &path_lookup, &findings);
	if let Some(engine) = rule_engine.as_ref() {
		for file in &parsed {
			let schema_diagnostics = schema_diagnostics_for_ast(
				engine.as_ref(),
				&file.relative_path,
				&file.ast.statements,
			);
			if schema_diagnostics.is_empty() {
				continue;
			}
			diagnostics_by_path
				.entry(normalize_path(&file.path))
				.or_default()
				.extend(schema_diagnostics);
		}
		for diagnostics in diagnostics_by_path.values_mut() {
			sort_and_dedup_diagnostics(diagnostics);
		}
	}

	let session = WorkspaceSession::from_analysis(index, file_paths, path_lookup, findings);

	WorkspaceSnapshot {
		candidates,
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
		"interface",
		"common/interface",
		"gfx",
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
			let Some(ext) = path.extension() else {
				continue;
			};
			let ext = ext.to_string_lossy();
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
		CandidateSource, CompletionCandidate, CompletionContext, ScanTarget, TargetRole,
		assignment_key_on_line, build_workspace_snapshot, build_workspace_snapshot_with_schema,
		detect_completion_context, document_symbols, extract_completion_prefix,
		find_vendored_schema_dir_from, load_rule_engine_with_cache_dir,
		localisation_stub_code_actions, parse_scan_targets_json, resolve_definition_locations,
		resolve_reference_locations, schema_completion_candidates, schema_diagnostics_for_text,
		schema_hover, select_completion_candidates, workspace_symbols,
	};
	use foch_core::model::test_support;
	use foch_cwt::{CwtSchemaGraph, RuleEngine};
	use std::fs;
	use std::path::{Path, PathBuf};
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

	#[test]
	fn vendored_schema_dir_lookup_walks_to_repo_root() {
		let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
		let found = find_vendored_schema_dir_from(&crate_root).expect("find vendored schema dir");
		assert!(found.ends_with("vendor/cwtools-eu4-config"));
	}

	#[test]
	fn schema_loader_reads_existing_fixture_pack() {
		let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("..")
			.join("foch-cwt")
			.join("tests/fixtures/schema-pack");
		let cache = TempDir::new().expect("create test CWT rule cache");
		let engine = load_rule_engine_with_cache_dir(&fixture_dir, Some(cache.path()))
			.expect("load fixture schema graph")
			.engine;
		assert!(engine.root_count() >= 2);
		assert!(engine.bind_root(Path::new("events/example.txt")).is_some());
	}

	fn lsp_fixture_dir() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests")
			.join("fixtures")
			.join("lsp")
	}

	fn load_lsp_rule_engine() -> std::sync::Arc<foch_cwt::RuleEngine> {
		let cache = TempDir::new().expect("create test CWT rule cache");
		load_rule_engine_with_cache_dir(&lsp_fixture_dir().join("schema"), Some(cache.path()))
			.expect("load LSP rule engine")
			.engine
	}

	fn load_inline_lsp_rule_engine(schema: &str) -> std::sync::Arc<foch_cwt::RuleEngine> {
		let schema_dir = TempDir::new().expect("create inline CWT schema dir");
		fs::write(schema_dir.path().join("inline.cwt"), schema).expect("write inline CWT schema");
		let cache = TempDir::new().expect("create inline CWT rule cache");
		load_rule_engine_with_cache_dir(schema_dir.path(), Some(cache.path()))
			.expect("load inline LSP rule engine")
			.engine
	}

	fn fixture_text(relative_path: &str) -> String {
		fs::read_to_string(lsp_fixture_dir().join(relative_path)).expect("read LSP fixture text")
	}

	fn position_for_token(text: &str, token: &str) -> Position {
		for (line_index, line) in text.lines().enumerate() {
			if let Some(character) = line.find(token) {
				return Position {
					line: line_index as u32,
					character: character as u32,
				};
			}
		}
		panic!("token `{token}` not found");
	}

	fn position_for_token_offset(text: &str, token: &str, offset: u32) -> Position {
		let mut position = position_for_token(text, token);
		position.character += offset;
		position
	}

	fn hover_markdown(hover: tower_lsp::lsp_types::Hover) -> String {
		let HoverContents::Markup(markup) = hover.contents else {
			panic!("expected markdown hover contents");
		};
		markup.value
	}

	#[test]
	fn hover_renders_event_field_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let hover = schema_hover(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token(&text, "immediate"),
		)
		.expect("event hover");
		let markdown = hover_markdown(hover);
		assert!(markdown.contains("**immediate**"));
		assert!(markdown.contains("Type: `Block`"));
		assert!(markdown.contains("Immediate effects executed when the event fires."));
		assert!(markdown.contains("push_scope=`country`"));
		assert!(markdown.contains("scope=`country`, `province`"));
	}

	#[test]
	fn hover_renders_enum_value_constraints_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let hover = schema_hover(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token(&text, "category"),
		)
		.expect("category hover");
		let markdown = hover_markdown(hover);
		assert!(markdown.contains("**category**"));
		assert!(markdown.contains("Type: `Scalar`"));
		assert!(markdown.contains("Value set: `enum` `power_categories`"));
		assert!(markdown.contains("`ADM`, `DIP`, `MIL`"));
		assert!(markdown.contains("Cardinality: `1..1`"));
	}

	#[test]
	fn hover_renders_mission_field_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("missions/sample.txt");
		let hover = schema_hover(
			engine.as_ref(),
			Path::new("missions/sample.txt"),
			&text,
			position_for_token(&text, "provinces_to_highlight"),
		)
		.expect("mission hover");
		let markdown = hover_markdown(hover);
		assert!(markdown.contains("**provinces_to_highlight**"));
		assert!(markdown.contains("Type: `Block`"));
		assert!(markdown.contains("Selects provinces relevant to the mission."));
		assert!(markdown.contains("Cardinality: `0..1`"));
		assert!(markdown.contains("`root`→`country`"));
		assert!(markdown.contains("`this`→`province`"));
	}

	#[test]
	fn hover_renders_scripted_effect_field_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("common/scripted_effects/sample.txt");
		let hover = schema_hover(
			engine.as_ref(),
			Path::new("common/scripted_effects/sample.txt"),
			&text,
			position_for_token(&text, "add_prestige"),
		)
		.expect("scripted effect hover");
		let markdown = hover_markdown(hover);
		assert!(markdown.contains("**add_prestige**"));
		assert!(markdown.contains("Type: `Scalar`"));
		assert!(markdown.contains("Adds prestige directly from a scripted effect body."));
	}

	#[test]
	fn completion_suggests_event_children_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token(&text, "trigger"),
			"",
		)
		.expect("event schema completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"title"));
		assert!(labels.contains(&"trigger"));
		assert!(labels.contains(&"immediate"));
		assert!(candidates.iter().any(|candidate| {
			candidate.label == "immediate"
				&& candidate
					.detail
					.starts_with("cwt: Immediate effects executed")
		}));
	}

	#[test]
	fn completion_expands_trigger_aliases_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token(&text, "has_country_flag"),
			"has",
		)
		.expect("trigger schema completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"has_country_flag"));
		assert!(!labels.contains(&"is_year"));
		assert!(
			candidates
				.iter()
				.all(|candidate| candidate.kind == CompletionItemKind::FUNCTION)
		);
		assert!(candidates.iter().any(|candidate| {
			candidate.label == "has_country_flag"
				&& candidate
					.detail
					.starts_with("cwt: Checks whether the current country")
		}));
	}

	#[test]
	fn completion_suggests_enum_values_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token_offset(&text, "ADM", 1),
			"a",
		)
		.expect("enum value completion");
		assert_eq!(candidates.len(), 1);
		assert_eq!(candidates[0].label, "ADM");
		assert_eq!(candidates[0].kind, CompletionItemKind::ENUM_MEMBER);
		assert_eq!(candidates[0].detail, "cwt enum power_categories");
	}

	#[test]
	fn completion_suggests_value_set_values_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token_offset(&text, "root", 2),
			"ro",
		)
		.expect("value_set value completion");
		assert_eq!(candidates.len(), 1);
		assert_eq!(candidates[0].label, "root");
		assert_eq!(candidates[0].kind, CompletionItemKind::VALUE);
		assert_eq!(candidates[0].detail, "cwt value_set event_targets");
	}

	#[test]
	fn completion_suggests_scripted_effect_fields_from_schema() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("common/scripted_effects/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("common/scripted_effects/sample.txt"),
			&text,
			position_for_token(&text, "add_prestige"),
			"add",
		)
		.expect("scripted effect schema completion");
		assert_eq!(candidates.len(), 1);
		assert_eq!(candidates[0].label, "add_prestige");
		assert_eq!(candidates[0].kind, CompletionItemKind::FIELD);
		assert!(
			candidates[0]
				.detail
				.starts_with("cwt: Adds prestige directly from a scripted effect body.")
		);
	}

	#[test]
	fn completion_filters_aliases_by_active_scope_when_known() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			&text,
			position_for_token(&text, "add_prestige"),
			"",
		)
		.expect("scope-filtered effect schema completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"add_prestige"));
		assert!(!labels.contains(&"province_only_effect"));
	}

	#[test]
	fn completion_inherits_subtype_scope_for_alias_filtering() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = {\n    has\n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			text,
			position_for_token_offset(text, "has", 3),
			"has",
		)
		.expect("subtype-scope-filtered trigger completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"has_country_flag"));
		assert!(!labels.contains(&"has_province_flag"));
		assert!(!labels.contains(&"has_sea_flag"));
	}

	#[test]
	fn completion_accepts_parent_scope_aliases_in_subscope_context() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    country_wide_effect = 1\n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("events/sample.txt"),
			text,
			position_for_token(text, "country_wide_effect"),
			"",
		)
		.expect("subscope effect schema completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"country_wide_effect"));
		assert!(labels.contains(&"add_prestige"));
		assert!(labels.contains(&"province_only_effect"));
	}

	#[test]
	fn completion_uses_replace_scope_this_for_alias_filtering() {
		let engine = load_lsp_rule_engine();
		let text = "demo_mission = {\n  provinces_to_highlight = {\n    has\n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("missions/sample.txt"),
			text,
			position_for_token_offset(text, "has", 3),
			"has",
		)
		.expect("replace_scope-filtered trigger completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"has_country_flag"));
		assert!(labels.contains(&"has_province_flag"));
		assert!(!labels.contains(&"has_sea_flag"));
	}

	#[test]
	fn completion_uses_root_type_key_filter_exclusion_for_schema_context() {
		let schema = r#"
		types = {
			type[idea_group] = {
				path = "game/common/ideas"
				subtype[selectable] = {
					category = scalar
				}
			}
			## type_key_filter <> { start trigger bonus ai_will_do }
			type[idea] = {
				path = "game/common/ideas"
				skip_root_key = any
			}
		}

		idea_group = {
			subtype[selectable] = {
				category = scalar
			}
		}

		idea = {
			idea_only = bool
		}
		"#;
		let schema_dir = TempDir::new().expect("create inline schema dir");
		fs::write(schema_dir.path().join("ideas.cwt"), schema).expect("write inline schema");
		let graph = CwtSchemaGraph::from_directory(schema_dir.path()).expect("load inline schema");
		let engine = RuleEngine::from_graph(&graph);
		let text = "sample_group = {\n  sample_idea = {\n    \n  }\n}\n";
		let candidates = schema_completion_candidates(
			&engine,
			Path::new("common/ideas/sample.txt"),
			text,
			Position {
				line: 2,
				character: 4,
			},
			"",
		)
		.expect("schema completion under filtered idea root");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"idea_only"));
		assert!(!labels.contains(&"category"));
	}

	#[test]
	fn completion_uses_ordered_skip_root_key_chain_for_schema_context() {
		let schema = r#"
		types = {
			type[game_age] = {
				path = "game/common/ages"
			}
			type[game_age_ability] = {
				path = "game/common/ages"
				skip_root_key = { any abilities }
			}
		}

		game_age = {
			start = int
		}

		game_age_ability = {
			ability_only = bool
		}
		"#;
		let engine = load_inline_lsp_rule_engine(schema);
		let text = "age_of_discovery = {\n  abilities = {\n    free_war_taxes = {\n      \n    }\n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("common/ages/sample.txt"),
			text,
			Position {
				line: 3,
				character: 6,
			},
			"",
		)
		.expect("schema completion under ordered skip_root_key chain");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"ability_only"));
		assert!(!labels.contains(&"start"));
	}

	#[test]
	fn completion_uses_cwt_path_file_for_root_matching() {
		let schema = r#"
		types = {
			type[map_fallback] = {
				path = "game/map"
			}
			type[area] = {
				path = "game/map"
				path_file = "area.txt"
			}
			type[region] = {
				path = "game/map"
				path_file = "region.txt"
			}
		}

		map_fallback = {
			fallback_only = bool
		}

		area = {
			area_only = bool
		}

		region = {
			region_only = bool
		}
		"#;
		let engine = load_inline_lsp_rule_engine(schema);
		let text = "sample_area = {\n  \n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("map/area.txt"),
			text,
			Position {
				line: 1,
				character: 2,
			},
			"",
		)
		.expect("path_file-specific area schema completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"area_only"));
		assert!(!labels.contains(&"region_only"));
		assert!(!labels.contains(&"fallback_only"));
	}

	#[test]
	fn completion_binds_dynamic_cwt_marker_fields() {
		let engine = load_lsp_rule_engine();
		let text =
			"demo_mission = {\n  mission_tree = {\n    conquest = {\n      has\n    }\n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("missions/sample.txt"),
			text,
			position_for_token_offset(text, "has", 3),
			"has",
		)
		.expect("dynamic-field trigger completion");
		let labels = candidates
			.iter()
			.map(|candidate| candidate.label.as_str())
			.collect::<Vec<_>>();
		assert!(labels.contains(&"has_country_flag"));
		assert!(labels.contains(&"has_province_flag"));
		assert!(!labels.contains(&"has_sea_flag"));
	}

	#[test]
	fn completion_does_not_suggest_dynamic_marker_literals() {
		let engine = load_lsp_rule_engine();
		let text = "demo_mission = {\n  mission_tree = {\n    \n  }\n}\n";
		let candidates = schema_completion_candidates(
			engine.as_ref(),
			Path::new("missions/sample.txt"),
			text,
			Position {
				line: 2,
				character: 4,
			},
			"",
		)
		.expect("mission tree schema completion");
		assert!(
			!candidates
				.iter()
				.any(|candidate| candidate.label == "<mission_stage>")
		);
	}

	#[test]
	fn diagnostics_report_alias_scope_mismatches_when_scope_is_known() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  immediate = {\n    province_only_effect = 1\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("province_only_effect")
				&& diagnostic.message.contains("`province`")
				&& diagnostic.message.contains("`country`")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
	}

	#[test]
	fn diagnostics_inherit_subtype_scope_for_alias_mismatches() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = {\n    has_province_flag = demo_flag\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_province_flag")
				&& diagnostic.message.contains("`province`")
				&& diagnostic.message.contains("`country`")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
	}

	#[test]
	fn diagnostics_use_replace_scope_this_for_alias_mismatches() {
		let engine = load_lsp_rule_engine();
		let text = "demo_mission = {\n  provinces_to_highlight = {\n    has_country_flag = demo_flag\n    has_province_flag = demo_flag\n    has_sea_flag = demo_flag\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("missions/sample.txt"), text);
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_country_flag")
		}));
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_province_flag")
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_sea_flag")
				&& diagnostic.message.contains("`sea`")
				&& diagnostic.message.contains("`province`")
		}));
	}

	#[test]
	fn diagnostics_bind_dynamic_cwt_marker_fields() {
		let engine = load_lsp_rule_engine();
		let text = "demo_mission = {\n  mission_tree = {\n    conquest = {\n      has_country_flag = demo_flag\n      has_province_flag = demo_flag\n      has_sea_flag = demo_flag\n    }\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("missions/sample.txt"), text);
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V001".to_string()))
				&& diagnostic.message.contains("conquest")
		}));
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_country_flag")
		}));
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_province_flag")
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("has_sea_flag")
				&& diagnostic.message.contains("`sea`")
				&& diagnostic.message.contains("`province`")
		}));
	}

	#[test]
	fn diagnostics_accept_parent_scope_aliases_in_subscope_context() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  province_effects = {\n    country_wide_effect = 1\n    province_only_effect = 1\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("country_wide_effect")
		}));
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("province_only_effect")
		}));
	}

	#[test]
	fn diagnostics_report_unknown_keys_and_cardinality_violations() {
		let engine = load_lsp_rule_engine();
		let text = fixture_text("events/diagnostics.txt");
		let diagnostics = schema_diagnostics_for_text(
			engine.as_ref(),
			Path::new("events/diagnostics.txt"),
			&text,
		);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V001".to_string()))
				&& diagnostic.message.contains("mystery_key")
				&& diagnostic.severity == Some(DiagnosticSeverity::WARNING)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V002".to_string()))
				&& diagnostic.message.contains("title")
				&& diagnostic.severity == Some(DiagnosticSeverity::WARNING)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V003".to_string()))
				&& diagnostic.message.contains("ECO")
				&& diagnostic.message.contains("power_categories")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V003".to_string()))
				&& diagnostic.message.contains("elsewhere")
				&& diagnostic.message.contains("event_targets")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("many")
				&& diagnostic.message.contains("days")
				&& diagnostic.message.contains("int")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("heavy")
				&& diagnostic.message.contains("chance")
				&& diagnostic.message.contains("float")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("maybe")
				&& diagnostic.message.contains("hidden")
				&& diagnostic.message.contains("bool")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("much")
				&& diagnostic.message.contains("add_prestige")
				&& diagnostic.message.contains("int")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
	}

	#[test]
	fn diagnostics_use_cwt_severity_for_schema_findings() {
		let schema = r#"
		types = {
			type[event] = {
				path = "game/events"
			}
		}

		event = {
			## severity = warning
			gentle_bool = bool

			## required
			## severity = info
			soft_required = scalar

			## cardinality = 1..1
			## severity = info
			singleton = scalar

			## push_scope = country
			trigger = {
				alias_name[trigger] = alias_match_left[trigger]
			}
		}

		## scope = sea
		## severity = warning
		alias[trigger:sea_only_trigger] = bool

		scopes = {
			country = { aliases = { country } }
			sea = { aliases = { sea } }
		}
		"#;
		let engine = load_inline_lsp_rule_engine(schema);
		let text = "\
sample = {
  gentle_bool = maybe
  singleton = first
  singleton = second
  trigger = {
    sea_only_trigger = yes
  }
}
";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("gentle_bool")
				&& diagnostic.severity == Some(DiagnosticSeverity::WARNING)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V002".to_string()))
				&& diagnostic.message.contains("singleton")
				&& diagnostic.severity == Some(DiagnosticSeverity::INFORMATION)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V004".to_string()))
				&& diagnostic.message.contains("soft_required")
				&& diagnostic.severity == Some(DiagnosticSeverity::INFORMATION)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V007".to_string()))
				&& diagnostic.message.contains("sea_only_trigger")
				&& diagnostic.severity == Some(DiagnosticSeverity::WARNING)
		}));
	}

	#[test]
	fn diagnostics_validate_cwt_ranged_scalar_types() {
		let schema = r#"
		types = {
			type[event] = {
				path = "game/events"
			}
		}

		event = {
			limited_int = int[1..3]
			limited_float = float[-1.0..1.0]
			open_int = int[0..inf]
		}
		"#;
		let engine = load_inline_lsp_rule_engine(schema);
		let text = "\
sample = {
  limited_int = 4
  limited_float = -2.0
  open_int = 99
}
";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("limited_int")
				&& diagnostic.message.contains("int[1..3]")
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("limited_float")
				&& diagnostic.message.contains("float[-1..1]")
		}));
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V005".to_string()))
				&& diagnostic.message.contains("open_int")
		}));
	}

	#[test]
	fn diagnostics_report_missing_required_schema_keys() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  title = sample_title\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V004".to_string()))
				&& diagnostic.message.contains("category")
				&& diagnostic.message.contains("at least 1")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V004".to_string()))
				&& diagnostic.message.contains("target")
				&& diagnostic.message.contains("at least 1")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
	}

	#[test]
	fn diagnostics_report_schema_value_shape_mismatches() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  category = ADM\n  target = root\n  trigger = yes\n  days = { value = 1 }\n  immediate = {\n    add_prestige = { amount = 5 }\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V006".to_string()))
				&& diagnostic.message.contains("trigger")
				&& diagnostic.message.contains("schema block")
				&& diagnostic.message.contains("scalar")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V006".to_string()))
				&& diagnostic.message.contains("days")
				&& diagnostic.message.contains("schema scalar")
				&& diagnostic.message.contains("block")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V006".to_string()))
				&& diagnostic.message.contains("add_prestige")
				&& diagnostic.message.contains("schema scalar")
				&& diagnostic.message.contains("block")
				&& diagnostic.severity == Some(DiagnosticSeverity::ERROR)
		}));
	}

	#[test]
	fn diagnostics_skip_unknown_keys_inside_alias_bodies() {
		let engine = load_lsp_rule_engine();
		let text = "namespace = sample\ncountry_event = {\n  trigger = {\n    custom_trigger = {\n      mystery_key = yes\n    }\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(engine.as_ref(), Path::new("events/sample.txt"), text);
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V001".to_string()))
				&& diagnostic.message.contains("mystery_key")
		}));
	}

	#[test]
	fn workspace_snapshot_includes_schema_diagnostics() {
		let engine = load_lsp_rule_engine();
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
}
