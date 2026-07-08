use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::CwtLoadError;
use crate::pack::{SchemaPack, SchemaPackId, SchemaSource, schema_pack_id_from_dir};
use crate::schema::{
	AliasCategory, CwtAlias, CwtFieldAttributes, CwtRuleField, CwtRuleValue, CwtSchemaGraph,
	CwtScope, CwtSeverity, CwtSubtype, CwtTypeDef, CwtTypeKeyFilter,
};
use crate::{CwtNodeId, CwtType, SchemaBinding};

pub const PACK_FORMAT_VERSION: &str = "0.6.1";
const DEFAULT_COMPILED_RULE_CACHE_DIR_NAME: &str = "cwt-rules";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleEngineLoadStatus {
	CacheHit,
	CompiledFromSource,
}

pub struct RuleEngineLoad {
	pub engine: Arc<RuleEngine>,
	pub status: RuleEngineLoadStatus,
	pub source_id: SchemaPackId,
	pub cache_path: Option<PathBuf>,
	pub timings: RuleEngineLoadTimings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleEngineLoadTimings {
	pub source_hash: Duration,
	pub cache_read: Option<Duration>,
	pub source_compile: Option<Duration>,
	pub total: Duration,
}

pub fn default_compiled_rule_cache_dir() -> PathBuf {
	foch_core::cache::default_foch_cache_dir().join(DEFAULT_COMPILED_RULE_CACHE_DIR_NAME)
}

pub fn load_rule_engine_from_dir(
	root: &Path,
	source: SchemaSource,
	cache_dir: Option<&Path>,
) -> Result<RuleEngineLoad, CwtLoadError> {
	let total_started = Instant::now();
	let hash_started = Instant::now();
	let source_id = schema_pack_id_from_dir(root)?;
	let source_hash = hash_started.elapsed();
	let source_id_hex = source_id.to_hex();
	let cache_path = cache_dir.map(|dir| compiled_rule_cache_path(dir, &source_id_hex));
	let mut cache_read = None;
	if let Some(path) = cache_path.as_ref() {
		let cache_started = Instant::now();
		let cached_pack = read_cached_compiled_pack(path, &source_id_hex);
		cache_read = Some(cache_started.elapsed());
		if let Some(pack) = cached_pack {
			return Ok(RuleEngineLoad {
				engine: Arc::new(RuleEngine::new(pack)),
				status: RuleEngineLoadStatus::CacheHit,
				source_id,
				cache_path,
				timings: RuleEngineLoadTimings {
					source_hash,
					cache_read,
					source_compile: None,
					total: total_started.elapsed(),
				},
			});
		}
	}

	let compile_started = Instant::now();
	let schema_pack = SchemaPack::load_from_dir_with_id(root, source, source_id.clone())?;
	let compiled_pack = CompiledRulePack::from_schema_pack(&schema_pack);
	let source_compile = compile_started.elapsed();
	if let Some(path) = cache_path.as_ref() {
		write_cached_compiled_pack(path, &compiled_pack);
	}
	Ok(RuleEngineLoad {
		engine: Arc::new(RuleEngine::new(compiled_pack)),
		status: RuleEngineLoadStatus::CompiledFromSource,
		source_id,
		cache_path,
		timings: RuleEngineLoadTimings {
			source_hash,
			cache_read,
			source_compile: Some(source_compile),
			total: total_started.elapsed(),
		},
	})
}

fn compiled_rule_cache_path(cache_dir: &Path, source_id: &str) -> PathBuf {
	let format = PACK_FORMAT_VERSION.replace('.', "_");
	cache_dir.join(format!("rules-fmt-{format}-src-{source_id}.bin"))
}

fn read_cached_compiled_pack(path: &Path, source_id: &str) -> Option<CompiledRulePack> {
	let bytes = fs::read(path).ok()?;
	let pack = CompiledRulePack::from_bytes(&bytes).ok()?;
	(pack.source_id.as_deref() == Some(source_id)).then_some(pack)
}

fn write_cached_compiled_pack(path: &Path, pack: &CompiledRulePack) {
	let Some(parent) = path.parent() else {
		return;
	};
	if fs::create_dir_all(parent).is_err() {
		return;
	}
	let Ok(bytes) = pack.to_bytes() else {
		return;
	};
	let _ = fs::write(path, bytes);
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledRulePack {
	pub format_version: String,
	pub source_id: Option<String>,
	pub roots: Vec<CompiledRoot>,
	pub aliases: Vec<CompiledAlias>,
	pub enums: Vec<CompiledStringSet>,
	pub value_sets: Vec<CompiledStringSet>,
	pub scopes: Vec<String>,
	pub scope_definitions: Vec<CompiledScope>,
}

impl CompiledRulePack {
	pub fn from_graph(graph: &CwtSchemaGraph) -> Self {
		let mut roots = graph
			.types
			.values()
			.map(CompiledRoot::from_type_def)
			.collect::<Vec<_>>();
		roots.sort_by(|left, right| left.name.cmp(&right.name));

		let mut aliases = graph
			.aliases
			.values()
			.map(CompiledAlias::from_alias)
			.collect::<Vec<_>>();
		aliases.sort_by(|left, right| {
			left.category
				.as_str()
				.cmp(right.category.as_str())
				.then_with(|| left.name.cmp(&right.name))
		});

		Self {
			format_version: PACK_FORMAT_VERSION.to_string(),
			source_id: None,
			roots,
			aliases,
			enums: sorted_string_sets(&graph.enums),
			value_sets: sorted_string_sets(&graph.value_sets),
			scopes: graph.scopes.clone(),
			scope_definitions: sorted_scope_definitions(graph.scope_definitions()),
		}
	}

	pub fn from_schema_pack(pack: &SchemaPack) -> Self {
		let mut compiled = Self::from_graph(pack.graph.as_ref());
		compiled.source_id = Some(pack.id.to_hex());
		compiled
	}

	pub fn to_bytes(&self) -> Result<Vec<u8>, CwtLoadError> {
		bincode::serialize(self).map_err(|error| CwtLoadError::Codec {
			message: error.to_string(),
		})
	}

	pub fn from_bytes(bytes: &[u8]) -> Result<Self, CwtLoadError> {
		let pack: Self = bincode::deserialize(bytes).map_err(|error| CwtLoadError::Codec {
			message: error.to_string(),
		})?;
		if pack.format_version != PACK_FORMAT_VERSION {
			return Err(CwtLoadError::Codec {
				message: format!(
					"compiled pack format `{}` is not supported by `{PACK_FORMAT_VERSION}`",
					pack.format_version
				),
			});
		}
		Ok(pack)
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledRoot {
	pub name: String,
	pub path: Option<String>,
	pub normalized_path: Option<String>,
	pub path_file: Option<String>,
	pub normalized_file_path: Option<String>,
	pub name_field: Option<String>,
	pub type_key_filter: Option<CompiledTypeKeyFilter>,
	pub push_scope: Option<String>,
	pub type_per_file: bool,
	pub name_from_file: bool,
	pub skip_root_keys: Vec<String>,
	pub subtypes: Vec<CompiledSubtype>,
	pub rules: Vec<CompiledRuleField>,
}

impl CompiledRoot {
	fn from_type_def(definition: &CwtTypeDef) -> Self {
		Self {
			name: definition.name.as_str().to_string(),
			path: definition.path.clone(),
			normalized_path: definition.path.as_deref().map(normalize_schema_path),
			path_file: definition.path_file.clone(),
			normalized_file_path: definition
				.path
				.as_deref()
				.zip(definition.path_file.as_deref())
				.map(|(path, path_file)| {
					normalized_schema_file_path(&normalize_schema_path(path), path_file)
				}),
			name_field: definition.name_field.clone(),
			type_key_filter: definition
				.type_key_filter
				.as_ref()
				.map(CompiledTypeKeyFilter::from_schema),
			push_scope: definition.push_scope.clone(),
			type_per_file: definition.type_per_file,
			name_from_file: definition.name_from_file,
			skip_root_keys: definition.skip_root_keys.clone(),
			subtypes: definition
				.subtypes
				.iter()
				.map(CompiledSubtype::from_subtype)
				.collect(),
			rules: definition
				.rules
				.iter()
				.map(CompiledRuleField::from_rule_field)
				.collect(),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledSubtype {
	pub name: String,
	pub attributes: CompiledFieldAttributes,
	pub type_key_filter: Option<CompiledTypeKeyFilter>,
	pub rules: Vec<CompiledRuleField>,
}

impl CompiledSubtype {
	fn from_subtype(subtype: &CwtSubtype) -> Self {
		Self {
			name: subtype.name.clone(),
			attributes: CompiledFieldAttributes::from_attributes(&subtype.attributes),
			type_key_filter: subtype
				.type_key_filter
				.as_ref()
				.map(CompiledTypeKeyFilter::from_schema),
			rules: subtype
				.rules
				.iter()
				.map(CompiledRuleField::from_rule_field)
				.collect(),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CompiledTypeKeyFilter {
	Exact(Vec<String>),
	Exclude(Vec<String>),
}

impl CompiledTypeKeyFilter {
	fn from_schema(filter: &CwtTypeKeyFilter) -> Self {
		match filter {
			CwtTypeKeyFilter::Exact(values) => Self::Exact(values.clone()),
			CwtTypeKeyFilter::Exclude(values) => Self::Exclude(values.clone()),
		}
	}

	fn matches(&self, key: &str) -> bool {
		match self {
			Self::Exact(values) => values.iter().any(|value| value == key),
			Self::Exclude(values) => values.iter().all(|value| value != key),
		}
	}

	fn primary_label(&self) -> Option<&str> {
		match self {
			Self::Exact(values) => values.first().map(String::as_str),
			Self::Exclude(_) => None,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledAlias {
	pub category: CompiledAliasCategory,
	pub name: String,
	pub attributes: CompiledFieldAttributes,
	pub value: CompiledRuleValue,
	pub rules: Vec<CompiledRuleField>,
}

impl CompiledAlias {
	fn from_alias(alias: &CwtAlias) -> Self {
		Self {
			category: CompiledAliasCategory::from_schema(&alias.category),
			name: alias.name.clone(),
			attributes: CompiledFieldAttributes::from_attributes(&alias.attributes),
			value: CompiledRuleValue::from_rule_value(&alias.value),
			rules: alias
				.rules
				.iter()
				.map(CompiledRuleField::from_rule_field)
				.collect(),
		}
	}
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum CompiledAliasCategory {
	Trigger,
	Effect,
	Modifier,
	Link,
	Other(String),
}

impl CompiledAliasCategory {
	pub fn from_name(name: &str) -> Self {
		match name {
			"trigger" => Self::Trigger,
			"effect" => Self::Effect,
			"modifier" => Self::Modifier,
			"link" => Self::Link,
			other => Self::Other(other.to_string()),
		}
	}

	pub fn as_str(&self) -> &str {
		match self {
			Self::Trigger => "trigger",
			Self::Effect => "effect",
			Self::Modifier => "modifier",
			Self::Link => "link",
			Self::Other(name) => name.as_str(),
		}
	}

	fn from_schema(category: &AliasCategory) -> Self {
		match category {
			AliasCategory::Trigger => Self::Trigger,
			AliasCategory::Effect => Self::Effect,
			AliasCategory::Modifier => Self::Modifier,
			AliasCategory::Link => Self::Link,
			AliasCategory::Other(name) => Self::Other(name.clone()),
		}
	}
}

impl Display for CompiledAliasCategory {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledFieldAttributes {
	pub push_scope: Option<String>,
	pub replace_scope: HashMap<String, String>,
	pub scope: Vec<String>,
	pub cardinality: Option<(u32, Option<u32>)>,
	pub severity: Option<CompiledSeverity>,
	pub description: Option<String>,
	pub raw: Vec<(String, String)>,
}

impl CompiledFieldAttributes {
	fn from_attributes(attributes: &CwtFieldAttributes) -> Self {
		Self {
			push_scope: attributes.push_scope.clone(),
			replace_scope: attributes.replace_scope.clone(),
			scope: attributes.scope.clone(),
			cardinality: attributes.cardinality,
			severity: attributes.severity.map(CompiledSeverity::from_schema),
			description: attributes.description.clone(),
			raw: attributes.raw.clone(),
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CompiledSeverity {
	Error,
	Warning,
	Info,
}

impl CompiledSeverity {
	fn from_schema(severity: CwtSeverity) -> Self {
		match severity {
			CwtSeverity::Error => Self::Error,
			CwtSeverity::Warning => Self::Warning,
			CwtSeverity::Info => Self::Info,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledRuleField {
	pub key: String,
	pub value: CompiledRuleValue,
	pub attributes: CompiledFieldAttributes,
}

impl CompiledRuleField {
	fn from_rule_field(field: &CwtRuleField) -> Self {
		Self {
			key: field.key.clone(),
			value: CompiledRuleValue::from_rule_value(&field.value),
			attributes: CompiledFieldAttributes::from_attributes(&field.attributes),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CompiledRuleValue {
	Scalar(String),
	Block(Vec<CompiledRuleField>),
	Marker(String),
}

impl CompiledRuleValue {
	fn from_rule_value(value: &CwtRuleValue) -> Self {
		match value {
			CwtRuleValue::Scalar(value) => Self::Scalar(value.clone()),
			CwtRuleValue::Block(fields) => Self::Block(
				fields
					.iter()
					.map(CompiledRuleField::from_rule_field)
					.collect(),
			),
			CwtRuleValue::Marker(value) => Self::Marker(value.clone()),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledStringSet {
	pub name: String,
	pub values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledScope {
	pub name: String,
	pub aliases: Vec<String>,
	pub is_subscope_of: Vec<String>,
}

pub struct RuleEngine {
	pack: Arc<CompiledRulePack>,
	index: RuntimeRuleIndex,
}

impl RuleEngine {
	pub fn from_graph(graph: &CwtSchemaGraph) -> Self {
		Self::new(CompiledRulePack::from_graph(graph))
	}

	pub fn new(pack: CompiledRulePack) -> Self {
		let pack = Arc::new(pack);
		Self::from_arc(pack)
	}

	pub fn from_arc(pack: Arc<CompiledRulePack>) -> Self {
		let index = RuntimeRuleIndex::new(pack.as_ref());
		Self { pack, index }
	}

	pub fn pack(&self) -> &CompiledRulePack {
		self.pack.as_ref()
	}

	pub fn root_count(&self) -> usize {
		self.pack.roots.len()
	}

	pub fn alias_count(&self) -> usize {
		self.pack.aliases.len()
	}

	pub fn aliases(&self) -> &[CompiledAlias] {
		self.pack.aliases.as_slice()
	}

	pub fn enum_values(&self, name: &str) -> Option<&[String]> {
		self.index
			.enums
			.get(name)
			.map(|index| self.pack.enums[*index].values.as_slice())
	}

	pub fn value_set_values(&self, name: &str) -> Option<&[String]> {
		self.index
			.value_sets
			.get(name)
			.map(|index| self.pack.value_sets[*index].values.as_slice())
	}

	pub fn scope_matches(&self, required_scope: &str, active_scope: &str) -> bool {
		if required_scope == active_scope {
			return true;
		}
		let Some(required_index) = self.index.scope_labels.get(required_scope).copied() else {
			return false;
		};
		let Some(active_index) = self.index.scope_labels.get(active_scope).copied() else {
			return false;
		};
		self.scope_index_matches(required_index, active_index)
	}

	pub fn bind_root(&self, file_path: &Path) -> Option<&CompiledRoot> {
		let SchemaBinding::Bound { type_id, .. } = self.root_binding(file_path) else {
			return None;
		};
		self.pack
			.roots
			.iter()
			.find(|root| root.name == type_id.as_str())
	}

	pub fn root_binding(&self, file_path: &Path) -> SchemaBinding {
		let normalized = normalize_path(file_path);
		match self.matching_root_indices(file_path).as_slice() {
			[] => SchemaBinding::Unbound {
				reason: format!("no root type matches `{normalized}`"),
			},
			[index] => bound_node(&self.pack.roots[*index], Vec::new()),
			_ => SchemaBinding::Dynamic {
				reason: "ambiguous-root-type",
			},
		}
	}

	pub fn bind_chain(&self, file_path: &Path, ast_path: &[&str]) -> SchemaBinding {
		if ast_path.is_empty() {
			return self.root_binding(file_path);
		}
		let matches = self.matching_root_indices(file_path);
		if matches.is_empty() {
			return SchemaBinding::Unbound {
				reason: format!("no root type matches `{}`", normalize_path(file_path)),
			};
		}
		let attempts = matches
			.into_iter()
			.map(|index| self.bind_chain_from_root(&self.pack.roots[index], ast_path))
			.collect::<Vec<_>>();
		let max_consumed = attempts
			.iter()
			.map(|attempt| attempt.consumed)
			.max()
			.unwrap_or(0);
		let best = attempts
			.into_iter()
			.filter(|attempt| attempt.consumed == max_consumed)
			.collect::<Vec<_>>();
		choose_best_binding(best, max_consumed)
	}

	pub fn bind_context<'p>(
		&'p self,
		file_path: &Path,
		ast_path: &[&str],
	) -> Option<RuleContext<'p>> {
		let target = self.bind_chain(file_path, ast_path);
		if !matches!(&target, SchemaBinding::Bound { .. }) {
			return None;
		}
		for index in self.matching_root_indices(file_path) {
			let root = &self.pack.roots[index];
			let Some((context, binding)) = self.bind_context_from_root(root, ast_path) else {
				continue;
			};
			if binding == target {
				return Some(context);
			}
		}
		None
	}

	pub fn bind_fields<'p>(
		&'p self,
		parent: RuleContext<'p>,
		key: &str,
	) -> Vec<&'p CompiledRuleField> {
		let Some(rule_sets) = parent_rule_sets(parent) else {
			return Vec::new();
		};
		let mut matches = self.match_rule_sets_all(&rule_sets, key);
		if !matches.is_empty() {
			return matches;
		}
		for rules in &rule_sets {
			match self.match_alias_rules(rules, key) {
				Some(RuleMatch::Alias { wildcard, .. }) => matches.push(wildcard),
				Some(RuleMatch::Dynamic { .. }) => return Vec::new(),
				_ => {}
			}
		}
		if !matches.is_empty() {
			return matches;
		}
		for rules in &rule_sets {
			match match_dynamic_key_rules(rules, key) {
				Some(RuleMatch::Field(field)) => return vec![field],
				Some(RuleMatch::Dynamic { .. }) => return Vec::new(),
				_ => {}
			}
		}
		matches
	}

	pub fn bind_field_match<'p>(
		&'p self,
		parent: RuleContext<'p>,
		key: &str,
	) -> Option<CompiledBindFieldMatch<'p>> {
		let rule_sets = parent_rule_sets(parent)?;
		for rules in &rule_sets {
			if let Some(field) = rules.iter().find(|field| field.key == key) {
				return Some(CompiledBindFieldMatch::Field(field));
			}
		}
		for rules in &rule_sets {
			match self.match_alias_rules(rules, key) {
				Some(RuleMatch::Alias { wildcard, alias }) => {
					return Some(CompiledBindFieldMatch::Alias { wildcard, alias });
				}
				Some(RuleMatch::Dynamic { .. }) => return None,
				_ => {}
			}
		}
		for rules in &rule_sets {
			match match_dynamic_key_rules(rules, key) {
				Some(RuleMatch::Field(field)) => return Some(CompiledBindFieldMatch::Field(field)),
				Some(RuleMatch::Dynamic { .. }) => return None,
				_ => {}
			}
		}
		None
	}

	pub fn bind_field<'p>(
		&'p self,
		parent: RuleContext<'p>,
		key: &str,
	) -> Option<&'p CompiledRuleField> {
		self.bind_field_match(parent, key)
			.map(|field_match| field_match.field())
	}

	fn matching_root_indices(&self, file_path: &Path) -> Vec<usize> {
		let normalized = normalize_path(file_path);
		let mut matches = self
			.pack
			.roots
			.iter()
			.enumerate()
			.filter_map(|(index, root)| {
				let normalized_path = root.normalized_path.as_deref()?;
				root_path_match_len(
					&normalized,
					normalized_path,
					root.normalized_file_path.as_deref(),
				)
				.map(|match_len| (match_len, root.name.as_str(), index))
			})
			.collect::<Vec<_>>();
		matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
		let Some((best_length, _, _)) = matches.first() else {
			return Vec::new();
		};
		let best_length = *best_length;
		matches
			.into_iter()
			.filter(|(length, _, _)| *length == best_length)
			.map(|(_, _, index)| index)
			.collect()
	}

	fn scope_index_matches(&self, required_index: usize, active_index: usize) -> bool {
		if required_index == active_index {
			return true;
		}
		let mut stack = vec![active_index];
		let mut visited = vec![false; self.pack.scope_definitions.len()];
		while let Some(index) = stack.pop() {
			let Some(visited_entry) = visited.get_mut(index) else {
				continue;
			};
			if *visited_entry {
				continue;
			}
			*visited_entry = true;
			if index == required_index {
				return true;
			}
			let Some(scope) = self.pack.scope_definitions.get(index) else {
				continue;
			};
			for parent in &scope.is_subscope_of {
				if let Some(parent_index) = self.index.scope_labels.get(parent).copied() {
					stack.push(parent_index);
				}
			}
		}
		false
	}

	fn bind_chain_from_root(&self, root: &CompiledRoot, ast_path: &[&str]) -> BindAttempt {
		let mut node = ResolvedNode::Root(root);
		let mut node_path = Vec::new();
		let mut consumed = 0;
		let mut root_instance_consumed = false;
		let mut skip_root_index = 0;
		let mut segments = ast_path.iter().copied().peekable();
		while let Some(key) = segments.next() {
			if let ResolvedNode::Root(definition) = node {
				if let Some(skip_key) = definition.skip_root_keys.get(skip_root_index) {
					if root_skip_key_matches(skip_key, key) {
						skip_root_index += 1;
						consumed += 1;
						continue;
					}
					return BindAttempt {
						consumed,
						binding: SchemaBinding::Unbound {
							reason: format!(
								"expected schema skip_root_key `{skip_key}` under {}",
								describe_node(node)
							),
						},
					};
				}
				match self.match_subtype(definition, key) {
					Some(Ok(subtype)) => {
						node = ResolvedNode::Subtype(definition, subtype);
						node_path.push(format!("subtype:{}", subtype_label(subtype)));
						consumed += 1;
						continue;
					}
					Some(Err(reason)) => {
						return BindAttempt {
							consumed,
							binding: SchemaBinding::Dynamic { reason },
						};
					}
					None => {}
				}
				if !definition.skip_root_keys.is_empty() && !root_instance_consumed {
					if root_type_accepts_instance_key(definition, key) {
						root_instance_consumed = true;
						consumed += 1;
						if segments.peek().is_none() {
							return BindAttempt {
								consumed,
								binding: bound_node(root, node_path),
							};
						}
						continue;
					}
					return BindAttempt {
						consumed,
						binding: SchemaBinding::Unbound {
							reason: format!(
								"expected root instance key under {}",
								describe_node(node)
							),
						},
					};
				}
			}
			if let Some(rule_match) = self.match_rules_for_node(node, key) {
				match rule_match {
					RuleMatch::Field(field) => {
						node = ResolvedNode::Field(field);
						node_path.push(format!("field:{}", field.key));
						consumed += 1;
						continue;
					}
					RuleMatch::Alias { alias, .. } => {
						node = ResolvedNode::Alias(alias);
						node_path.push(format!("alias:{}:{}", alias.category, alias.name));
						consumed += 1;
						continue;
					}
					RuleMatch::Dynamic { reason } => {
						return BindAttempt {
							consumed,
							binding: SchemaBinding::Dynamic { reason },
						};
					}
				}
			}
			if let ResolvedNode::Root(definition) = node
				&& !root_instance_consumed
				&& root_type_accepts_instance_key(definition, key)
			{
				root_instance_consumed = true;
				consumed += 1;
				if segments.peek().is_none() {
					return BindAttempt {
						consumed,
						binding: bound_node(root, node_path),
					};
				}
				continue;
			}
			return BindAttempt {
				consumed,
				binding: SchemaBinding::Unbound {
					reason: format!(
						"no schema field matches `{key}` under {}",
						describe_node(node)
					),
				},
			};
		}
		BindAttempt {
			consumed,
			binding: bound_node(root, node_path),
		}
	}

	fn bind_context_from_root<'p>(
		&'p self,
		root: &'p CompiledRoot,
		ast_path: &[&str],
	) -> Option<(RuleContext<'p>, SchemaBinding)> {
		let mut node = ResolvedNode::Root(root);
		let mut node_path = Vec::new();
		let mut root_instance_consumed = false;
		let mut skip_root_index = 0;
		let mut segments = ast_path.iter().copied().peekable();
		while let Some(key) = segments.next() {
			if let ResolvedNode::Root(definition) = node {
				if let Some(skip_key) = definition.skip_root_keys.get(skip_root_index) {
					if root_skip_key_matches(skip_key, key) {
						skip_root_index += 1;
						continue;
					}
					return None;
				}
				match self.match_subtype(definition, key) {
					Some(Ok(subtype)) => {
						node = ResolvedNode::Subtype(definition, subtype);
						node_path.push(format!("subtype:{}", subtype_label(subtype)));
						continue;
					}
					Some(Err(_)) => return None,
					None => {}
				}
				if !definition.skip_root_keys.is_empty() && !root_instance_consumed {
					if root_type_accepts_instance_key(definition, key) {
						root_instance_consumed = true;
						if segments.peek().is_none() {
							return Some((
								RuleContext::RootType(root),
								bound_node(root, node_path),
							));
						}
						continue;
					}
					return None;
				}
			}
			if let Some(rule_match) = self.match_rules_for_node(node, key) {
				match rule_match {
					RuleMatch::Field(field) => {
						node = ResolvedNode::Field(field);
						node_path.push(format!("field:{}", field.key));
						continue;
					}
					RuleMatch::Alias { alias, .. } => {
						node = ResolvedNode::Alias(alias);
						node_path.push(format!("alias:{}:{}", alias.category, alias.name));
						continue;
					}
					RuleMatch::Dynamic { .. } => return None,
				}
			}
			if let ResolvedNode::Root(definition) = node
				&& !root_instance_consumed
				&& root_type_accepts_instance_key(definition, key)
			{
				root_instance_consumed = true;
				if segments.peek().is_none() {
					return Some((RuleContext::RootType(root), bound_node(root, node_path)));
				}
				continue;
			}
			return None;
		}
		Some((context_for_node(node), bound_node(root, node_path)))
	}

	fn match_rules_for_node<'p>(
		&'p self,
		node: ResolvedNode<'p>,
		key: &str,
	) -> Option<RuleMatch<'p>> {
		match node {
			ResolvedNode::Root(definition) => {
				self.match_rule_sets(&[definition.rules.as_slice()], key)
			}
			ResolvedNode::Subtype(definition, subtype) => self.match_rule_sets(
				&[subtype.rules.as_slice(), definition.rules.as_slice()],
				key,
			),
			ResolvedNode::Field(field) => self.match_rule_sets(&[field_rules(field)?], key),
			ResolvedNode::Alias(alias) => self.match_rule_sets(&[alias.rules.as_slice()], key),
		}
	}

	fn match_rule_sets<'p>(
		&'p self,
		rule_sets: &[&'p [CompiledRuleField]],
		key: &str,
	) -> Option<RuleMatch<'p>> {
		for rules in rule_sets {
			if let Some(field) = rules.iter().find(|field| field.key == key) {
				return Some(RuleMatch::Field(field));
			}
		}
		for rules in rule_sets {
			if let Some(alias_match) = self.match_alias_rules(rules, key) {
				return Some(alias_match);
			}
		}
		for rules in rule_sets {
			if let Some(dynamic_match) = match_dynamic_key_rules(rules, key) {
				return Some(dynamic_match);
			}
		}
		None
	}

	fn match_rule_sets_all<'p>(
		&'p self,
		rule_sets: &[&'p [CompiledRuleField]],
		key: &str,
	) -> Vec<&'p CompiledRuleField> {
		let mut matches = Vec::new();
		for rules in rule_sets {
			matches.extend(rules.iter().filter(|field| field.key == key));
		}
		matches
	}

	fn match_alias_rules<'p>(
		&'p self,
		rules: &'p [CompiledRuleField],
		key: &str,
	) -> Option<RuleMatch<'p>> {
		let mut matches = Vec::new();
		for field in rules {
			let Some((head, payload)) = parse_marker(&field.key) else {
				continue;
			};
			if head != "alias_name" {
				continue;
			}
			let category = CompiledAliasCategory::from_name(payload);
			let Some(alias_index) = self.index.aliases.get(&(category, key.to_string())) else {
				continue;
			};
			matches.push((field, &self.pack.aliases[*alias_index]));
		}
		match matches.as_slice() {
			[] => None,
			[(wildcard, alias)] => Some(RuleMatch::Alias { wildcard, alias }),
			_ => Some(RuleMatch::Dynamic {
				reason: "ambiguous-alias-match",
			}),
		}
	}

	fn match_subtype<'p>(
		&'p self,
		definition: &'p CompiledRoot,
		key: &str,
	) -> Option<Result<&'p CompiledSubtype, &'static str>> {
		let matches = definition
			.subtypes
			.iter()
			.filter(|subtype| subtype_matches(subtype, key))
			.collect::<Vec<_>>();
		match matches.as_slice() {
			[] => None,
			[subtype] => Some(Ok(*subtype)),
			_ => Some(Err("ambiguous-subtype")),
		}
	}
}

#[derive(Clone, Copy, Debug)]
pub enum RuleContext<'p> {
	RootType(&'p CompiledRoot),
	Subtype(&'p CompiledRoot, &'p CompiledSubtype),
	RuleField(&'p CompiledRuleField),
	AliasRules(&'p [CompiledRuleField]),
}

#[derive(Clone, Copy, Debug)]
pub enum CompiledBindFieldMatch<'p> {
	Field(&'p CompiledRuleField),
	Alias {
		wildcard: &'p CompiledRuleField,
		alias: &'p CompiledAlias,
	},
}

impl<'p> CompiledBindFieldMatch<'p> {
	pub fn field(&self) -> &'p CompiledRuleField {
		match self {
			Self::Field(field)
			| Self::Alias {
				wildcard: field, ..
			} => field,
		}
	}

	pub fn alias(&self) -> Option<&'p CompiledAlias> {
		match self {
			Self::Field(_) => None,
			Self::Alias { alias, .. } => Some(alias),
		}
	}
}

struct RuntimeRuleIndex {
	aliases: HashMap<(CompiledAliasCategory, String), usize>,
	enums: HashMap<String, usize>,
	value_sets: HashMap<String, usize>,
	scope_labels: HashMap<String, usize>,
}

impl RuntimeRuleIndex {
	fn new(pack: &CompiledRulePack) -> Self {
		let aliases = pack
			.aliases
			.iter()
			.enumerate()
			.map(|(index, alias)| ((alias.category.clone(), alias.name.clone()), index))
			.collect();
		let enums = pack
			.enums
			.iter()
			.enumerate()
			.map(|(index, values)| (values.name.clone(), index))
			.collect();
		let value_sets = pack
			.value_sets
			.iter()
			.enumerate()
			.map(|(index, values)| (values.name.clone(), index))
			.collect();
		let mut scope_labels = HashMap::new();
		for (index, scope) in pack.scope_definitions.iter().enumerate() {
			scope_labels.entry(scope.name.clone()).or_insert(index);
			for alias in &scope.aliases {
				scope_labels.entry(alias.clone()).or_insert(index);
			}
		}
		Self {
			aliases,
			enums,
			value_sets,
			scope_labels,
		}
	}
}

#[derive(Clone, Copy)]
enum ResolvedNode<'p> {
	Root(&'p CompiledRoot),
	Subtype(&'p CompiledRoot, &'p CompiledSubtype),
	Field(&'p CompiledRuleField),
	Alias(&'p CompiledAlias),
}

enum RuleMatch<'p> {
	Field(&'p CompiledRuleField),
	Alias {
		wildcard: &'p CompiledRuleField,
		alias: &'p CompiledAlias,
	},
	Dynamic {
		reason: &'static str,
	},
}

#[derive(Clone)]
struct BindAttempt {
	consumed: usize,
	binding: SchemaBinding,
}

fn choose_best_binding(attempts: Vec<BindAttempt>, max_consumed: usize) -> SchemaBinding {
	let Some(first) = attempts.first() else {
		return SchemaBinding::Dynamic {
			reason: "ambiguous-binding",
		};
	};
	if attempts
		.iter()
		.all(|attempt| attempt.binding == first.binding)
	{
		return first.binding.clone();
	}
	let bound = attempts
		.iter()
		.filter(|attempt| matches!(attempt.binding, SchemaBinding::Bound { .. }))
		.collect::<Vec<_>>();
	if bound.len() == 1 {
		return bound[0].binding.clone();
	}
	let dynamic = attempts
		.iter()
		.filter(|attempt| matches!(attempt.binding, SchemaBinding::Dynamic { .. }))
		.collect::<Vec<_>>();
	if bound.is_empty() && dynamic.len() == 1 {
		return dynamic[0].binding.clone();
	}
	if max_consumed == 0 {
		SchemaBinding::Dynamic {
			reason: "ambiguous-root-type",
		}
	} else {
		SchemaBinding::Dynamic {
			reason: "ambiguous-binding",
		}
	}
}

fn parent_rule_sets(parent: RuleContext<'_>) -> Option<Vec<&[CompiledRuleField]>> {
	match parent {
		RuleContext::RootType(root) => Some(vec![root.rules.as_slice()]),
		RuleContext::Subtype(root, subtype) => {
			Some(vec![subtype.rules.as_slice(), root.rules.as_slice()])
		}
		RuleContext::RuleField(field) => Some(vec![field_rules(field)?]),
		RuleContext::AliasRules(rules) => Some(vec![rules]),
	}
}

fn context_for_node(node: ResolvedNode<'_>) -> RuleContext<'_> {
	match node {
		ResolvedNode::Root(root) => RuleContext::RootType(root),
		ResolvedNode::Subtype(root, subtype) => RuleContext::Subtype(root, subtype),
		ResolvedNode::Field(field) => RuleContext::RuleField(field),
		ResolvedNode::Alias(alias) => RuleContext::AliasRules(alias.rules.as_slice()),
	}
}

fn field_rules(field: &CompiledRuleField) -> Option<&[CompiledRuleField]> {
	let CompiledRuleValue::Block(fields) = &field.value else {
		return None;
	};
	Some(fields.as_slice())
}

fn root_path_match_len(
	file_path: &str,
	normalized_root_path: &str,
	normalized_file_path: Option<&str>,
) -> Option<usize> {
	if let Some(normalized_file_path) = normalized_file_path {
		return (file_path == normalized_file_path).then_some(normalized_file_path.len());
	}
	(file_path == normalized_root_path
		|| file_path.starts_with(&format!("{normalized_root_path}/")))
	.then_some(normalized_root_path.len())
}

fn root_skip_key_matches(skip_key: &str, key: &str) -> bool {
	skip_key == "any" || skip_key == key
}

fn match_dynamic_key_rules<'p>(rules: &'p [CompiledRuleField], key: &str) -> Option<RuleMatch<'p>> {
	if is_dynamic_key_marker(key) {
		return None;
	}
	let matches = rules
		.iter()
		.filter(|field| is_dynamic_key_marker(&field.key))
		.collect::<Vec<_>>();
	match matches.as_slice() {
		[] => None,
		[field] => Some(RuleMatch::Field(field)),
		_ => Some(RuleMatch::Dynamic {
			reason: "ambiguous-dynamic-field-match",
		}),
	}
}

fn is_dynamic_key_marker(key: &str) -> bool {
	key.len() > 2
		&& key.starts_with('<')
		&& key.ends_with('>')
		&& !key.chars().any(char::is_whitespace)
}

fn subtype_matches(subtype: &CompiledSubtype, key: &str) -> bool {
	subtype.name == key
		|| subtype
			.type_key_filter
			.as_ref()
			.is_some_and(|filter| filter.matches(key))
}

fn subtype_label(subtype: &CompiledSubtype) -> &str {
	subtype
		.type_key_filter
		.as_ref()
		.and_then(|filter| filter.primary_label())
		.unwrap_or(&subtype.name)
}

fn root_type_accepts_instance_key(definition: &CompiledRoot, key: &str) -> bool {
	definition.type_key_filter.as_ref().map_or_else(
		|| definition.subtypes.is_empty(),
		|filter| filter.matches(key),
	)
}

fn describe_node(node: ResolvedNode<'_>) -> String {
	match node {
		ResolvedNode::Root(definition) => format!("root type `{}`", definition.name),
		ResolvedNode::Subtype(definition, subtype) => format!(
			"subtype `{}` of type `{}`",
			subtype_label(subtype),
			definition.name
		),
		ResolvedNode::Field(field) => format!("field `{}`", field.key),
		ResolvedNode::Alias(alias) => {
			format!("alias `{}` in category `{}`", alias.name, alias.category)
		}
	}
}

fn bound_node(definition: &CompiledRoot, path: Vec<String>) -> SchemaBinding {
	let node_id = if path.is_empty() {
		CwtNodeId(format!("type:{}:root", definition.name))
	} else {
		CwtNodeId(format!("type:{}:{}", definition.name, path.join("/")))
	};
	SchemaBinding::Bound {
		type_id: CwtType::new(definition.name.clone()),
		node_id,
	}
}

fn parse_marker(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

fn sorted_string_sets(map: &HashMap<String, Vec<String>>) -> Vec<CompiledStringSet> {
	let mut sets = map
		.iter()
		.map(|(name, values)| {
			let mut values = values.clone();
			values.sort();
			values.dedup();
			CompiledStringSet {
				name: name.clone(),
				values,
			}
		})
		.collect::<Vec<_>>();
	sets.sort_by(|left, right| left.name.cmp(&right.name));
	sets
}

fn sorted_scope_definitions(definitions: &[CwtScope]) -> Vec<CompiledScope> {
	let mut by_name = BTreeMap::<String, CompiledScope>::new();
	for definition in definitions {
		let scope = by_name
			.entry(definition.name.clone())
			.or_insert_with(|| CompiledScope {
				name: definition.name.clone(),
				aliases: Vec::new(),
				is_subscope_of: Vec::new(),
			});
		scope.aliases.extend(definition.aliases.iter().cloned());
		scope
			.is_subscope_of
			.extend(definition.is_subscope_of.iter().cloned());
	}
	for scope in by_name.values_mut() {
		scope.aliases.sort();
		scope.aliases.dedup();
		scope.is_subscope_of.sort();
		scope.is_subscope_of.dedup();
	}
	by_name.into_values().collect()
}

fn normalize_schema_path(path: &str) -> String {
	path.trim_start_matches("game/")
		.trim_matches('/')
		.to_ascii_lowercase()
}

fn normalized_schema_file_path(normalized_root_path: &str, path_file: &str) -> String {
	let path_file = normalize_schema_path(path_file);
	if normalized_root_path.is_empty() {
		path_file
	} else {
		format!("{normalized_root_path}/{path_file}")
	}
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy()
		.replace('\\', "/")
		.trim_matches('/')
		.to_ascii_lowercase()
}
