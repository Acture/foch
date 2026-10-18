pub mod definition_module;
pub mod documents;
pub(crate) mod emit;
mod extractors;
pub mod localisation;
pub mod parse_cache;
pub mod parser;

pub use self::emit::emit_native_statements;
pub(crate) use self::extractors::is_clausewitz_date_key;

use self::localisation::collect_localisation_definitions_from_root;
use self::parser::{AstFile, AstStatement, AstValue, ParseResult, SpanRange, read_failure};
use super::base::builtin::{is_contextual_keyword, is_reserved_keyword};
use super::content::{
	ContentFamilyDescriptor, ContentFamilyScopePolicy, ScriptFileKind, eu4,
	module_name_for_descriptor,
};
use super::cwt::{
	Eu4Dialect, is_country_file_reference, is_country_tag_text, is_province_id_text,
	is_schema_effect, is_schema_trigger,
};
use crate::game::eu4::editor::schema::{complex_enum_file_values, schema_definitions};
use crate::game::schema::bind::{
	BoundContext, DefinitionKeyTemplate, SCRIPT_PARAMETER_KEY_ENUM, SchemaBinder, ScopeKeyword,
	StatementBinding, alias_wildcard, conditions_match, rules_admit,
};
use crate::game::schema::query::{
	CompiledAlias, CompiledAliasCategory, CompiledBindFieldMatch, CompiledRoot, CompiledRuleValue,
	CwtQuery, RuleContext,
};
use crate::model::{
	AliasUsage, DocumentFamily, DocumentRecord, GamePath, GamePathBuf, KeyUsage,
	LocalisationDefinition, MaybeScope, ParamBinding, ParseIssue, ResourceReference,
	ScalarAssignment, ScopeKind, ScopeNode, ScopeSet, SemanticIndex, SourceSpan, SymbolDefinition,
	SymbolKind, SymbolReference, UiDefinition, base_scope, has_fatal_parse_issue, lookup_scope,
};
use rayon::prelude::*;
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct ParsedScriptFile {
	pub mod_id: String,
	/// The file the script was read from, or `None` for a script that exists
	/// only in memory, such as a folded definition module.
	pub path: Option<PathBuf>,
	/// The game path the script is loaded at, which identifies it to content
	/// families, rules and every index built from it.
	pub relative_path: GamePathBuf,
	pub content_family: Option<&'static ContentFamilyDescriptor>,
	pub file_kind: ScriptFileKind,
	pub module_name: String,
	pub ast: AstFile,
	pub source: String,
	pub parse_issues: Vec<ParseIssue>,
	pub parse_cache_hit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedScriptInputIdentity {
	pub size_bytes: u64,
	pub content_digest: String,
}

pub(super) struct ParsedScriptWithInputIdentity {
	pub file: ParsedScriptFile,
	pub input_identity: Option<ParsedScriptInputIdentity>,
}

use parse_cache::{parse_clausewitz_bytes_cached, parse_clausewitz_for_path};

pub fn classify_script_file(relative: &GamePath) -> ScriptFileKind {
	eu4()
		.classify_content_family(relative)
		.map_or(ScriptFileKind::new("other"), |descriptor| {
			descriptor.script_file_kind.clone()
		})
}

/// The module of a file no content family names: its parent directories
/// joined with `.`, or `other` directly under the root.
fn fallback_module_name_from_relative(relative: &GamePath) -> String {
	relative.parent().map_or_else(
		|| "other".to_string(),
		|parent| parent.iter().collect::<Vec<_>>().join("."),
	)
}

fn qualify_symbol_name(module: &str, local: &str) -> String {
	format!("eu4::{module}::{local}")
}

/// Reads and parses the script at `relative` under `root`. A read failure is
/// reported as a parse issue of that script.
pub fn parse_script_file(mod_id: &str, root: &Path, relative: &GamePath) -> ParsedScriptFile {
	parse_script_file_with_cache_and_input_identity(mod_id, root, relative, true).file
}

pub(super) fn parse_script_file_with_input_identity(
	mod_id: &str,
	root: &Path,
	relative: &GamePath,
) -> ParsedScriptWithInputIdentity {
	parse_script_file_with_cache_and_input_identity(mod_id, root, relative, true)
}

pub(super) fn parse_script_file_without_cache_with_input_identity(
	mod_id: &str,
	root: &Path,
	relative: &GamePath,
) -> ParsedScriptWithInputIdentity {
	parse_script_file_with_cache_and_input_identity(mod_id, root, relative, false)
}

/// Builds an AST-only parsed script from caller-supplied bytes of the script
/// at `relative` under `root`. The file is never reopened and raw source text
/// is not retained.
pub fn parse_script_bytes_cached(
	mod_id: &str,
	root: &Path,
	relative: &GamePath,
	bytes: &[u8],
) -> ParsedScriptFile {
	let (parsed, parse_cache_hit) = parse_clausewitz_bytes_cached(relative, bytes);
	parsed_script_file_from_result(
		mod_id,
		Some(relative.to_path(root)),
		parsed,
		parse_cache_hit,
		String::new(),
	)
}

fn parse_script_file_with_cache_and_input_identity(
	mod_id: &str,
	root: &Path,
	relative: &GamePath,
	use_cache: bool,
) -> ParsedScriptWithInputIdentity {
	let file = relative.to_path(root);
	let (parsed, parse_cache_hit, source, input_identity) = match std::fs::read(&file) {
		Ok(bytes) => {
			let content_digest = blake3::hash(&bytes).to_hex().to_string();
			let source = crate::game::eu4::text::decode_paradox_bytes(&bytes).into_owned();
			let (parsed, parse_cache_hit) = if use_cache {
				parse_clausewitz_bytes_cached(relative, &bytes)
			} else {
				(parse_clausewitz_for_path(relative, &source), false)
			};
			(
				parsed,
				parse_cache_hit,
				source,
				Some(ParsedScriptInputIdentity {
					size_bytes: bytes.len() as u64,
					content_digest,
				}),
			)
		}
		Err(err) => (
			read_failure(&err).into_parse_result(relative.to_owned()),
			false,
			String::new(),
			None,
		),
	};
	ParsedScriptWithInputIdentity {
		file: parsed_script_file_from_result(mod_id, Some(file), parsed, parse_cache_hit, source),
		input_identity,
	}
}

/// Semantic processing identifies content and resolves CWT rules by game
/// path, which `parsed` carries in its AST; `path` is only where the source
/// was read from.
fn parsed_script_file_from_result(
	mod_id: &str,
	path: Option<PathBuf>,
	parsed: ParseResult,
	parse_cache_hit: bool,
	source: String,
) -> ParsedScriptFile {
	let relative = parsed.ast.path.clone();
	let content_family = eu4().classify_content_family(&relative);
	let file_kind = content_family.map_or(ScriptFileKind::new("other"), |descriptor| {
		descriptor.script_file_kind.clone()
	});
	let module_name = content_family.map_or_else(
		|| fallback_module_name_from_relative(&relative),
		|descriptor| module_name_for_descriptor(&relative, descriptor).replace('-', "_"),
	);
	let parse_issues = parsed
		.diagnostics
		.into_iter()
		.map(|item| ParseIssue {
			mod_id: mod_id.to_string(),
			path: relative.clone(),
			line: item.span.start.line,
			column: item.span.start.column,
			message: item.message,
			repair: item.repair,
			isolation: item.isolation,
		})
		.collect();

	ParsedScriptFile {
		mod_id: mod_id.to_string(),
		path,
		relative_path: relative,
		content_family,
		file_kind,
		module_name,
		ast: parsed.ast,
		source,
		parse_issues,
		parse_cache_hit,
	}
}

pub fn collect_localisation_definitions(
	mod_id: &str,
	root: &Path,
) -> std::io::Result<Vec<LocalisationDefinition>> {
	collect_localisation_definitions_from_root(mod_id, root)
}

pub fn build_semantic_index(files: &[ParsedScriptFile]) -> SemanticIndex {
	let declared = schema_declared_definitions(files);
	// Files index independently: scope ids are positions within the index
	// being built, and appending in file order moves them where a sequential
	// build would have put them.
	let file_indexes =
		files
			.par_iter()
			.map(|file| {
				let mut index = SemanticIndex::default();
				index.documents.push(DocumentRecord {
					mod_id: file.mod_id.clone(),
					path: file.relative_path.clone(),
					family: DocumentFamily::Clausewitz,
					parse_ok: !has_fatal_parse_issue(&file.parse_issues),
				});
				index.parse_issues.extend(file.parse_issues.clone());
				build_file_index(file, &declared, &mut index);
				index
					.resource_references
					.extend(schema_key_definitions(file, super::cwt::rule_engine()));
				let cultures =
					super::cultures::collect_culture_references(&file.ast, &file.relative_path);
				index
					.resource_references
					.extend(
						cultures
							.references
							.into_iter()
							.map(|reference| ResourceReference {
								key: "culture_reference".to_owned(),
								value: reference.id,
								mod_id: file.mod_id.clone(),
								path: reference.location.relative_path,
								line: reference.location.span.start.line,
								column: reference.location.span.start.column,
							}),
					);
				index
					.resource_references
					.extend(cultures.group_references.into_iter().map(|reference| {
						ResourceReference {
							key: "culture_group_reference".to_owned(),
							value: reference.group,
							mod_id: file.mod_id.clone(),
							path: reference.location.relative_path,
							line: reference.location.span.start.line,
							column: reference.location.span.start.column,
						}
					}));
				index
			})
			.collect::<Vec<_>>();
	let mut index = SemanticIndex::default();
	for file_index in file_indexes {
		index.append(file_index);
	}
	link_schema_declared_calls(&mut index);
	refine_named_scope_switches(&mut index);
	infer_definition_scope_from_references(&mut index);
	infer_definition_dynamic_alias_masks_from_references(&mut index);
	index
}

/// Key name, before `:` and the scope, for a definition whose name switches
/// to that scope where it is written as a key (`named_scope:country` for a
/// country tag).
pub(crate) const NAMED_SCOPE_DEFINITION: &str = "named_scope";

/// Gives each block a name opens where it switches scope (`FRA = { ... }`,
/// `finland_area = { ... }`) the scope the set holding that name switches to,
/// once the definitions are known, and the same scope to every block below
/// it that runs in its parent's. A name two sets with different scopes hold
/// (a trade node is named by its location's province) stays unknown.
pub fn refine_named_scope_switches(index: &mut SemanticIndex) {
	let mut named = HashMap::<String, Option<MaybeScope>>::new();
	for reference in &index.resource_references {
		let Some(scope) = reference
			.key
			.strip_prefix(NAMED_SCOPE_DEFINITION)
			.and_then(|rest| rest.strip_prefix(':'))
		else {
			continue;
		};
		let scope = lookup_scope(scope).map_or(MaybeScope::Unknown, MaybeScope::Known);
		named
			.entry(reference.value.clone())
			.and_modify(|known| {
				if *known != Some(scope) {
					*known = None;
				}
			})
			.or_insert(Some(scope));
	}
	if named.is_empty() {
		return;
	}
	let mut opened = HashMap::<(usize, &str), Vec<usize>>::new();
	for scope in &index.scopes {
		if let Some(parent) = scope.parent {
			opened
				.entry((parent, scope.key.as_str()))
				.or_default()
				.push(scope.id);
		}
	}
	let mut switches = Vec::new();
	for reference in &index.references {
		if reference.schema_alias
			|| !matches!(
				reference.kind,
				SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger
			) {
			continue;
		}
		let Some(Some(MaybeScope::Known(scope_type))) = named.get(&reference.name) else {
			continue;
		};
		let Some(block) = opened
			.get(&(reference.scope_id, reference.name.as_str()))
			.and_then(|blocks| {
				blocks
					.iter()
					.copied()
					.filter(|block| index.scopes[*block].span.line >= reference.line)
					.min_by_key(|block| index.scopes[*block].span.line)
			})
		else {
			continue;
		};
		let scope = &index.scopes[block];
		if scope.this_type.is_unknown()
			&& matches!(scope.kind, ScopeKind::Effect | ScopeKind::Trigger)
		{
			switches.push((block, *scope_type));
		}
	}
	let mut refined = vec![false; index.scopes.len()];
	for (block, scope_type) in switches {
		let scope = &mut index.scopes[block];
		scope.this_type = scope_type.into();
		scope.aliases.insert("THIS".to_string(), scope_type.into());
		refined[block] = true;
	}
	// Scopes are numbered parents first, so one pass reaches every block
	// below a refined one.
	for id in 0..index.scopes.len() {
		let Some(parent) = index.scopes[id].parent else {
			continue;
		};
		if !refined[parent] {
			continue;
		}
		let parent_this = index.scopes[parent].this_type;
		let scope = &mut index.scopes[id];
		if scope
			.aliases
			.get("PREV")
			.is_none_or(|prev| prev.is_unknown())
		{
			scope.aliases.insert("PREV".to_string(), parent_this);
		}
		if scope.inherits_this && scope.this_type.is_unknown() {
			scope.this_type = parent_this;
			scope.aliases.insert("THIS".to_string(), parent_this);
			refined[id] = true;
		}
	}
}

/// Key name for a definition whose name the schema makes a trigger key.
pub(crate) const TRIGGER_KEY_DEFINITION: &str = "trigger_key_definition";
/// Key name for a definition whose name the schema makes an effect key.
pub(crate) const EFFECT_KEY_DEFINITION: &str = "effect_key_definition";

/// The names in `file` that are trigger or effect keys, such as `tin = 1`
/// for a trade good a mod adds, or `FRA = { ... }` for a tag. Each file
/// records its own, so the game's names come with the base snapshot and a
/// mod's with that mod.
fn schema_key_definitions(file: &ParsedScriptFile, engine: &CwtQuery) -> Vec<ResourceReference> {
	let schema_path = schema_path(file);
	let mut references = Vec::new();
	let mut push = |set: &str, name: String, line: usize, column: usize| {
		let (trigger, effect) = super::cwt::schema_names_are_keys(set);
		let keys = [
			trigger.then(|| TRIGGER_KEY_DEFINITION.to_owned()),
			effect.then(|| EFFECT_KEY_DEFINITION.to_owned()),
			super::cwt::schema_names_push_scope(set)
				.map(|scope| format!("{NAMED_SCOPE_DEFINITION}:{scope}")),
		];
		for key in keys.into_iter().flatten() {
			references.push(ResourceReference {
				key,
				value: name.clone(),
				mod_id: file.mod_id.clone(),
				path: file.relative_path.clone(),
				line,
				column,
			});
		}
	};
	for root in engine.matching_roots(&schema_path) {
		if super::cwt::schema_names_are_keys(&root.name) == (false, false) {
			continue;
		}
		for (name, range) in schema_definitions(root, &file.ast.statements, &schema_path) {
			push(
				&root.name,
				name,
				range.start.line as usize + 1,
				range.start.character as usize + 1,
			);
		}
	}
	for complex_enum in engine
		.complex_enums()
		.iter()
		.filter(|complex_enum| complex_enum.matches(&schema_path))
	{
		let set = format!("enum[{}]", complex_enum.name);
		if super::cwt::schema_names_are_keys(&set) == (false, false) {
			continue;
		}
		for (name, span) in complex_enum_file_values(complex_enum, &file.ast.statements) {
			push(&set, name, span.start.line, span.start.column);
		}
	}
	references
}

/// What a block a path names holds in the active EU4 schema: effects,
/// triggers or neither.
pub fn script_container_scope_kind(file_path: &GamePath, ast_path: &[&str]) -> Option<ScopeKind> {
	let binder = SchemaBinder::new(super::cwt::rule_engine(), &Eu4Dialect, None, file_path);
	let rules = binder.context_for_path::<AstStatement>(&[], ast_path)?;
	Some(rules_scope_kind(rules, &HashSet::new()))
}

/// The kind of a block by what its rules accept where they name no key:
/// effects, triggers, or data.
fn rules_scope_kind(rules: RuleContext<'_>, subtypes: &HashSet<String>) -> ScopeKind {
	if rules_admit(rules, subtypes, &CompiledAliasCategory::Effect) {
		ScopeKind::Effect
	} else if rules_admit(rules, subtypes, &CompiledAliasCategory::Trigger) {
		ScopeKind::Trigger
	} else {
		ScopeKind::Block
	}
}

/// The game path whose rules read `file`.
fn schema_path(file: &ParsedScriptFile) -> GamePathBuf {
	file.content_family.map_or_else(
		|| file.relative_path.clone(),
		|descriptor| descriptor.schema_path(&file.relative_path),
	)
}

/// The scripted effects and triggers `files` define under names the schema
/// declares as the engine's own (EU4's rule files declare some of the game's
/// scripted effects that way). A call of one in the same files binds as the
/// engine's, yet is recorded as a call, with what it passes.
fn schema_declared_definitions(files: &[ParsedScriptFile]) -> HashSet<(SymbolKind, String)> {
	let engine = super::cwt::rule_engine();
	let mut declared = HashSet::new();
	for file in files {
		let path = schema_path(file);
		for root in engine.matching_roots(&path) {
			let kind = match root.name.as_str() {
				"scripted_effect" => SymbolKind::ScriptedEffect,
				"scripted_trigger" => SymbolKind::ScriptedTrigger,
				_ => continue,
			};
			for (name, _) in schema_definitions(root, &file.ast.statements, &path) {
				let schema_declared = match kind {
					SymbolKind::ScriptedEffect => is_schema_effect(&name),
					_ => is_schema_trigger(&name),
				};
				if schema_declared {
					declared.insert((kind, name));
				}
			}
		}
	}
	declared
}

fn build_file_index(
	file: &ParsedScriptFile,
	declared: &HashSet<(SymbolKind, String)>,
	index: &mut SemanticIndex,
) {
	let mut aliases = BTreeMap::new();
	let scope_policy = file
		.content_family
		.map_or(ContentFamilyScopePolicy::default(), |descriptor| {
			descriptor.scope_policy
		});
	let root_this_type = scope_policy.root_scope;
	aliases.insert("THIS".to_string(), root_this_type);
	aliases.insert("ROOT".to_string(), root_this_type);
	if let Some(from_alias) = scope_policy.from_alias {
		aliases.insert("FROM".to_string(), from_alias.into());
	}

	let root_scope = push_scope(ScopeArgs {
		index,
		kind: ScopeKind::File,
		parent: None,
		this_type: aliases.get("THIS").copied().unwrap_or(MaybeScope::Unknown),
		aliases,
		mod_id: &file.mod_id,
		path: &file.relative_path,
		line: line_from_stmt(file.ast.statements.first()),
		key: "",
		inherits_this: false,
	});

	let schema_path = schema_path(file);
	let binder = SchemaBinder::new(super::cwt::rule_engine(), &Eu4Dialect, None, &schema_path);
	let bound = binder.file(&file.ast.statements);
	let mut ctx = BuildContext {
		mod_id: &file.mod_id,
		path: &file.relative_path,
		content_family: file.content_family,
		file_kind: file.file_kind.clone(),
		module_name: &file.module_name,
		source: &file.source,
		binder: &binder,
		declared,
		technology_monarch_power: None,
		technology_definition_ordinal: 0,
		random_map_tile_emitted: false,
		random_name_table_emitted: false,
	};

	walk_statements(
		&file.ast.statements,
		index,
		root_scope,
		&mut ctx,
		&bound,
		None,
		None,
	);
}

struct BuildContext<'a> {
	mod_id: &'a str,
	path: &'a GamePath,
	content_family: Option<&'static ContentFamilyDescriptor>,
	file_kind: ScriptFileKind,
	module_name: &'a str,
	source: &'a str,
	binder: &'a SchemaBinder<'a>,
	/// Scripted definitions the indexed files make under names the schema
	/// declares (see [`schema_declared_definitions`]).
	declared: &'a HashSet<(SymbolKind, String)>,
	technology_monarch_power: Option<String>,
	technology_definition_ordinal: usize,
	random_map_tile_emitted: bool,
	random_name_table_emitted: bool,
}

/// How a block a key opens where effects or triggers are written takes it,
/// when no rule names the key: a call of a scripted effect or trigger, with
/// its parameters, or a switch to the scope the key names (a country tag, an
/// area, a province).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallBody {
	Parameters,
	ScopeSwitch,
}

fn walk_statements<'a>(
	statements: &'a [AstStatement],
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &mut BuildContext<'a>,
	bound: &BoundContext<'a, AstStatement>,
	active_scripted_effect: Option<usize>,
	namespace: Option<String>,
) {
	let mut current_namespace = namespace;

	for statement in statements {
		match statement {
			AstStatement::Assignment {
				key,
				key_span,
				value,
				..
			} => {
				let binding = ctx.binder.bind(bound, key, value);
				let call = call_kind(ctx.binder, bound, key, &binding);
				// A parameter written as a key (`$scope$ = { ... }`) stands for a
				// name its call substitutes, so its block is read as such a
				// name's would be.
				let dynamic = call.or_else(|| {
					ctx.binder
						.dialect
						.is_parameter_key(key)
						.then(|| position_kind(bound))
						.flatten()
				});
				let call_choice = match (dynamic, value) {
					(Some(kind), AstValue::Block { .. }) => {
						dynamic_call_binding(ctx.binder, bound, kind, value)
					}
					_ => None,
				};
				let call_body = call_choice.as_ref().map(|(_, body)| *body);
				let child = match value {
					AstValue::Block { .. } => Some(
						ctx.binder.child(
							bound,
							statement,
							key,
							call_choice
								.as_ref()
								.map_or(&binding, |(call_binding, _)| call_binding),
						),
					),
					AstValue::Scalar { .. } => None,
				};
				let definition = child.as_ref().and_then(definition_root);
				let is_event_definition = definition.is_some_and(|root| root.name == "event");

				record_foundation_resource_semantics(index, scope_id, ctx, key, key_span, value);
				if !is_event_definition {
					record_key_usage(index, scope_id, ctx, key, key_span);
					record_scalar_assignment(index, scope_id, ctx, key, key_span, value);
					record_ui_scalar_semantics(index, ctx, key, key_span, value);

					if key == "namespace"
						&& let Some(value_text) = scalar_text(value)
					{
						current_namespace = Some(value_text);
					}

					if let Some(alias) = scope_alias_key(ctx.binder, key)
						&& !matches!(binding.matched, Some(CompiledBindFieldMatch::Field(_)))
					{
						record_alias_usage(index, scope_id, ctx, alias, key_span);
					}

					record_alias_tokens_from_value(index, scope_id, ctx, value);
					record_param_tokens(index, active_scripted_effect, value);
				}

				if let Some(text) = scalar_text(value).filter(|_| {
					binding
						.matched
						.is_some_and(|field_match| names_localisation(field_match.value()))
				}) {
					push_resource_reference(index, ctx, key_span, LOCALISATION_REFERENCE, &text);
				}
				if let Some(event_id) = scalar_text(value).filter(|_| {
					binding
						.matched
						.is_some_and(|field_match| names_event(field_match.value()))
				}) {
					push_symbol_reference(
						index,
						ctx,
						SymbolKind::Event,
						event_id,
						key_span,
						scope_id,
						ProvidedParams::default(),
					);
				}

				let declared_call = call
					.is_none()
					.then(|| position_kind(bound))
					.flatten()
					.filter(|kind| {
						binding
							.matched
							.is_some_and(|field_match| field_match.alias().is_some())
							&& ctx.declared.contains(&(*kind, key.clone()))
					});
				if let Some(kind) = call.or(declared_call) {
					let provided = match value {
						AstValue::Block { items, .. } => collect_provided_params(items),
						AstValue::Scalar { .. } => ProvidedParams::default(),
					};
					let index_before = index.references.len();
					push_symbol_reference(
						index,
						ctx,
						kind,
						key.clone(),
						key_span,
						scope_id,
						provided,
					);
					if declared_call.is_some() {
						index.references[index_before].schema_alias = true;
					}
				}

				let (Some(child), AstValue::Block { items, span }) = (child, value) else {
					continue;
				};
				record_ui_block_semantics(index, ctx, key, key_span, items);
				let child_scope = create_child_scope(ChildScopeArgs {
					index,
					parent_scope_id: scope_id,
					ctx,
					key,
					span,
					bound,
					child: &child,
					definition,
					call_body,
					is_field: matches!(binding.matched, Some(CompiledBindFieldMatch::Field(_))),
				});
				let mut next_scripted_effect = active_scripted_effect;
				if let Some(root) = definition {
					for text in required_localisation(ctx, root, &child, key, items) {
						push_resource_reference(
							index,
							ctx,
							key_span,
							LOCALISATION_REFERENCE,
							&text,
						);
					}
					if is_event_definition {
						push_event_definition(
							index,
							ctx,
							key,
							span,
							items,
							child_scope,
							current_namespace.as_deref(),
						);
						next_scripted_effect = None;
					} else if let Some(kind) = definition_symbol_kind(root) {
						let definition_index = push_symbol_definition(
							index,
							ctx,
							kind,
							key,
							key_span,
							span,
							items,
							child_scope,
						);
						if kind == SymbolKind::ScriptedEffect {
							next_scripted_effect = Some(definition_index);
						}
					}
				}
				walk_statements(
					items,
					index,
					child_scope,
					ctx,
					&child,
					next_scripted_effect,
					current_namespace.clone(),
				);
			}
			AstStatement::Item { value, span } => {
				record_alias_tokens_from_value(index, scope_id, ctx, value);
				record_param_tokens(index, active_scripted_effect, value);
				if let Some(event_id) = scalar_text(value).filter(|_| {
					ctx.binder
						.bind_item(bound, value)
						.is_some_and(|field| names_event(&field.value))
				}) {
					push_symbol_reference(
						index,
						ctx,
						SymbolKind::Event,
						event_id,
						span,
						scope_id,
						ProvidedParams::default(),
					);
				}
				if let AstValue::Block { items, span } = value {
					let this_type = scope_this_type(index, scope_id);
					let aliases = scope_aliases(index, scope_id);
					let child_scope = push_scope(ScopeArgs {
						index,
						kind: ScopeKind::Block,
						parent: Some(scope_id),
						this_type,
						aliases,
						mod_id: ctx.mod_id,
						path: ctx.path,
						line: span.start.line,
						key: "",
						inherits_this: true,
					});
					walk_statements(
						items,
						index,
						child_scope,
						ctx,
						bound,
						active_scripted_effect,
						current_namespace.clone(),
					);
				}
			}
			AstStatement::Comment { .. } => {}
		}
	}
}

/// The kind of call a key makes: in a block whose rules accept effects or
/// triggers, a key no rule names, which a scripted definition, or a name a
/// definition gives, must answer for. A parameter of the definition being
/// read (`$name$`), and a key an optional block opens (`[[name]key`), are
/// known only once a call substitutes them.
fn call_kind(
	binder: &SchemaBinder<'_>,
	bound: &BoundContext<'_, AstStatement>,
	key: &str,
	binding: &StatementBinding<'_>,
) -> Option<SymbolKind> {
	if binding.matched.is_some() || binder.dialect.holds_parameter(key) || key.contains('[') {
		return None;
	}
	position_kind(bound)
}

/// What a block's rules accept where they name no key: effects, triggers, or
/// neither.
fn position_kind(bound: &BoundContext<'_, AstStatement>) -> Option<SymbolKind> {
	if bound.admits(&CompiledAliasCategory::Effect) {
		Some(SymbolKind::ScriptedEffect)
	} else if bound.admits(&CompiledAliasCategory::Trigger) {
		Some(SymbolKind::ScriptedTrigger)
	} else {
		None
	}
}

/// The body a block call takes, chosen as any key's form is: among the sets
/// of names the schema accepts where the call is written (a country tag's,
/// an area's, a scripted effect's parameters), the one whose rules reject
/// the fewest of the block's statements. A scope switch wins a tie.
fn dynamic_call_binding<'a>(
	binder: &SchemaBinder<'a>,
	bound: &BoundContext<'a, AstStatement>,
	kind: SymbolKind,
	value: &AstValue,
) -> Option<(StatementBinding<'a>, CallBody)> {
	let category = call_category(kind);
	let wildcard = alias_wildcard(bound.rules()?, bound.subtypes(), &category)?;
	let (calls, switches): (Vec<_>, Vec<_>) = binder
		.engine
		.dynamic_aliases(&category)
		.filter(|alias| {
			matches!(alias.value, CompiledRuleValue::Block(_))
				&& alias.name != SCRIPT_PARAMETER_KEY_ENUM
		})
		.map(|alias| CompiledBindFieldMatch::Alias { wildcard, alias })
		.partition(|field_match| field_match.alias().is_some_and(is_scripted_call_body));
	// Many sets switch with the same rules (any area's, any region's); each
	// body is weighed once.
	let mut alternatives = Vec::<CompiledBindFieldMatch<'a>>::new();
	for candidate in switches.into_iter().chain(calls) {
		let same_body = |other: &CompiledBindFieldMatch<'a>| {
			other.alias().map(|alias| &alias.rules) == candidate.alias().map(|alias| &alias.rules)
				&& other.alias().map(|alias| &alias.attributes.scope)
					== candidate.alias().map(|alias| &alias.attributes.scope)
		};
		if !alternatives.iter().any(same_body) {
			alternatives.push(candidate);
		}
	}
	let matched =
		binder.select_alternative(&alternatives, value, bound.subtypes(), bound.scopes())?;
	let body = if matched.alias().is_some_and(is_scripted_call_body) {
		CallBody::Parameters
	} else {
		CallBody::ScopeSwitch
	};
	Some((
		StatementBinding {
			alternatives: vec![matched],
			matched: Some(matched),
			block_form_count: 1,
		},
		body,
	))
}

/// Whether an alias is the body of a call of a scripted effect or trigger
/// (`<scripted_effect>`).
fn is_scripted_call_body(alias: &CompiledAlias) -> bool {
	DefinitionKeyTemplate::parse(&alias.name).is_some_and(|template| {
		matches!(template.type_name, "scripted_effect" | "scripted_trigger")
	})
}

fn call_category(kind: SymbolKind) -> CompiledAliasCategory {
	match kind {
		SymbolKind::ScriptedTrigger => CompiledAliasCategory::Trigger,
		_ => CompiledAliasCategory::Effect,
	}
}

/// The type a block defines an instance of: what a top-level key, below any
/// wrappers, binds to.
fn definition_root<'a>(child: &BoundContext<'a, AstStatement>) -> Option<&'a CompiledRoot> {
	match child.rules()? {
		RuleContext::RootType(root) | RuleContext::Subtype(root, _) => Some(root),
		RuleContext::RuleField(_) | RuleContext::AliasRules(_) => None,
	}
}

/// The symbol a definition of a schema type is indexed as.
fn definition_symbol_kind(root: &CompiledRoot) -> Option<SymbolKind> {
	match root.name.as_str() {
		"scripted_effect" => Some(SymbolKind::ScriptedEffect),
		"scripted_trigger" => Some(SymbolKind::ScriptedTrigger),
		"decision" => Some(SymbolKind::Decision),
		"diplomatic_action" | "new_diplomatic_action" => Some(SymbolKind::DiplomaticAction),
		"triggered_modifier" => Some(SymbolKind::TriggeredModifier),
		_ => None,
	}
}

/// Key name for a localisation key the script needs: a value the rules type
/// as one, or a key a definition's type requires (`$_title`).
pub(crate) const LOCALISATION_REFERENCE: &str = "localisation_reference";

/// Whether a rule value is a localisation key. Synced localisation (a
/// ruler's or leader's name) may be the text itself, as inline localisation
/// is.
fn names_localisation(value: &CompiledRuleValue) -> bool {
	matches!(
		value,
		CompiledRuleValue::Scalar(text) | CompiledRuleValue::Marker(text)
			if matches!(text.as_str(), "localisation" | "localization")
	)
}

/// The localisation keys a definition's type requires of it: each required
/// pattern of the type's `localisation` rules that holds for its subtypes,
/// with the definition's name for `$`.
fn required_localisation(
	ctx: &BuildContext<'_>,
	root: &CompiledRoot,
	child: &BoundContext<'_, AstStatement>,
	key: &str,
	items: &[AstStatement],
) -> Vec<String> {
	if root.localisation.is_empty() {
		return Vec::new();
	}
	let identity = root
		.name_field
		.as_deref()
		.and_then(|field| extract_assignment_scalar(items, field))
		.or_else(|| {
			root.name_from_file
				.then(|| ctx.path.file_stem().map(str::to_string))
				.flatten()
		})
		.unwrap_or_else(|| key.to_string());
	root.localisation
		.iter()
		.filter(|rule| {
			rule.attributes
				.raw
				.iter()
				.any(|(name, value)| name == "required" && value.is_empty())
				&& conditions_match(&rule.conditions, child.subtypes())
		})
		.filter_map(|rule| match &rule.value {
			CompiledRuleValue::Scalar(pattern) | CompiledRuleValue::Marker(pattern)
				if pattern.contains('$') =>
			{
				Some(pattern.replace('$', &identity))
			}
			_ => None,
		})
		.collect()
}

/// Whether a rule value is an event's name (`<event.country>`).
fn names_event(value: &CompiledRuleValue) -> bool {
	matches!(
		value,
		CompiledRuleValue::Scalar(text) | CompiledRuleValue::Marker(text)
			if text.strip_prefix('<').is_some_and(|rest| {
				rest.strip_prefix("event").is_some_and(|rest| rest.starts_with(['>', '.']))
			})
	)
}

/// The scope alias a key names (`ROOT = { ... }`), as the index spells it.
fn scope_alias_key(binder: &SchemaBinder<'_>, key: &str) -> Option<&'static str> {
	match binder.dialect.scope_keyword(key)? {
		ScopeKeyword::This => Some("THIS"),
		ScopeKeyword::Root => Some("ROOT"),
		ScopeKeyword::From => Some("FROM"),
		ScopeKeyword::Prev => Some("PREV"),
		_ => None,
	}
}

fn push_symbol_reference(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	kind: SymbolKind,
	name: String,
	span: &SpanRange,
	scope_id: usize,
	provided: ProvidedParams,
) {
	let ProvidedParams {
		mut names,
		mut bindings,
	} = provided;
	names.sort();
	names.dedup();
	bindings.sort_by(|lhs, rhs| {
		(lhs.name.as_str(), lhs.value.as_str()).cmp(&(rhs.name.as_str(), rhs.value.as_str()))
	});
	bindings.dedup_by(|lhs, rhs| lhs.name == rhs.name && lhs.value == rhs.value);
	index.references.push(SymbolReference {
		kind,
		name,
		module: ctx.module_name.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: span.start.line,
		column: span.start.column,
		scope_id,
		provided_params: names,
		param_bindings: bindings,
		schema_alias: false,
	});
}

#[allow(clippy::too_many_arguments)]
fn push_symbol_definition(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	kind: SymbolKind,
	key: &str,
	key_span: &SpanRange,
	span: &SpanRange,
	items: &[AstStatement],
	child_scope: usize,
) -> usize {
	let optional_params =
		collect_optional_params_from_source(ctx.source, span.start.offset, span.end.offset);
	let optional_set: HashSet<&str> = optional_params.iter().map(String::as_str).collect();
	let mut required_params: Vec<String> = collect_required_params(items)
		.into_iter()
		.filter(|param| !optional_set.contains(param.as_str()))
		.collect();
	required_params.sort();
	required_params.dedup();
	index.definitions.push(SymbolDefinition {
		kind,
		name: qualify_symbol_name(ctx.module_name, key),
		module: ctx.module_name.to_string(),
		local_name: key.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: key_span.start.line,
		column: key_span.start.column,
		scope_id: child_scope,
		declared_this_type: scope_this_type(index, child_scope),
		inferred_this_type: MaybeScope::Unknown,
		inferred_this_mask: ScopeSet::EMPTY,
		inferred_from_mask: ScopeSet::EMPTY,
		inferred_root_mask: ScopeSet::EMPTY,
		required_params,
		optional_params,
		scope_param_names: collect_scope_param_names(items),
	});
	index.definitions.len() - 1
}

/// An event, named by its `id` in the file's namespace.
fn push_event_definition(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	span: &SpanRange,
	items: &[AstStatement],
	event_scope: usize,
	namespace: Option<&str>,
) {
	let Some(id) = extract_assignment_scalar(items, "id") else {
		return;
	};
	let full_id = match namespace {
		Some(namespace) if !id.contains('.') => format!("{namespace}.{id}"),
		_ => id,
	};
	let this_type = scope_this_type(index, event_scope);
	index.definitions.push(SymbolDefinition {
		kind: SymbolKind::Event,
		name: full_id,
		module: ctx.module_name.to_string(),
		local_name: key.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: span.start.line,
		column: span.start.column,
		scope_id: event_scope,
		declared_this_type: this_type,
		inferred_this_type: this_type,
		inferred_this_mask: scope_mask(this_type),
		inferred_from_mask: ScopeSet::EMPTY,
		inferred_root_mask: ScopeSet::EMPTY,
		required_params: Vec::new(),
		optional_params: Vec::new(),
		scope_param_names: Vec::new(),
	});
}

fn record_key_usage(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
) {
	index.key_usages.push(KeyUsage {
		key: key.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: key_span.start.line,
		column: key_span.start.column,
		scope_id,
		this_type: scope_this_type(index, scope_id),
	});
}

fn record_scalar_assignment(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let AstValue::Scalar { value, .. } = value else {
		return;
	};

	index.scalar_assignments.push(ScalarAssignment {
		key: key.to_string(),
		value: value.as_text(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: key_span.start.line,
		column: key_span.start.column,
		scope_id,
	});
}

fn record_ui_scalar_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	if ctx.file_kind != ScriptFileKind::new("ui") {
		return;
	}
	let Some(text) = scalar_text(value) else {
		return;
	};

	if key == "name" && is_ui_identifier_candidate(&text) {
		index.ui_definitions.push(UiDefinition {
			name: text.clone(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_owned(),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}

	if is_ui_resource_key(key) {
		index.resource_references.push(ResourceReference {
			key: key.to_string(),
			value: text,
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_owned(),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}
}

fn record_foundation_resource_semantics(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &mut BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	value: &AstValue,
) {
	let Some(descriptor) = ctx.content_family else {
		return;
	};
	if let Some(extractor) = extractors::extractor_for(descriptor) {
		extractor.extract(index, scope_id, ctx, key, key_span, value);
	}
}

fn push_resource_reference(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key_span: &SpanRange,
	key: &str,
	value: &str,
) {
	index.resource_references.push(ResourceReference {
		key: key.to_string(),
		value: value.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: key_span.start.line,
		column: key_span.start.column,
	});
}

fn is_top_level_named_block(
	index: &SemanticIndex,
	scope_id: usize,
	key: &str,
	value: &AstValue,
) -> bool {
	scope_kind(index, scope_id) == ScopeKind::File
		&& matches!(value, AstValue::Block { .. })
		&& !is_keyword(key)
}

fn is_named_block_in_top_level_block(
	index: &SemanticIndex,
	scope_id: usize,
	key: &str,
	value: &AstValue,
) -> bool {
	scope_kind(index, scope_id) == ScopeKind::Block
		&& scope_parent_kind(index, scope_id) == Some(ScopeKind::File)
		&& matches!(value, AstValue::Block { .. })
		&& !is_keyword(key)
}

fn scope_parent_kind(index: &SemanticIndex, scope_id: usize) -> Option<ScopeKind> {
	let parent = index.scopes.get(scope_id)?.parent?;
	Some(scope_kind(index, parent))
}

fn monarch_power_prefix(value: &str) -> Option<&'static str> {
	match value {
		"ADM" => Some("adm"),
		"DIP" => Some("dip"),
		"MIL" => Some("mil"),
		_ => None,
	}
}

fn extract_block_scalar_items(value: &AstValue) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut values: Vec<String> = Vec::new();
	for item in items {
		match item {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				if let Some(text) = scalar_text(value) {
					values.push(text);
				}
			}
			AstStatement::Comment { .. } => {}
		}
	}
	values
}

fn extract_named_block_scalar_items(value: &AstValue, key_name: &str) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut values: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != key_name {
			continue;
		}
		if let Some(text) = scalar_text(value) {
			values.push(text);
			continue;
		}
		values.extend(extract_block_scalar_items(value));
	}
	values
}

fn extract_named_block_member_keys(value: &AstValue, key_name: &str) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut keys: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != key_name {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		for nested in items {
			let AstStatement::Assignment { key, .. } = nested else {
				continue;
			};
			keys.push(key.clone());
		}
	}
	keys
}

fn extract_yes_assignment_keys(value: &AstValue) -> Vec<String> {
	let AstValue::Block { items, .. } = value else {
		return Vec::new();
	};
	let mut keys: Vec<String> = Vec::new();
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		let Some(text) = scalar_text(value) else {
			continue;
		};
		if text.eq_ignore_ascii_case("yes") {
			keys.push(key.clone());
		}
	}
	keys
}

fn extract_nested_assignment_scalar(
	items: &[AstStatement],
	block_name: &str,
	field_name: &str,
) -> Option<String> {
	for item in items {
		let AstStatement::Assignment { key, value, .. } = item else {
			continue;
		};
		if key != block_name {
			continue;
		}
		let AstValue::Block { items, .. } = value else {
			continue;
		};
		if let Some(text) = extract_assignment_scalar(items, field_name) {
			return Some(text);
		}
	}
	None
}

fn record_ui_block_semantics(
	index: &mut SemanticIndex,
	ctx: &BuildContext<'_>,
	key: &str,
	key_span: &SpanRange,
	items: &[AstStatement],
) {
	if ctx.file_kind != ScriptFileKind::new("ui") || !looks_like_ui_container_key(key) {
		return;
	}
	let Some(name) = extract_assignment_scalar(items, "name") else {
		return;
	};
	if !is_ui_identifier_candidate(&name) {
		return;
	}
	index.ui_definitions.push(UiDefinition {
		name,
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: key_span.start.line,
		column: key_span.start.column,
	});
}

fn record_alias_usage(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	alias: &str,
	span: &SpanRange,
) {
	index.alias_usages.push(AliasUsage {
		alias: alias.to_string(),
		mod_id: ctx.mod_id.to_string(),
		path: ctx.path.to_owned(),
		line: span.start.line,
		column: span.start.column,
		scope_id,
	});
}

fn record_alias_tokens_from_value(
	index: &mut SemanticIndex,
	scope_id: usize,
	ctx: &BuildContext<'_>,
	value: &AstValue,
) {
	let AstValue::Scalar { value, span } = value else {
		return;
	};
	let text = value.as_text();
	for cap in alias_capture_regex().captures_iter(&text) {
		let Some(alias) = cap.get(1) else {
			continue;
		};
		index.alias_usages.push(AliasUsage {
			alias: alias.as_str().to_string(),
			mod_id: ctx.mod_id.to_string(),
			path: ctx.path.to_owned(),
			line: span.start.line,
			column: span.start.column,
			scope_id,
		});
	}
}

fn record_param_tokens(index: &mut SemanticIndex, def_idx: Option<usize>, value: &AstValue) {
	let Some(def_idx) = def_idx else {
		return;
	};

	match value {
		AstValue::Scalar { value, .. } => {
			let text = value.as_text();
			for cap in param_capture_regex().captures_iter(&text) {
				let Some(param) = cap.get(1) else {
					continue;
				};
				let param = param.as_str().to_string();
				if let Some(def) = index.definitions.get_mut(def_idx)
					&& !def.required_params.contains(&param)
					&& !def.optional_params.contains(&param)
				{
					def.required_params.push(param);
				}
			}
		}
		AstValue::Block { items, .. } => {
			for item in items {
				match item {
					AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
						record_param_tokens(index, Some(def_idx), value)
					}
					AstStatement::Comment { .. } => {}
				}
			}
		}
	}
}

fn alias_capture_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| Regex::new(r"\b(ROOT|FROM|THIS|PREV)\b").expect("valid alias regex"))
}

fn param_capture_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\$").expect("valid param regex"))
}

struct ChildScopeArgs<'i, 'a> {
	index: &'i mut SemanticIndex,
	parent_scope_id: usize,
	ctx: &'i BuildContext<'a>,
	key: &'i str,
	span: &'i SpanRange,
	bound: &'i BoundContext<'a, AstStatement>,
	child: &'i BoundContext<'a, AstStatement>,
	definition: Option<&'a CompiledRoot>,
	call_body: Option<CallBody>,
	/// Whether the key is a field of its block's rules, which names no scope
	/// whatever it spells (`from = { 100 200 100 }` in a map tile).
	is_field: bool,
}

/// A block's scope: what it holds, and the scope it runs in, as its rules
/// say. A key that switches to a scope the schema names by a set of names (a
/// country tag, an area) leaves its scope unknown here; which set holds the
/// key is known only once the definitions are.
fn create_child_scope(args: ChildScopeArgs<'_, '_>) -> usize {
	let ChildScopeArgs {
		index,
		parent_scope_id,
		ctx,
		key,
		span,
		bound,
		child,
		definition,
		call_body,
		is_field,
	} = args;
	let parent_this = scope_this_type(index, parent_scope_id);
	let mut aliases = scope_aliases(index, parent_scope_id);
	let alias = |name: &str| aliases.get(name).copied().unwrap_or(MaybeScope::Unknown);
	let keyword = ctx.binder.dialect.scope_keyword(key).filter(|_| !is_field);
	let (mut this_type, mut inherits_this) = match (keyword, call_body) {
		(_, Some(CallBody::ScopeSwitch)) => (MaybeScope::Unknown, false),
		(Some(ScopeKeyword::Root), _) => (alias("ROOT"), false),
		(Some(ScopeKeyword::From), _) => (alias("FROM"), false),
		(Some(ScopeKeyword::Prev), _) => (alias("PREV"), false),
		(Some(ScopeKeyword::PrevPrev), _) => (MaybeScope::Unknown, false),
		(Some(_), _) => (parent_this, true),
		(None, _) if ctx.binder.dialect.is_event_target(key) => (MaybeScope::Unknown, false),
		(None, _) if child.scopes() != bound.scopes() => (schema_scope_type(child.scopes()), false),
		(None, _) => (parent_this, definition.is_none()),
	};
	aliases.insert("PREV".to_string(), parent_this);
	if let Some(replace_scope) = child.replace_scope() {
		for (name, scope) in replace_scope {
			let scope = replaced_scope_type(ctx.binder, scope);
			match name.as_str() {
				"this" => {
					this_type = scope;
					inherits_this = false;
				}
				"root" => {
					aliases.insert("ROOT".to_string(), scope);
				}
				"from" => {
					aliases.insert("FROM".to_string(), scope);
				}
				_ => {}
			}
		}
	}
	let kind = match definition.map(|root| root.name.as_str()) {
		// An event runs in its own scope, fired by a country.
		Some("event") => {
			aliases.insert("ROOT".to_string(), this_type);
			aliases
				.entry("FROM".to_string())
				.or_insert(base_scope::country().into());
			ScopeKind::Event
		}
		Some("scripted_effect") => ScopeKind::ScriptedEffect,
		Some("scripted_trigger") => ScopeKind::Trigger,
		_ => child.rules().map_or(ScopeKind::Block, |rules| {
			rules_scope_kind(rules, child.subtypes())
		}),
	};
	aliases.insert("THIS".to_string(), this_type);
	push_scope(ScopeArgs {
		index,
		kind,
		parent: Some(parent_scope_id),
		this_type,
		aliases,
		mod_id: ctx.mod_id,
		path: ctx.path,
		line: span.start.line,
		key,
		inherits_this,
	})
}

/// The scope a `replace_scope` entry names: a scope, or a link the rule files
/// write for the scope it leads to (`from = owner` in buildings).
fn replaced_scope_type(binder: &SchemaBinder<'_>, scope: &str) -> MaybeScope {
	let scope = binder
		.scope_link(scope)
		.and_then(|link| link.output_scope.as_deref())
		.unwrap_or(scope);
	schema_scope_type(&[scope.to_string()])
}

/// The scope a block runs in when the schema names exactly one the index
/// tracks.
fn schema_scope_type(scopes: &[String]) -> MaybeScope {
	match scopes {
		[scope] => lookup_scope(scope).map_or(MaybeScope::Unknown, MaybeScope::Known),
		_ => MaybeScope::Unknown,
	}
}

fn province_name_table_id(path: &GamePath) -> Option<String> {
	path.file_stem().map(str::to_string)
}

fn random_map_tile_id(path: &GamePath) -> Option<String> {
	path.file_stem().map(str::to_string)
}

fn random_name_table_id(path: &GamePath) -> Option<String> {
	match path.file_stem() {
		Some("RandomLandNames") => Some("random_land_names".to_string()),
		Some("RandomSeaNames") => Some("random_sea_names".to_string()),
		Some("RandomLakeNames") => Some("random_lake_names".to_string()),
		_ => None,
	}
}

struct ScopeArgs<'a> {
	index: &'a mut SemanticIndex,
	kind: ScopeKind,
	parent: Option<usize>,
	this_type: MaybeScope,
	aliases: BTreeMap<String, MaybeScope>,
	mod_id: &'a str,
	path: &'a GamePath,
	line: usize,
	key: &'a str,
	inherits_this: bool,
}

fn push_scope(args: ScopeArgs<'_>) -> usize {
	let ScopeArgs {
		index,
		kind,
		parent,
		this_type,
		aliases,
		mod_id,
		path,
		line,
		key,
		inherits_this,
	} = args;
	let id = index.scopes.len();
	index.scopes.push(ScopeNode {
		id,
		kind,
		parent,
		this_type,
		aliases,
		mod_id: mod_id.to_string(),
		path: path.to_owned(),
		span: SourceSpan { line, column: 1 },
		key: key.to_string(),
		inherits_this,
	});
	id
}

fn scope_kind(index: &SemanticIndex, scope_id: usize) -> ScopeKind {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.kind)
		.unwrap_or(ScopeKind::Block)
}

fn scope_this_type(index: &SemanticIndex, scope_id: usize) -> MaybeScope {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.this_type)
		.unwrap_or(MaybeScope::Unknown)
}

fn scope_aliases(index: &SemanticIndex, scope_id: usize) -> BTreeMap<String, MaybeScope> {
	index
		.scopes
		.get(scope_id)
		.map(|scope| scope.aliases.clone())
		.unwrap_or_default()
}

fn line_from_stmt(stmt: Option<&AstStatement>) -> usize {
	stmt.map(|item| match item {
		AstStatement::Assignment { span, .. } => span.start.line,
		AstStatement::Item { span, .. } => span.start.line,
		AstStatement::Comment { span, .. } => span.start.line,
	})
	.unwrap_or(1)
}

fn extract_assignment_scalar(items: &[AstStatement], name: &str) -> Option<String> {
	for item in items {
		if let AstStatement::Assignment { key, value, .. } = item
			&& key == name
			&& let Some(text) = scalar_text(value)
		{
			return Some(text);
		}
	}
	None
}

fn scalar_text(value: &AstValue) -> Option<String> {
	let AstValue::Scalar { value, .. } = value else {
		return None;
	};
	Some(value.as_text())
}

fn collect_required_params(items: &[AstStatement]) -> Vec<String> {
	let param_re = Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\$").expect("valid param regex");
	let mut params = Vec::new();
	for stmt in items {
		match stmt {
			AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
				collect_params_from_value(value, &param_re, &mut params)
			}
			AstStatement::Comment { .. } => {}
		}
	}
	params
}

/// Detect parameters that callers may legitimately omit.
///
/// Paradox script uses two markers to declare optional parameters in
/// scripted_effects / scripted_triggers / similar callable bodies:
///
///   1. `[[NAME] ... ]` — the inner block is only emitted when `$NAME$` is
///      provided; this is the canonical "optional block" syntax. Any
///      `$NAME$` reference inside such a block is gated on `NAME` being
///      bound.
///   2. `$NAME|fallback$` — a default-value substitution; `NAME` is
///      optional because the engine substitutes `fallback` when missing.
///
/// We scan the raw source slice covering the callable body and treat any
/// name introduced via these markers as optional. Numbered series
/// (`culture1`, `culture2`, ... `cultureN`) are handled implicitly because
/// vanilla EU4 always wraps the higher-indexed slots in `[[cultureN] ... ]`
/// blocks. This means we don't need an allowlist: a callable that uses
/// `[[X]` to gate `$X$` automatically marks `X` as optional.
fn collect_optional_params_from_source(
	source: &str,
	start_offset: usize,
	end_offset: usize,
) -> Vec<String> {
	if source.is_empty() || end_offset <= start_offset || end_offset > source.len() {
		return Vec::new();
	}
	let slice = &source[start_offset..end_offset];
	let bracket_re =
		Regex::new(r"\[\[\s*([A-Za-z_][A-Za-z0-9_]*)\s*\]").expect("valid optional-block regex");
	let default_re =
		Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)\s*\|").expect("valid default-fallback regex");
	let mut names: HashSet<String> = HashSet::new();
	for cap in bracket_re.captures_iter(slice) {
		if let Some(name) = cap.get(1) {
			names.insert(name.as_str().to_string());
		}
	}
	for cap in default_re.captures_iter(slice) {
		if let Some(name) = cap.get(1) {
			names.insert(name.as_str().to_string());
		}
	}
	let mut collected: Vec<String> = names.into_iter().collect();
	collected.sort();
	collected
}

fn collect_params_from_value(value: &AstValue, re: &Regex, out: &mut Vec<String>) {
	match value {
		AstValue::Scalar { value, .. } => {
			let text = value.as_text();
			for cap in re.captures_iter(&text) {
				if let Some(param) = cap.get(1) {
					out.push(param.as_str().to_string());
				}
			}
		}
		AstValue::Block { items, .. } => {
			for stmt in items {
				match stmt {
					AstStatement::Assignment { value, .. } | AstStatement::Item { value, .. } => {
						collect_params_from_value(value, re, out)
					}
					AstStatement::Comment { .. } => {}
				}
			}
		}
	}
}

#[derive(Default)]
struct ProvidedParams {
	names: Vec<String>,
	bindings: Vec<ParamBinding>,
}

/// The parameters a call passes: every key its block assigns, and the value
/// of each one that assigns a value.
fn collect_provided_params(items: &[AstStatement]) -> ProvidedParams {
	let mut params = ProvidedParams::default();
	for stmt in items {
		if let AstStatement::Assignment { key, value, .. } = stmt {
			params.names.push(key.clone());
			if let Some(value) = scalar_text(value) {
				params.bindings.push(ParamBinding {
					name: key.clone(),
					value,
				});
			}
		}
	}
	params
}

fn collect_scope_param_names(items: &[AstStatement]) -> Vec<String> {
	let mut names = HashSet::new();
	collect_scope_param_names_from_statements(items, &mut names);
	let mut collected: Vec<String> = names.into_iter().collect();
	collected.sort();
	collected
}

fn extract_template_param_name(value: &str) -> Option<&str> {
	let trimmed = value.trim();
	if !(trimmed.starts_with('$') && trimmed.ends_with('$') && trimmed.len() > 2) {
		return None;
	}
	let param = &trimmed[1..trimmed.len() - 1];
	if param
		.chars()
		.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
	{
		Some(param)
	} else {
		None
	}
}

fn collect_scope_param_names_from_statements(items: &[AstStatement], names: &mut HashSet<String>) {
	for stmt in items {
		if let AstStatement::Assignment { key, value, .. } = stmt {
			if let Some(param_name) = extract_template_param_name(key)
				&& matches!(value, AstValue::Block { .. })
			{
				names.insert(param_name.to_string());
			}
			if let AstValue::Block { items, .. } = value {
				collect_scope_param_names_from_statements(items, names);
			}
		}
	}
}

pub fn is_decision_container_key(key: &str) -> bool {
	matches!(
		key,
		"country_decisions" | "province_decisions" | "religion_decisions" | "government_decisions"
	)
}

fn is_keyword(key: &str) -> bool {
	if is_reserved_keyword(key) || is_contextual_keyword(key) {
		return true;
	}
	matches!(key, "condition" | "from")
}

fn looks_like_ui_container_key(key: &str) -> bool {
	key.ends_with("Type")
		|| matches!(
			key,
			"containerWindow" | "window" | "button" | "icon" | "sprite" | "shield"
		)
}

fn is_ui_identifier_candidate(value: &str) -> bool {
	!value.is_empty()
		&& value.len() <= 128
		&& value
			.chars()
			.all(|ch| matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '-' | ':'))
}

fn is_ui_resource_key(key: &str) -> bool {
	matches!(
		key,
		"spriteType"
			| "buttonSprite"
			| "quadTextureSprite"
			| "texturefile"
			| "icon" | "iconType"
			| "pdxmesh"
			| "mesh"
	)
}

/// Links calls to scripted effects and triggers the schema declares under
/// their own names. Those keys bind as the engine's effects and triggers, so
/// the walk records no call; where the index holds a definition of that name,
/// a key written where effects (or triggers) go calls it. Calls already linked
/// stay as they are, so linking an index assembled from linked ones adds only
/// the calls one part makes to another's definitions.
pub fn link_schema_declared_calls(index: &mut SemanticIndex) {
	let declared = index
		.definitions
		.iter()
		.filter(|definition| match definition.kind {
			SymbolKind::ScriptedEffect => is_schema_effect(&definition.local_name),
			SymbolKind::ScriptedTrigger => is_schema_trigger(&definition.local_name),
			_ => false,
		})
		.map(|definition| (definition.kind, definition.local_name.clone()))
		.collect::<HashSet<_>>();
	if declared.is_empty() {
		return;
	}
	let linked = index
		.references
		.iter()
		.map(|reference| {
			(
				reference.mod_id.clone(),
				reference.path.clone(),
				reference.line,
				reference.column,
			)
		})
		.collect::<HashSet<_>>();
	let mut calls = Vec::new();
	for usage in &index.key_usages {
		let kind = match scope_kind(index, usage.scope_id) {
			ScopeKind::Effect | ScopeKind::ScriptedEffect => SymbolKind::ScriptedEffect,
			ScopeKind::Trigger => SymbolKind::ScriptedTrigger,
			_ => continue,
		};
		if !declared.contains(&(kind, usage.key.clone()))
			|| linked.contains(&(
				usage.mod_id.clone(),
				usage.path.clone(),
				usage.line,
				usage.column,
			)) {
			continue;
		}
		calls.push(SymbolReference {
			kind,
			name: usage.key.clone(),
			module: String::new(),
			mod_id: usage.mod_id.clone(),
			path: usage.path.clone(),
			line: usage.line,
			column: usage.column,
			scope_id: usage.scope_id,
			provided_params: Vec::new(),
			param_bindings: Vec::new(),
			schema_alias: true,
		});
	}
	index.references.extend(calls);
}

fn is_inferred_callable_kind(kind: SymbolKind) -> bool {
	matches!(
		kind,
		SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger
	)
}

fn resolve_reference_targets_for_kind(
	index: &SemanticIndex,
	reference: &SymbolReference,
	kind: SymbolKind,
) -> Vec<usize> {
	if reference.kind != kind {
		return Vec::new();
	}

	let mut exact = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != kind {
			continue;
		}
		if def.module == reference.module && def.local_name == reference.name {
			exact.push(idx);
		}
	}
	if !exact.is_empty() {
		return exact;
	}

	let mut by_local = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != kind {
			continue;
		}
		if def.local_name == reference.name {
			by_local.push(idx);
		}
	}
	// Scripted effects/triggers are global — if any definition matches by
	// name+kind, the reference is resolved (EU4 uses last-loaded-wins for
	// duplicate names, so having 1+ matches means the key exists).
	if !by_local.is_empty() {
		return by_local;
	}

	Vec::new()
}

pub fn resolve_scripted_effect_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	resolve_reference_targets_for_kind(index, reference, SymbolKind::ScriptedEffect)
}

pub fn resolve_scripted_trigger_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	resolve_reference_targets_for_kind(index, reference, SymbolKind::ScriptedTrigger)
}

pub fn resolve_symbol_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	match reference.kind {
		SymbolKind::Event => resolve_event_reference_targets(index, reference),
		SymbolKind::ScriptedEffect => resolve_scripted_effect_reference_targets(index, reference),
		SymbolKind::ScriptedTrigger => resolve_scripted_trigger_reference_targets(index, reference),
		_ => Vec::new(),
	}
}

pub fn count_symbol_references_resolving_to_mod(
	index: &SemanticIndex,
	source_mod_id: &str,
	target_mod_id: &str,
) -> u32 {
	let table = SymbolTable::new(index);
	let count = index
		.references
		.iter()
		.filter(|reference| reference.mod_id == source_mod_id)
		.filter(|reference| {
			table
				.targets(index, reference)
				.into_iter()
				.filter_map(|target| index.definitions.get(target))
				.any(|definition| definition.mod_id == target_mod_id)
		})
		.count();
	count.min(u32::MAX as usize) as u32
}

/// Search for definitions of `target_kind` that match `reference` by name,
/// ignoring the reference's own kind.  Used for cross-kind resolution
/// (e.g. a trigger-context key that is actually a scripted effect).
pub fn resolve_cross_kind_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
	target_kind: SymbolKind,
) -> Vec<usize> {
	let mut by_local = Vec::new();
	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != target_kind {
			continue;
		}
		if def.module == reference.module && def.local_name == reference.name {
			return vec![idx];
		}
		if def.local_name == reference.name {
			by_local.push(idx);
		}
	}
	by_local
}

/// Resolve an event reference to its definition(s) in the index.
///
/// EU4 event IDs use `namespace.number` format, but references can be bare
/// numbers. This resolver handles three cases:
///   1. Exact match: reference name equals definition name
///   2. Bare-ID ref → qualified def: ref "9073", def "hre_event.9073"
///   3. Qualified ref → bare def: ref "hre_event.9073", def "9073"
pub fn resolve_event_reference_targets(
	index: &SemanticIndex,
	reference: &SymbolReference,
) -> Vec<usize> {
	if reference.kind != SymbolKind::Event {
		return Vec::new();
	}

	let ref_name = reference.name.as_str();
	let mut matches = Vec::new();

	for (idx, def) in index.definitions.iter().enumerate() {
		if def.kind != SymbolKind::Event {
			continue;
		}
		let def_name = def.name.as_str();

		// 1. Exact match
		if def_name == ref_name {
			matches.push(idx);
			continue;
		}
		// 2. Bare-ID ref against qualified def: def ends with ".{ref}"
		if def_name.ends_with(ref_name)
			&& def_name.as_bytes().get(def_name.len() - ref_name.len() - 1) == Some(&b'.')
		{
			matches.push(idx);
			continue;
		}
		// 3. Qualified ref against bare def: ref ends with ".{def}"
		if ref_name.ends_with(def_name)
			&& ref_name.as_bytes().get(ref_name.len() - def_name.len() - 1) == Some(&b'.')
		{
			matches.push(idx);
		}
	}

	matches
}

/// An index's definitions by kind and name, to resolve many references
/// without reading every definition for each. Its answers are those of the
/// `resolve_*` functions.
pub struct SymbolTable {
	by_name: HashMap<(SymbolKind, String), Vec<usize>>,
	/// Event ids by every tail after a `.` (`9073` for `hre_event.9073`).
	events_by_tail: HashMap<String, Vec<usize>>,
}

impl SymbolTable {
	pub fn new(index: &SemanticIndex) -> Self {
		let mut by_name = HashMap::<(SymbolKind, String), Vec<usize>>::new();
		let mut events_by_tail = HashMap::<String, Vec<usize>>::new();
		for (id, definition) in index.definitions.iter().enumerate() {
			let name = match definition.kind {
				SymbolKind::Event => &definition.name,
				_ => &definition.local_name,
			};
			by_name
				.entry((definition.kind, name.clone()))
				.or_default()
				.push(id);
			if definition.kind == SymbolKind::Event {
				for (dot, _) in name.match_indices('.') {
					events_by_tail
						.entry(name[dot + 1..].to_string())
						.or_default()
						.push(id);
				}
			}
		}
		Self {
			by_name,
			events_by_tail,
		}
	}

	fn named(&self, kind: SymbolKind, name: &str) -> &[usize] {
		self.by_name
			.get(&(kind, name.to_string()))
			.map_or(&[], Vec::as_slice)
	}

	/// What [`resolve_symbol_reference_targets`] answers.
	pub fn targets(&self, index: &SemanticIndex, reference: &SymbolReference) -> Vec<usize> {
		match reference.kind {
			SymbolKind::Event => self.events(reference),
			SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger => {
				self.scripted(index, reference, reference.kind)
			}
			_ => Vec::new(),
		}
	}

	/// What [`resolve_scripted_effect_reference_targets`] or its trigger twin
	/// answers: the definitions of `kind` in the reference's own module,
	/// else of its name anywhere.
	pub fn scripted(
		&self,
		index: &SemanticIndex,
		reference: &SymbolReference,
		kind: SymbolKind,
	) -> Vec<usize> {
		if reference.kind != kind {
			return Vec::new();
		}
		let all = self.named(kind, &reference.name);
		let exact = all
			.iter()
			.copied()
			.filter(|id| index.definitions[*id].module == reference.module)
			.collect::<Vec<_>>();
		if exact.is_empty() {
			all.to_vec()
		} else {
			exact
		}
	}

	/// What [`resolve_cross_kind_reference_targets`] answers.
	pub fn of_kind(
		&self,
		index: &SemanticIndex,
		reference: &SymbolReference,
		kind: SymbolKind,
	) -> Vec<usize> {
		let all = self.named(kind, &reference.name);
		match all
			.iter()
			.copied()
			.find(|id| index.definitions[*id].module == reference.module)
		{
			Some(exact) => vec![exact],
			None => all.to_vec(),
		}
	}

	/// What [`resolve_event_reference_targets`] answers.
	pub fn events(&self, reference: &SymbolReference) -> Vec<usize> {
		if reference.kind != SymbolKind::Event {
			return Vec::new();
		}
		let name = reference.name.as_str();
		let mut ids = self.named(SymbolKind::Event, name).to_vec();
		ids.extend(self.events_by_tail.get(name).into_iter().flatten());
		for (dot, _) in name.match_indices('.') {
			ids.extend_from_slice(self.named(SymbolKind::Event, &name[dot + 1..]));
		}
		ids.sort_unstable();
		ids.dedup();
		ids
	}
}

fn infer_definition_scope_from_references(index: &mut SemanticIndex) {
	let callable_scope_map = build_inferred_callable_scope_map(index);
	let table = SymbolTable::new(index);

	let mut observed_masks: Vec<ScopeSet> = index
		.definitions
		.iter()
		.map(|definition| {
			if is_inferred_callable_kind(definition.kind) {
				scope_mask(definition.declared_this_type)
			} else {
				ScopeSet::EMPTY
			}
		})
		.collect();

	let mut changed = true;
	while changed {
		changed = false;
		for reference in &index.references {
			if !is_inferred_callable_kind(reference.kind) {
				continue;
			}
			let caller_mask = effective_scope_mask_with_overrides(
				index,
				&callable_scope_map,
				&observed_masks,
				reference.scope_id,
			);
			if caller_mask.is_empty() {
				continue;
			}
			let target_defs = match reference.kind {
				SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger => {
					table.scripted(index, reference, reference.kind)
				}
				_ => Vec::new(),
			};
			for def_idx in target_defs {
				let mut merged = observed_masks[def_idx] | caller_mask;
				if let Some(definition) = index.definitions.get(def_idx)
					&& definition.kind == SymbolKind::ScriptedEffect
				{
					for binding in &reference.param_bindings {
						if !definition
							.scope_param_names
							.iter()
							.any(|name| name == &binding.name)
						{
							continue;
						}
						merged |= binding_value_scope_mask(
							index,
							&callable_scope_map,
							&observed_masks,
							reference.scope_id,
							&binding.value,
						);
					}
				}
				if merged != observed_masks[def_idx] {
					observed_masks[def_idx] = merged;
					changed = true;
				}
			}
		}
	}

	let backfill_candidates: Vec<bool> = index
		.definitions
		.iter()
		.enumerate()
		.map(|(idx, definition)| {
			definition.kind == SymbolKind::ScriptedEffect && observed_masks[idx].is_empty()
		})
		.collect();
	for scope_id in 0..index.scopes.len() {
		let scope_mask = effective_scope_mask_with_overrides(
			index,
			&callable_scope_map,
			&observed_masks,
			scope_id,
		);
		if scope_mask.is_empty() {
			continue;
		}
		let Some(def_idx) = nearest_enclosing_scripted_effect_definition_index(
			index,
			&callable_scope_map,
			scope_id,
		) else {
			continue;
		};
		if !backfill_candidates[def_idx] {
			continue;
		}
		observed_masks[def_idx] |= scope_mask;
	}

	for usage in &index.key_usages {
		if usage.key != "capital_scope" {
			continue;
		}
		let Some(def_idx) =
			nearest_enclosing_callable_definition_index(index, &callable_scope_map, usage.scope_id)
		else {
			continue;
		};
		let Some(definition) = index.definitions.get(def_idx) else {
			continue;
		};
		if definition.kind != SymbolKind::ScriptedTrigger {
			continue;
		}
		observed_masks[def_idx] |= scope_mask(base_scope::country());
	}

	for (idx, definition) in index.definitions.iter_mut().enumerate() {
		if !is_inferred_callable_kind(definition.kind) {
			continue;
		}
		definition.inferred_this_mask = observed_masks[idx];
		definition.inferred_this_type = scope_type_from_mask(observed_masks[idx]);
	}
}

/// Propagate dynamic-scope aliases (FROM, ROOT) from invocation sites into
/// callable definitions (scripted_effects, scripted_triggers, events). For
/// each invocation the caller's resolved alias mask is unioned into the
/// target's `inferred_from_mask` / `inferred_root_mask`. After the fixed
/// point converges the resolved types are also injected into the body
/// scope's alias map so that `is_alias_visible` and downstream rules see
/// the alias as bound — eliminating invisible-scope-alias / unknown-scope-type noise rooted in callable
/// bodies that inherit dynamic scope from their (statically resolvable)
/// callers.
///
/// FROM in event bodies is seeded by `handle_event_block` already; this
/// pass mostly affects scripted_effect / scripted_trigger callees but we
/// still walk events for consistency. ROOT in event bodies is pinned to
/// the event scope at seed time, so the relevant case here is again
/// scripted_effect / scripted_trigger inheriting the caller's ROOT.
fn infer_definition_dynamic_alias_masks_from_references(index: &mut SemanticIndex) {
	let callable_scope_map = build_inferred_callable_scope_map(index);
	let table = SymbolTable::new(index);
	let mut from_masks: Vec<ScopeSet> = vec![ScopeSet::EMPTY; index.definitions.len()];
	let mut root_masks: Vec<ScopeSet> = vec![ScopeSet::EMPTY; index.definitions.len()];

	let mut changed = true;
	while changed {
		changed = false;
		for reference in &index.references {
			if !matches!(
				reference.kind,
				SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger | SymbolKind::Event
			) {
				continue;
			}
			let caller_from = caller_alias_mask_via_chain(
				index,
				&callable_scope_map,
				&from_masks,
				reference.scope_id,
				"FROM",
			);
			let caller_root = caller_alias_mask_via_chain(
				index,
				&callable_scope_map,
				&root_masks,
				reference.scope_id,
				"ROOT",
			);
			if caller_from.is_empty() && caller_root.is_empty() {
				continue;
			}
			let target_defs = table.targets(index, reference);
			for def_idx in target_defs {
				let merged_from = from_masks[def_idx] | caller_from;
				if merged_from != from_masks[def_idx] {
					from_masks[def_idx] = merged_from;
					changed = true;
				}
				let merged_root = root_masks[def_idx] | caller_root;
				if merged_root != root_masks[def_idx] {
					root_masks[def_idx] = merged_root;
					changed = true;
				}
			}
		}
	}

	let scope_updates: Vec<(usize, Option<MaybeScope>, Option<MaybeScope>)> = index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, def)| {
			if !matches!(
				def.kind,
				SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger | SymbolKind::Event
			) {
				return None;
			}
			let from_ty =
				(!from_masks[idx].is_empty()).then(|| scope_type_from_mask(from_masks[idx]));
			let root_ty =
				(!root_masks[idx].is_empty()).then(|| scope_type_from_mask(root_masks[idx]));
			if from_ty.is_none() && root_ty.is_none() {
				return None;
			}
			Some((def.scope_id, from_ty, root_ty))
		})
		.collect();
	for (scope_id, from_ty, root_ty) in scope_updates {
		if let Some(scope) = index.scopes.get_mut(scope_id) {
			if let Some(ty) = from_ty {
				scope.aliases.entry("FROM".to_string()).or_insert(ty);
			}
			if let Some(ty) = root_ty {
				scope.aliases.entry("ROOT".to_string()).or_insert(ty);
			}
		}
	}
	for (idx, definition) in index.definitions.iter_mut().enumerate() {
		if matches!(
			definition.kind,
			SymbolKind::ScriptedEffect | SymbolKind::ScriptedTrigger | SymbolKind::Event
		) {
			definition.inferred_from_mask = from_masks[idx];
			definition.inferred_root_mask = root_masks[idx];
		}
	}
}

fn caller_alias_mask_via_chain(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_alias_masks: &[ScopeSet],
	mut scope_id: usize,
	alias: &str,
) -> ScopeSet {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return ScopeSet::EMPTY;
		};
		if let Some(ty) = scope.aliases.get(alias) {
			let mask = scope_mask(*ty);
			if !mask.is_empty() {
				return mask;
			}
		}
		if let Some(def_idx) = callable_scope_map.get(&scope_id)
			&& let Some(mask) = observed_alias_masks.get(*def_idx).copied()
			&& !mask.is_empty()
		{
			return mask;
		}
		let Some(parent) = scope.parent else {
			return ScopeSet::EMPTY;
		};
		scope_id = parent;
	}
}

pub(crate) fn build_inferred_callable_scope_map(index: &SemanticIndex) -> HashMap<usize, usize> {
	index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, definition)| {
			is_inferred_callable_kind(definition.kind).then_some((definition.scope_id, idx))
		})
		.collect()
}

pub(crate) fn collect_inferred_callable_masks(index: &SemanticIndex) -> Vec<ScopeSet> {
	index
		.definitions
		.iter()
		.map(|definition| definition.inferred_this_mask)
		.collect()
}

fn binding_value_scope_mask(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[ScopeSet],
	scope_id: usize,
	value: &str,
) -> ScopeSet {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return ScopeSet::EMPTY;
	}
	match trimmed {
		"THIS" | "ROOT" | "FROM" | "PREV" => {
			return effective_alias_scope_mask_with_overrides(
				index,
				callable_scope_map,
				observed_masks,
				scope_id,
				trimmed,
			);
		}
		_ => {}
	}
	super::cwt::rule_engine()
		.link(trimmed)
		.and_then(|link| link.output_scope.as_deref())
		.and_then(lookup_scope)
		.map_or(ScopeSet::EMPTY, scope_mask)
}

pub(crate) fn effective_alias_scope_mask_with_overrides(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[ScopeSet],
	mut scope_id: usize,
	alias: &str,
) -> ScopeSet {
	let mut fallback_mask = ScopeSet::EMPTY;
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return fallback_mask;
		};
		if alias == "THIS" {
			let this_mask = scope_mask(scope.this_type);
			if !this_mask.is_empty() {
				return this_mask;
			}
			if let Some(def_idx) = callable_scope_map.get(&scope_id) {
				let inferred_mask = observed_masks
					.get(*def_idx)
					.copied()
					.unwrap_or(ScopeSet::EMPTY);
				if !inferred_mask.is_empty() {
					return inferred_mask;
				}
			}
		}
		if let Some(alias_type) = scope.aliases.get(alias) {
			let alias_mask = scope_mask(*alias_type);
			if !alias_mask.is_empty() {
				return alias_mask;
			}
		}
		if fallback_mask.is_empty()
			&& let Some(def_idx) = callable_scope_map.get(&scope_id)
		{
			let inferred_mask = observed_masks
				.get(*def_idx)
				.copied()
				.unwrap_or(ScopeSet::EMPTY);
			if !inferred_mask.is_empty() {
				fallback_mask = inferred_mask;
			}
		}
		let Some(parent) = scope.parent else {
			return fallback_mask;
		};
		scope_id = parent;
	}
}

fn nearest_enclosing_callable_definition_index(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	mut scope_id: usize,
) -> Option<usize> {
	loop {
		if let Some(def_idx) = callable_scope_map.get(&scope_id)
			&& index
				.definitions
				.get(*def_idx)
				.is_some_and(|definition| is_inferred_callable_kind(definition.kind))
		{
			return Some(*def_idx);
		}
		let parent = index.scopes.get(scope_id).and_then(|scope| scope.parent)?;
		scope_id = parent;
	}
}

fn nearest_enclosing_scripted_effect_definition_index(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	scope_id: usize,
) -> Option<usize> {
	let def_idx = nearest_enclosing_callable_definition_index(index, callable_scope_map, scope_id)?;
	index
		.definitions
		.get(def_idx)
		.filter(|definition| definition.kind == SymbolKind::ScriptedEffect)
		.map(|_| def_idx)
}

fn scope_mask<T>(scope: T) -> ScopeSet
where
	T: Into<ScopeSet>,
{
	scope.into()
}

fn scope_type_from_mask(mask: ScopeSet) -> MaybeScope {
	mask.as_single_scope()
		.map(MaybeScope::Known)
		.unwrap_or(MaybeScope::Unknown)
}

pub(crate) fn effective_scope_mask_with_overrides(
	index: &SemanticIndex,
	callable_scope_map: &HashMap<usize, usize>,
	observed_masks: &[ScopeSet],
	mut scope_id: usize,
) -> ScopeSet {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return ScopeSet::EMPTY;
		};
		if !scope.this_type.is_unknown() {
			return scope_mask(scope.this_type);
		}
		if let Some(def_idx) = callable_scope_map.get(&scope_id)
			&& let Some(inferred_mask) = observed_masks.get(*def_idx).copied()
			&& !inferred_mask.is_empty()
		{
			return inferred_mask;
		}
		let Some(parent) = scope.parent else {
			return ScopeSet::EMPTY;
		};
		scope_id = parent;
	}
}

#[cfg(test)]
mod tests;
