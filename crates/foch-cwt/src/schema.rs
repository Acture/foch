use std::collections::{BTreeSet, HashMap};
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use foch_syntax::{CommentKind, ParadoxNode, ParadoxScalar, ParadoxTree};
use walkdir::WalkDir;

use crate::error::CwtLoadError;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CwtType(String);

impl CwtType {
	pub fn new(name: impl Into<String>) -> Self {
		Self(name.into())
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl Display for CwtType {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AliasCategory {
	Trigger,
	Effect,
	Modifier,
	Link,
	Other(String),
}

impl AliasCategory {
	pub fn from_name(name: &str) -> Self {
		match name {
			"trigger" => Self::Trigger,
			"effect" => Self::Effect,
			"modifier" => Self::Modifier,
			"link" => Self::Link,
			other => Self::Other(other.to_string()),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtAlias {
	pub category: AliasCategory,
	pub name: String,
	pub attributes: CwtFieldAttributes,
	pub value: CwtRuleValue,
	pub rules: Vec<CwtRuleField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtSubtype {
	pub name: String,
	pub type_key_filter: Option<CwtTypeKeyFilter>,
	pub rules: Vec<CwtRuleField>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CwtSeverity {
	Error,
	Warning,
	Info,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtTypeKeyFilter {
	Exact(Vec<String>),
	Exclude(Vec<String>),
}

impl CwtTypeKeyFilter {
	pub fn matches(&self, key: &str) -> bool {
		match self {
			Self::Exact(values) => values.iter().any(|value| value == key),
			Self::Exclude(values) => values.iter().all(|value| value != key),
		}
	}

	pub fn primary_label(&self) -> Option<&str> {
		match self {
			Self::Exact(values) => values.first().map(String::as_str),
			Self::Exclude(_) => None,
		}
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CwtFieldAttributes {
	pub push_scope: Option<String>,
	pub replace_scope: HashMap<String, String>,
	pub scope: Vec<String>,
	pub cardinality: Option<(u32, Option<u32>)>,
	pub severity: Option<CwtSeverity>,
	pub description: Option<String>,
	pub raw: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtRuleField {
	pub key: String,
	pub value: CwtRuleValue,
	pub attributes: CwtFieldAttributes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtRuleValue {
	Scalar(String),
	Block(Vec<CwtRuleField>),
	Marker(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtTypeDef {
	pub name: CwtType,
	pub path: Option<String>,
	pub name_field: Option<String>,
	pub type_key_filter: Option<CwtTypeKeyFilter>,
	pub push_scope: Option<String>,
	pub type_per_file: bool,
	pub name_from_file: bool,
	pub skip_root_keys: Vec<String>,
	pub subtypes: Vec<CwtSubtype>,
	pub rules: Vec<CwtRuleField>,
}

impl CwtTypeDef {
	fn new(name: CwtType) -> Self {
		Self {
			name,
			path: None,
			name_field: None,
			type_key_filter: None,
			push_scope: None,
			type_per_file: false,
			name_from_file: false,
			skip_root_keys: Vec::new(),
			subtypes: Vec::new(),
			rules: Vec::new(),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtScope {
	pub name: String,
	pub aliases: Vec<String>,
	pub is_subscope_of: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CwtSchemaGraph {
	pub types: HashMap<CwtType, CwtTypeDef>,
	pub aliases: HashMap<(AliasCategory, String), CwtAlias>,
	pub enums: HashMap<String, Vec<String>>,
	pub value_sets: HashMap<String, Vec<String>>,
	pub scopes: Vec<String>,
	scope_definitions: Vec<CwtScope>,
}

impl CwtSchemaGraph {
	pub fn from_paradox_tree(tree: &ParadoxTree) -> Self {
		Self::try_from_paradox_tree(tree).expect("projecting ParadoxTree into CwtSchemaGraph")
	}

	pub fn from_directory(dir: &Path) -> Result<Self, CwtLoadError> {
		let mut graph = Self::default();
		for path in cwt_files(dir)? {
			let bytes = std::fs::read(&path).map_err(|source| CwtLoadError::Io {
				path: path.clone(),
				source,
			})?;
			let tree = ParadoxTree::parse(&bytes)?;
			graph.ingest_tree(path.strip_prefix(dir).ok(), &tree)?;
		}
		graph.finalize_scopes();
		Ok(graph)
	}

	pub(crate) fn try_from_paradox_tree(tree: &ParadoxTree) -> Result<Self, CwtLoadError> {
		let mut graph = Self::default();
		graph.ingest_tree(None, tree)?;
		graph.finalize_scopes();
		Ok(graph)
	}

	pub(crate) fn scope_definitions(&self) -> &[CwtScope] {
		&self.scope_definitions
	}

	fn ingest_tree(
		&mut self,
		relative: Option<&Path>,
		tree: &ParadoxTree,
	) -> Result<(), CwtLoadError> {
		let nodes = tree.nodes()?;
		let mut pending_doc_comments = Vec::new();
		for node in &nodes {
			match node {
				ParadoxNode::Comment {
					text,
					kind: CommentKind::DocAttribute,
					..
				} => pending_doc_comments.push((*text).to_string()),
				ParadoxNode::Comment { .. } => pending_doc_comments.clear(),
				ParadoxNode::Assignment { key, value, .. } => {
					let key = key.as_text();
					self.ingest_top_level_assignment(
						relative,
						key.as_ref(),
						value,
						std::mem::take(&mut pending_doc_comments),
					);
				}
				_ => pending_doc_comments.clear(),
			}
		}
		Ok(())
	}

	fn ingest_top_level_assignment(
		&mut self,
		relative: Option<&Path>,
		key: &str,
		value: &ParadoxNode<'_>,
		doc_comments: Vec<String>,
	) {
		if key == "types" {
			if let Some(items) = block_items(value) {
				self.ingest_types_block(items);
			}
			return;
		}
		if key == "scopes" {
			if let Some(items) = block_items(value) {
				self.ingest_scopes_block(items);
			}
			return;
		}
		if let Some(marker) = ParsedMarker::parse(key) {
			match marker.head {
				"alias" | "alias_name" => {
					self.insert_alias(&marker, value, parse_field_attributes(doc_comments));
				}
				"enum" => insert_enumeration(&mut self.enums, marker.payload, value),
				"value_set" => insert_enumeration(&mut self.value_sets, marker.payload, value),
				"type" => self.merge_type_header(
					marker.payload,
					value,
					parse_type_key_filter(&doc_comments),
				),
				_ => {}
			}
			return;
		}
		if let Some(items) = block_items(value) {
			self.merge_type_body(CwtType::new(key), items, relative);
		}
	}

	fn ingest_types_block(&mut self, items: &[ParadoxNode<'_>]) {
		let mut pending_doc_comments = Vec::new();
		for item in items {
			match item {
				ParadoxNode::Comment {
					text,
					kind: CommentKind::DocAttribute,
					..
				} => pending_doc_comments.push((*text).to_string()),
				ParadoxNode::Comment { .. } => pending_doc_comments.clear(),
				_ => {
					let Some((key, value)) = assignment_parts(item) else {
						pending_doc_comments.clear();
						continue;
					};
					let Some(marker) = ParsedMarker::parse(&key) else {
						pending_doc_comments.clear();
						continue;
					};
					if marker.head == "type" {
						self.merge_type_header(
							marker.payload,
							value,
							parse_type_key_filter(&pending_doc_comments),
						);
					}
					pending_doc_comments.clear();
				}
			}
		}
	}

	fn merge_type_header(
		&mut self,
		name: &str,
		value: &ParadoxNode<'_>,
		type_key_filter: Option<CwtTypeKeyFilter>,
	) {
		let entry = self
			.types
			.entry(CwtType::new(name))
			.or_insert_with_key(|name| CwtTypeDef::new(name.clone()));
		if let Some(type_key_filter) = type_key_filter {
			entry.type_key_filter = Some(type_key_filter);
		}
		let Some(items) = block_items(value) else {
			return;
		};
		merge_type_items(entry, items, true);
	}

	fn merge_type_body(
		&mut self,
		name: CwtType,
		items: &[ParadoxNode<'_>],
		relative: Option<&Path>,
	) {
		let entry = self
			.types
			.entry(name)
			.or_insert_with_key(|name| CwtTypeDef::new(name.clone()));
		if entry.path.is_none() {
			entry.path = relative.and_then(|path| path.parent()).map(normalize_path);
		}
		merge_type_items(entry, items, false);
	}

	fn insert_alias(
		&mut self,
		marker: &ParsedMarker<'_>,
		value: &ParadoxNode<'_>,
		attributes: CwtFieldAttributes,
	) {
		let (category_name, alias_name) = marker
			.payload
			.split_once(':')
			.map_or((marker.payload, marker.payload), |(category, name)| {
				(category, name)
			});
		let category = AliasCategory::from_name(category_name);
		self.aliases.insert(
			(category.clone(), alias_name.to_string()),
			CwtAlias {
				category,
				name: alias_name.to_string(),
				attributes,
				value: node_to_rule_value(value),
				rules: block_to_rules(value),
			},
		);
	}

	fn ingest_scopes_block(&mut self, items: &[ParadoxNode<'_>]) {
		for item in items {
			let Some((key, value)) = assignment_parts(item) else {
				continue;
			};
			let Some(scope_items) = block_items(value) else {
				continue;
			};
			let aliases = scope_items
				.iter()
				.find_map(|item| {
					assignment_parts(item).and_then(|(child_key, child_value)| {
						(child_key == "aliases").then(|| collect_scalar_items(child_value))
					})
				})
				.unwrap_or_default();
			let parents = scope_items
				.iter()
				.find_map(|item| {
					assignment_parts(item).and_then(|(child_key, child_value)| {
						(child_key == "is_subscope_of").then(|| collect_scalar_items(child_value))
					})
				})
				.unwrap_or_default();
			self.scope_definitions.push(CwtScope {
				name: key.to_string(),
				aliases,
				is_subscope_of: parents,
			});
		}
	}

	fn finalize_scopes(&mut self) {
		let mut scopes = BTreeSet::new();
		for scope in &self.scope_definitions {
			for alias in &scope.aliases {
				scopes.insert(alias.clone());
			}
		}
		self.scopes = scopes.into_iter().collect();
	}
}

#[derive(Clone, Copy, Debug)]
struct ParsedMarker<'source> {
	head: &'source str,
	payload: &'source str,
}

impl<'source> ParsedMarker<'source> {
	fn parse(text: &'source str) -> Option<Self> {
		let (head, rest) = text.split_once('[')?;
		let payload = rest.strip_suffix(']')?;
		Some(Self { head, payload })
	}
}

fn merge_type_items(entry: &mut CwtTypeDef, items: &[ParadoxNode<'_>], header_fields: bool) {
	let mut pending_doc_comments = Vec::new();
	for item in items {
		match item {
			ParadoxNode::Comment {
				text,
				kind: CommentKind::DocAttribute,
				..
			} => {
				pending_doc_comments.push((*text).to_string());
			}
			ParadoxNode::Comment { .. } => pending_doc_comments.clear(),
			_ => {
				let Some((key, child_value)) = assignment_parts(item) else {
					pending_doc_comments.clear();
					continue;
				};
				if header_fields {
					match key.as_str() {
						"path" => entry.path = scalar_text(child_value).map(normalize_schema_path),
						"name_field" => entry.name_field = scalar_text(child_value),
						"push_scope" => entry.push_scope = scalar_text(child_value),
						"type_per_file" => {
							entry.type_per_file = scalar_bool(child_value).unwrap_or(false)
						}
						"name_from_file" => {
							entry.name_from_file = scalar_bool(child_value).unwrap_or(false)
						}
						"skip_root_key" => {
							merge_unique(
								&mut entry.skip_root_keys,
								collect_scalar_items(child_value),
							);
						}
						_ => {}
					}
					if matches!(
						key.as_str(),
						"path"
							| "name_field" | "push_scope"
							| "type_per_file" | "name_from_file"
							| "skip_root_key"
					) {
						pending_doc_comments.clear();
						continue;
					}
				}
				if let Some(marker) = ParsedMarker::parse(&key)
					&& marker.head == "subtype"
				{
					entry.subtypes.push(CwtSubtype {
						name: marker.payload.to_string(),
						type_key_filter: parse_type_key_filter(&pending_doc_comments),
						rules: block_to_rules(child_value),
					});
				} else {
					let mut fields = statement_to_rule_fields(item);
					if let Some(field) = fields.first_mut() {
						field.attributes =
							parse_field_attributes(std::mem::take(&mut pending_doc_comments));
					}
					entry.rules.extend(fields);
				}
				pending_doc_comments.clear();
			}
		}
	}
}

fn cwt_files(root: &Path) -> Result<Vec<PathBuf>, CwtLoadError> {
	let mut files = WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("cwt"))
		.map(|entry| entry.into_path())
		.collect::<Vec<_>>();
	files.sort_by_key(|path| normalize_path(path));
	Ok(files)
}

fn assignment_parts<'source>(
	node: &'source ParadoxNode<'source>,
) -> Option<(String, &'source ParadoxNode<'source>)> {
	let ParadoxNode::Assignment { key, value, .. } = node else {
		return None;
	};
	Some((key.as_text().into_owned(), value.as_ref()))
}

fn block_items<'source>(
	node: &'source ParadoxNode<'source>,
) -> Option<&'source [ParadoxNode<'source>]> {
	match node {
		ParadoxNode::Block { items, .. } | ParadoxNode::Array { items, .. } => {
			Some(items.as_slice())
		}
		ParadoxNode::Item { value, .. } => block_items(value),
		_ => None,
	}
}

fn rule_field(key: String, value: CwtRuleValue) -> CwtRuleField {
	CwtRuleField {
		key,
		value,
		attributes: CwtFieldAttributes::default(),
	}
}

fn block_to_rules(node: &ParadoxNode<'_>) -> Vec<CwtRuleField> {
	let Some(items) = block_items(node) else {
		return Vec::new();
	};
	let mut rules = Vec::new();
	let mut pending_doc_comments = Vec::new();
	for item in items {
		match item {
			ParadoxNode::Comment {
				text,
				kind: CommentKind::DocAttribute,
				..
			} => pending_doc_comments.push((*text).to_string()),
			ParadoxNode::Comment { .. } => pending_doc_comments.clear(),
			_ => {
				let mut fields = statement_to_rule_fields(item);
				if let Some(field) = fields.first_mut() {
					field.attributes =
						parse_field_attributes(std::mem::take(&mut pending_doc_comments));
				}
				rules.extend(fields);
				pending_doc_comments.clear();
			}
		}
	}
	rules
}

fn statement_to_rule_fields(node: &ParadoxNode<'_>) -> Vec<CwtRuleField> {
	match node {
		ParadoxNode::Assignment { key, value, .. } => {
			vec![rule_field(
				key.as_text().into_owned(),
				node_to_rule_value(value),
			)]
		}
		ParadoxNode::Condition { keyword, body, .. }
		| ParadoxNode::Logical { keyword, body, .. }
		| ParadoxNode::Scope { keyword, body, .. } => {
			vec![rule_field((*keyword).to_string(), node_to_rule_value(body))]
		}
		ParadoxNode::MacroMap { key, items, .. } => vec![rule_field(
			key.as_text().into_owned(),
			CwtRuleValue::Block(items.iter().flat_map(statement_to_rule_fields).collect()),
		)],
		ParadoxNode::Item { value, .. } => statement_to_rule_fields(value),
		ParadoxNode::Comment { .. }
		| ParadoxNode::Scalar(_)
		| ParadoxNode::Block { .. }
		| ParadoxNode::Array { .. }
		| ParadoxNode::CwtMarker { .. } => Vec::new(),
	}
}

fn node_to_rule_value(node: &ParadoxNode<'_>) -> CwtRuleValue {
	match node {
		ParadoxNode::Block { .. } | ParadoxNode::Array { .. } => {
			CwtRuleValue::Block(block_to_rules(node))
		}
		ParadoxNode::Scalar(value) => CwtRuleValue::Scalar(value.as_text().into_owned()),
		ParadoxNode::CwtMarker { payload, .. } => CwtRuleValue::Marker((*payload).to_string()),
		ParadoxNode::Item { value, .. } => node_to_rule_value(value),
		ParadoxNode::Condition { keyword, body, .. }
		| ParadoxNode::Logical { keyword, body, .. }
		| ParadoxNode::Scope { keyword, body, .. } => CwtRuleValue::Block(vec![rule_field(
			(*keyword).to_string(),
			node_to_rule_value(body),
		)]),
		ParadoxNode::MacroMap { key, items, .. } => CwtRuleValue::Block(vec![rule_field(
			key.as_text().into_owned(),
			CwtRuleValue::Block(items.iter().flat_map(statement_to_rule_fields).collect()),
		)]),
		ParadoxNode::Comment { text, .. } => CwtRuleValue::Scalar((*text).to_string()),
		ParadoxNode::Assignment { .. } => CwtRuleValue::Block(statement_to_rule_fields(node)),
	}
}

fn parse_field_attributes(comments: Vec<String>) -> CwtFieldAttributes {
	let mut attributes = CwtFieldAttributes::default();
	for comment in comments {
		let text = comment.trim();
		if text.is_empty() {
			continue;
		}
		if let Some((key, raw_value)) = split_attribute_assignment(text) {
			let key = key.trim();
			let value = raw_value.trim();
			match key {
				"push_scope" => attributes.push_scope = Some(value.to_string()),
				"replace_scope" | "replace_scopes" => {
					if let Some(replace_scope) = parse_scope_map(value) {
						attributes.replace_scope = replace_scope;
					} else {
						attributes.raw.push((key.to_string(), value.to_string()));
					}
				}
				"scope" => {
					let scope = parse_scope_list(value);
					if scope.is_empty() {
						attributes.raw.push((key.to_string(), value.to_string()));
					} else {
						attributes.scope = scope;
					}
				}
				"cardinality" => {
					if let Some(cardinality) = parse_cardinality(value) {
						attributes.cardinality = Some(cardinality);
					} else {
						attributes.raw.push((key.to_string(), value.to_string()));
					}
				}
				"severity" => {
					if let Some(severity) = parse_severity(value) {
						attributes.severity = Some(severity);
					} else {
						attributes.raw.push((key.to_string(), value.to_string()));
					}
				}
				"description" => append_description(&mut attributes, value),
				_ => attributes.raw.push((key.to_string(), value.to_string())),
			}
			continue;
		}
		if looks_like_flag_attribute(text) {
			attributes.raw.push((text.to_string(), String::new()));
		} else {
			append_description(&mut attributes, text);
		}
	}
	attributes
}

fn parse_type_key_filter(comments: &[String]) -> Option<CwtTypeKeyFilter> {
	let mut filter = None;
	for comment in comments {
		let text = comment.trim();
		let Some((operator, raw_value)) = text
			.strip_prefix("type_key_filter")
			.map(str::trim_start)
			.and_then(|rest| {
				if let Some(value) = rest.strip_prefix("<>") {
					Some(("<>", value))
				} else {
					rest.strip_prefix('=').map(|value| ("=", value))
				}
			})
		else {
			continue;
		};
		let values = parse_type_key_filter_values(raw_value.trim());
		if values.is_empty() {
			continue;
		}
		filter = Some(match operator {
			"<>" => CwtTypeKeyFilter::Exclude(values),
			"=" => CwtTypeKeyFilter::Exact(values),
			_ => unreachable!("type_key_filter parser only emits known operators"),
		});
	}
	filter
}

fn parse_type_key_filter_values(value: &str) -> Vec<String> {
	let value = strip_braces(value);
	if value.is_empty() {
		return Vec::new();
	}
	value
		.split_whitespace()
		.filter_map(|token| {
			let token = token.trim();
			(!token.is_empty()).then(|| token.to_string())
		})
		.collect()
}

fn split_attribute_assignment(text: &str) -> Option<(&str, &str)> {
	let (key, value) = text.split_once('=')?;
	Some((key.trim(), value.trim()))
}

fn parse_scope_list(value: &str) -> Vec<String> {
	let value = strip_braces(value);
	if value.is_empty() {
		return Vec::new();
	}
	value.split_whitespace().map(ToString::to_string).collect()
}

fn parse_scope_map(value: &str) -> Option<HashMap<String, String>> {
	let tokens = strip_braces(value).split_whitespace().collect::<Vec<_>>();
	if tokens.is_empty() {
		return Some(HashMap::new());
	}
	let mut mappings = HashMap::new();
	let mut index = 0;
	while index < tokens.len() {
		let (key, equals, value) = match tokens.get(index..index + 3) {
			Some([key, equals, value]) => (*key, *equals, *value),
			_ => return None,
		};
		if equals != "=" {
			return None;
		}
		mappings.insert(key.to_string(), value.to_string());
		index += 3;
	}
	Some(mappings)
}

fn parse_cardinality(value: &str) -> Option<(u32, Option<u32>)> {
	let (minimum, maximum) = value.split_once("..")?;
	let minimum = minimum.trim().parse().ok()?;
	let maximum = match maximum.trim() {
		"inf" => None,
		value => Some(value.parse().ok()?),
	};
	Some((minimum, maximum))
}

fn parse_severity(value: &str) -> Option<CwtSeverity> {
	match value.trim() {
		"error" => Some(CwtSeverity::Error),
		"warning" => Some(CwtSeverity::Warning),
		"info" => Some(CwtSeverity::Info),
		_ => None,
	}
}

fn append_description(attributes: &mut CwtFieldAttributes, text: &str) {
	if text.is_empty() {
		return;
	}
	match &mut attributes.description {
		Some(existing) => {
			existing.push('\n');
			existing.push_str(text);
		}
		None => attributes.description = Some(text.to_string()),
	}
}

fn looks_like_flag_attribute(text: &str) -> bool {
	text.split_whitespace().count() == 1
		&& text
			.chars()
			.all(|character| character.is_ascii_alphanumeric() || "_:-[]<>".contains(character))
}

fn strip_braces(text: &str) -> &str {
	text.trim()
		.strip_prefix('{')
		.and_then(|text| text.strip_suffix('}'))
		.unwrap_or(text)
		.trim()
}

fn collect_scalar_items(node: &ParadoxNode<'_>) -> Vec<String> {
	match node {
		ParadoxNode::Block { items, .. } | ParadoxNode::Array { items, .. } => items
			.iter()
			.filter_map(|item| match item {
				ParadoxNode::Item { value, .. } => scalar_text(value),
				ParadoxNode::Scalar(value) => Some(value.as_text().into_owned()),
				ParadoxNode::Comment { .. } => None,
				_ => None,
			})
			.collect(),
		ParadoxNode::Item { value, .. } => collect_scalar_items(value),
		ParadoxNode::Scalar(value) => vec![value.as_text().into_owned()],
		_ => Vec::new(),
	}
}

fn scalar_text(node: &ParadoxNode<'_>) -> Option<String> {
	match node {
		ParadoxNode::Scalar(value) => Some(value.as_text().into_owned()),
		ParadoxNode::CwtMarker { payload, .. } => Some((*payload).to_string()),
		ParadoxNode::Item { value, .. } => scalar_text(value),
		_ => None,
	}
}

fn scalar_bool(node: &ParadoxNode<'_>) -> Option<bool> {
	match node {
		ParadoxNode::Scalar(ParadoxScalar::Bool(value)) => Some(*value),
		ParadoxNode::Scalar(ParadoxScalar::Identifier(text)) => match *text {
			"yes" | "true" => Some(true),
			"no" | "false" => Some(false),
			_ => None,
		},
		ParadoxNode::Item { value, .. } => scalar_bool(value),
		_ => None,
	}
}

fn insert_enumeration(
	target: &mut HashMap<String, Vec<String>>,
	name: &str,
	value: &ParadoxNode<'_>,
) {
	let values = collect_scalar_items(value);
	if values.is_empty() {
		return;
	}
	target
		.entry(name.to_string())
		.and_modify(|existing| merge_unique(existing, values.clone()))
		.or_insert(values);
}

fn merge_unique(values: &mut Vec<String>, incoming: Vec<String>) {
	for value in incoming {
		if !values.contains(&value) {
			values.push(value);
		}
	}
}

fn normalize_schema_path(path: String) -> String {
	path.trim_start_matches("game/")
		.trim_matches('/')
		.to_ascii_lowercase()
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy()
		.replace('\\', "/")
		.trim_matches('/')
		.to_ascii_lowercase()
}
