use foch_core::model::{AnalysisMode, Finding, SemanticIndex, Severity, SymbolKind};
use foch_engine::WorkspaceSession;
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility};
use foch_language::analyzer::eu4_builtin::{
	alias_keywords, builtin_effect_names, builtin_trigger_names, contextual_keywords,
	reserved_keywords,
};
use foch_language::analyzer::parser::{
	AstStatement, AstValue, ScalarValue, parse_clausewitz_content,
};
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, build_semantic_index, parse_script_file,
	resolve_scripted_effect_reference_targets,
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
	DidSaveTextDocumentParams, GotoDefinitionParams, GotoDefinitionResponse, InitializeParams,
	InitializeResult, InitializedParams, Location, MessageType, NumberOrString, OneOf, Position,
	Range, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
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

	async fn refresh_workspace_snapshot(&self) {
		let targets = { self.state.read().await.targets.clone() };
		let client = self.client.clone();
		let built = tokio::task::spawn_blocking(move || build_workspace_snapshot(&targets)).await;
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
		let snapshot = {
			let state = self.state.read().await;
			state.workspace.clone()
		};
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
		diagnostics.sort_by(|lhs, rhs| {
			range_start(&lhs.range)
				.cmp(&range_start(&rhs.range))
				.then_with(|| lhs.message.cmp(&rhs.message))
		});
		diagnostics.dedup_by(|lhs, rhs| {
			lhs.range == rhs.range && lhs.code == rhs.code && lhs.message == rhs.message
		});
		self.client
			.publish_diagnostics(uri.clone(), diagnostics, None)
			.await;
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
				definition_provider: Some(OneOf::Left(true)),
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
			state
				.workspace
				.as_ref()
				.map(|snapshot| snapshot.candidates.as_slice())
				.unwrap_or(&[]),
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

fn build_workspace_snapshot(roots: &[ScanTarget]) -> WorkspaceSnapshot {
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
	let diagnostics_by_path = build_workspace_diagnostics(&index, &path_lookup, &findings);

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
		diagnostics.sort_by(|lhs, rhs| {
			range_start(&lhs.range)
				.cmp(&range_start(&rhs.range))
				.then_with(|| lhs.message.cmp(&rhs.message))
		});
		diagnostics.dedup_by(|lhs, rhs| {
			lhs.range == rhs.range && lhs.code == rhs.code && lhs.message == rhs.message
		});
	}

	diagnostics_by_path
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
			if reference.kind != SymbolKind::ScriptedEffect {
				continue;
			}
			if reference.path != relative_path
				|| reference.line != position.line as usize + 1
				|| reference.column != current_column
				|| reference.name != assignment_key
			{
				continue;
			}
			for target in resolve_scripted_effect_reference_targets(&session.index, reference) {
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
			for definition in session.find_definitions(&token, Some(SymbolKind::Event)) {
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
		SymbolKind::ScriptedTrigger => (
			local_name.to_string(),
			CompletionItemKind::FUNCTION,
			"workspace scripted trigger".to_string(),
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
		assignment_key_on_line, build_workspace_snapshot, detect_completion_context,
		extract_completion_prefix, parse_scan_targets_json, resolve_definition_locations,
		select_completion_candidates,
	};
	use std::fs;
	use tempfile::TempDir;
	use tower_lsp::lsp_types::CompletionItemKind;
	use tower_lsp::lsp_types::{Position, Url};

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

	#[test]
	fn definition_resolves_flag_value_to_setter() {
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
}
