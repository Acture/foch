use foch_core::model::{
	AnalysisMode, Finding, SemanticIndex, Severity, SymbolDefinition, SymbolKind as FochSymbolKind,
};
use foch_cwt::{
	AliasCategory, BindContext, BindFieldMatch, CwtRuleField, CwtRuleValue, CwtSchemaGraph,
	SchemaBinding, SchemaPack, SchemaSource, install_base_scopes,
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
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
	CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
	Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
	DidSaveTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
	GotoDefinitionResponse, Hover, HoverContents, HoverParams, InitializeParams, InitializeResult,
	InitializedParams, Location, MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf,
	Position, Range, ReferenceParams, ServerCapabilities, SymbolInformation,
	SymbolKind as LspSymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
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
	schema_graph: Arc<RwLock<Option<Arc<CwtSchemaGraph>>>>,
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
			schema_graph: Arc::new(RwLock::new(None)),
		}
	}

	async fn refresh_workspace_snapshot(&self) {
		let targets = { self.state.read().await.targets.clone() };
		let schema_graph = self.schema_graph.read().await.clone();
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || {
			build_workspace_snapshot_with_schema(&targets, schema_graph)
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
		let schema_graph = self.schema_graph.read().await.clone();
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
		if let (Some(graph), Some(relative_path)) = (schema_graph.as_ref(), relative_path.as_ref())
		{
			diagnostics.extend(schema_diagnostics_for_text(
				graph.as_ref(),
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
		let schema_graph = match find_vendored_schema_dir() {
			Some(schema_dir) => {
				let load_result =
					tokio::task::spawn_blocking(move || load_schema_graph(&schema_dir)).await;
				match load_result {
					Ok(Ok(graph)) => {
						self.client
							.log_message(
								MessageType::INFO,
								format!(
									"foch lsp loaded CWT schema graph: {} types, {} aliases",
									graph.types.len(),
									graph.aliases.len()
								),
							)
							.await;
						Some(graph)
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
		*self.schema_graph.write().await = schema_graph;

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
		let Some(graph) = self.schema_graph.read().await.clone() else {
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
			graph.as_ref(),
			&relative_path,
			&text,
			position,
		))
	}

	async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
		let graph = self.schema_graph.read().await.clone();
		let state = self.state.read().await;
		let uri = &params.text_document_position.text_document.uri;
		let position = params.text_document_position.position;
		let text = state.docs.get(uri).map(String::as_str).unwrap_or_default();
		let prefix = extract_completion_prefix(text, position);
		let context = detect_completion_context(text, position);
		let prefix_lower = prefix.to_ascii_lowercase();

		let mut candidates = if let Some(graph) = graph.as_ref()
			&& let Ok(path) = uri.to_file_path()
			&& let Some((_, relative_path)) = match_scan_target(&state.targets, &path)
			&& let Some(candidates) = schema_completion_candidates(
				graph.as_ref(),
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

fn load_schema_graph(schema_dir: &Path) -> std::result::Result<Arc<CwtSchemaGraph>, String> {
	let pack = SchemaPack::load_from_dir(
		schema_dir,
		SchemaSource::UserProvided {
			path: schema_dir.to_path_buf(),
		},
	)
	.map_err(|err| err.to_string())?;
	install_base_scopes(pack.graph.as_ref());
	Ok(pack.graph)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyPathTarget {
	parent_path: Vec<String>,
	key: String,
	range: Range,
}

fn schema_hover(
	graph: &CwtSchemaGraph,
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
	let SchemaBinding::Bound { .. } = graph.bind_chain(file_path, &ast_path) else {
		return None;
	};
	let parent_path = target
		.parent_path
		.iter()
		.map(String::as_str)
		.collect::<Vec<_>>();
	let parent_context = graph.bind_context(file_path, &parent_path)?;
	let field_match = graph.bind_field_match(parent_context, &target.key)?;
	Some(Hover {
		contents: HoverContents::Markup(MarkupContent {
			kind: MarkupKind::Markdown,
			value: render_schema_hover_markdown(&target.key, &field_match),
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

fn render_schema_hover_markdown(key: &str, field_match: &BindFieldMatch<'_>) -> String {
	let field = field_match.field();
	let mut sections = vec![
		format!("**{key}**"),
		format!("Type: `{}`", rule_value_kind(&field.value)),
	];
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

fn rule_value_kind(value: &CwtRuleValue) -> &'static str {
	match value {
		CwtRuleValue::Scalar(_) => "Scalar",
		CwtRuleValue::Block(_) => "Block",
		CwtRuleValue::Marker(_) => "Marker",
	}
}

fn schema_hover_description<'a>(field_match: &'a BindFieldMatch<'a>) -> Option<&'a str> {
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

fn schema_hover_scope_context(field_match: &BindFieldMatch<'_>) -> Option<String> {
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

fn schema_hover_cardinality(field_match: &BindFieldMatch<'_>) -> Option<String> {
	field_match
		.field()
		.attributes
		.cardinality
		.or_else(|| {
			field_match
				.alias()
				.and_then(|alias| alias.attributes.cardinality)
		})
		.map(format_cardinality)
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
	graph: &CwtSchemaGraph,
	file_path: &Path,
	text: &str,
	position: Position,
	prefix_lower: &str,
) -> Option<Vec<CompletionCandidate>> {
	if !is_schema_key_completion_position(text, position) {
		return None;
	}
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	let parent_path = find_completion_parent_path(&parsed.ast.statements, position, &[])?;
	let parent_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = graph.bind_context(file_path, &parent_path)?;
	let mut candidates = Vec::new();
	for field in completion_rule_fields(parent_context) {
		candidates.extend(schema_completion_entries_for_field(
			graph,
			field,
			prefix_lower,
		));
	}
	candidates.sort_by(|left, right| left.label.cmp(&right.label));
	candidates.dedup_by(|left, right| left.label == right.label);
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

fn completion_rule_fields(context: BindContext<'_>) -> Vec<&CwtRuleField> {
	match context {
		BindContext::RootType(root) => root.rules.iter().collect(),
		BindContext::Subtype(root, subtype) => {
			subtype.rules.iter().chain(root.rules.iter()).collect()
		}
		BindContext::RuleField(field) => match &field.value {
			CwtRuleValue::Block(children) => children.iter().collect(),
			_ => Vec::new(),
		},
		BindContext::AliasRules(rules) => rules.iter().collect(),
	}
}

fn schema_completion_entries_for_field(
	graph: &CwtSchemaGraph,
	field: &CwtRuleField,
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
	let category = AliasCategory::from_name(payload);
	let mut candidates = graph
		.aliases
		.iter()
		.filter(|((alias_category, _), _)| *alias_category == category)
		.filter_map(|((_, alias_name), alias)| {
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

fn direct_schema_completion_entry(
	field: &CwtRuleField,
	prefix_lower: &str,
) -> Option<CompletionCandidate> {
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

fn schema_diagnostics_for_text(
	graph: &CwtSchemaGraph,
	file_path: &Path,
	text: &str,
) -> Vec<Diagnostic> {
	let parsed = parse_clausewitz_content(file_path.to_path_buf(), text);
	schema_diagnostics_for_ast(graph, file_path, &parsed.ast.statements)
}

fn schema_diagnostics_for_ast(
	graph: &CwtSchemaGraph,
	file_path: &Path,
	statements: &[AstStatement],
) -> Vec<Diagnostic> {
	let mut diagnostics = Vec::new();
	collect_schema_diagnostics(graph, file_path, statements, &[], &mut diagnostics);
	sort_and_dedup_diagnostics(&mut diagnostics);
	diagnostics
}

fn collect_schema_diagnostics(
	graph: &CwtSchemaGraph,
	file_path: &Path,
	statements: &[AstStatement],
	parent_path: &[String],
	diagnostics: &mut Vec<Diagnostic>,
) {
	let context_path = parent_path.iter().map(String::as_str).collect::<Vec<_>>();
	let parent_context = graph.bind_context(file_path, &context_path);
	let skip_unknown = matches!(parent_context, Some(BindContext::AliasRules(_)));
	let mut cardinality_ranges = HashMap::<String, (u32, Vec<Range>)>::new();
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
					parent_context.and_then(|context| graph.bind_field_match(context, key));
				if parent_context.is_some() && field_match.is_none() && !skip_unknown {
					diagnostics.push(schema_unknown_key_diagnostic(key_range, key));
				}
				if let Some(field_match) = field_match
					&& let Some(upper_bound) = schema_cardinality_upper(&field_match)
				{
					let entry = cardinality_ranges
						.entry(key.clone())
						.or_insert_with(|| (upper_bound, Vec::new()));
					entry.0 = entry.0.max(upper_bound);
					entry.1.push(key_range);
				}
				if let AstValue::Block { items, .. } = value {
					let mut child_path = parent_path.to_vec();
					child_path.push(key.clone());
					collect_schema_diagnostics(graph, file_path, items, &child_path, diagnostics);
				}
			}
			AstStatement::Item {
				value: AstValue::Block { items, .. },
				..
			} => collect_schema_diagnostics(graph, file_path, items, parent_path, diagnostics),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => {}
		}
	}
	for (key, (upper_bound, ranges)) in cardinality_ranges {
		if ranges.len() <= upper_bound as usize {
			continue;
		}
		for range in ranges.into_iter().skip(upper_bound as usize) {
			diagnostics.push(schema_cardinality_diagnostic(range, &key, upper_bound));
		}
	}
}

fn schema_cardinality_upper(field_match: &BindFieldMatch<'_>) -> Option<u32> {
	field_match
		.field()
		.attributes
		.cardinality
		.and_then(|(_, upper_bound)| upper_bound)
		.or_else(|| {
			field_match.alias().and_then(|alias| {
				alias
					.attributes
					.cardinality
					.and_then(|(_, upper_bound)| upper_bound)
			})
		})
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

fn schema_cardinality_diagnostic(range: Range, key: &str, upper_bound: u32) -> Diagnostic {
	Diagnostic {
		range,
		severity: Some(DiagnosticSeverity::WARNING),
		code: Some(NumberOrString::String("V002".to_string())),
		source: Some("foch".to_string()),
		message: format!("key `{key}` exceeds schema cardinality upper bound of {upper_bound}"),
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
	schema_graph: Option<Arc<CwtSchemaGraph>>,
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
	if let Some(graph) = schema_graph.as_ref() {
		for file in &parsed {
			let schema_diagnostics = schema_diagnostics_for_ast(
				graph.as_ref(),
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
	best.map(|(_, root, relative)| (root, relative))
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
		find_vendored_schema_dir_from, load_schema_graph, parse_scan_targets_json,
		resolve_definition_locations, resolve_reference_locations, schema_completion_candidates,
		schema_diagnostics_for_text, schema_hover, select_completion_candidates, workspace_symbols,
	};
	use foch_core::model::test_support;
	use std::fs;
	use std::path::{Path, PathBuf};
	use tempfile::TempDir;
	use tower_lsp::lsp_types::CompletionItemKind;
	use tower_lsp::lsp_types::{DiagnosticSeverity, HoverContents, NumberOrString, Position, Url};

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
		let graph = load_schema_graph(&fixture_dir).expect("load fixture schema graph");
		assert!(
			graph
				.types
				.values()
				.any(|definition| definition.name.as_str() == "event")
		);
		assert!(
			graph
				.types
				.values()
				.any(|definition| definition.name.as_str() == "mission")
		);
	}

	fn lsp_fixture_dir() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("tests")
			.join("fixtures")
			.join("lsp")
	}

	fn load_lsp_hover_graph() -> std::sync::Arc<foch_cwt::CwtSchemaGraph> {
		load_schema_graph(&lsp_fixture_dir().join("schema")).expect("load LSP schema graph")
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

	fn hover_markdown(hover: tower_lsp::lsp_types::Hover) -> String {
		let HoverContents::Markup(markup) = hover.contents else {
			panic!("expected markdown hover contents");
		};
		markup.value
	}

	#[test]
	fn hover_renders_event_field_from_schema() {
		let graph = load_lsp_hover_graph();
		let text = fixture_text("events/sample.txt");
		let hover = schema_hover(
			graph.as_ref(),
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
	fn hover_renders_mission_field_from_schema() {
		let graph = load_lsp_hover_graph();
		let text = fixture_text("missions/sample.txt");
		let hover = schema_hover(
			graph.as_ref(),
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
		let graph = load_lsp_hover_graph();
		let text = fixture_text("common/scripted_effects/sample.txt");
		let hover = schema_hover(
			graph.as_ref(),
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
		let graph = load_lsp_hover_graph();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			graph.as_ref(),
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
		let graph = load_lsp_hover_graph();
		let text = fixture_text("events/sample.txt");
		let candidates = schema_completion_candidates(
			graph.as_ref(),
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
	fn completion_suggests_scripted_effect_fields_from_schema() {
		let graph = load_lsp_hover_graph();
		let text = fixture_text("common/scripted_effects/sample.txt");
		let candidates = schema_completion_candidates(
			graph.as_ref(),
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
	fn diagnostics_report_unknown_keys_and_cardinality_violations() {
		let graph = load_lsp_hover_graph();
		let text = fixture_text("events/diagnostics.txt");
		let diagnostics =
			schema_diagnostics_for_text(graph.as_ref(), Path::new("events/diagnostics.txt"), &text);
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
	}

	#[test]
	fn diagnostics_skip_unknown_keys_inside_alias_bodies() {
		let graph = load_lsp_hover_graph();
		let text = "namespace = sample\ncountry_event = {\n  trigger = {\n    custom_trigger = {\n      mystery_key = yes\n    }\n  }\n}\n";
		let diagnostics =
			schema_diagnostics_for_text(graph.as_ref(), Path::new("events/sample.txt"), text);
		assert!(!diagnostics.iter().any(|diagnostic| {
			diagnostic.code == Some(NumberOrString::String("V001".to_string()))
				&& diagnostic.message.contains("mystery_key")
		}));
	}

	#[test]
	fn workspace_snapshot_includes_schema_diagnostics() {
		let graph = load_lsp_hover_graph();
		let root = lsp_fixture_dir();
		let snapshot = build_workspace_snapshot_with_schema(
			&[ScanTarget {
				path: root.clone(),
				role: TargetRole::Mod,
			}],
			Some(graph),
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
