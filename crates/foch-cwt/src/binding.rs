use std::path::Path;

use foch_syntax::ParadoxNode;

use crate::schema::{
	AliasCategory, CwtAlias, CwtRuleField, CwtRuleValue, CwtSchemaGraph, CwtSubtype, CwtType,
	CwtTypeDef,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CwtNodeId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaBinding {
	Bound {
		type_id: CwtType,
		node_id: CwtNodeId,
	},
	Dynamic {
		reason: &'static str,
	},
	Unbound {
		reason: String,
	},
}

pub struct BoundNode<'tree> {
	pub syntax: &'tree ParadoxNode<'tree>,
	pub binding: SchemaBinding,
}

#[derive(Clone, Copy, Debug)]
pub enum BindContext<'g> {
	RootType(&'g CwtTypeDef),
	Subtype(&'g CwtTypeDef, &'g CwtSubtype),
	RuleField(&'g CwtRuleField),
	AliasRules(&'g [CwtRuleField]),
}

#[derive(Clone, Copy, Debug)]
pub enum BindFieldMatch<'g> {
	Field(&'g CwtRuleField),
	Alias {
		wildcard: &'g CwtRuleField,
		alias: &'g CwtAlias,
	},
}

impl<'g> BindFieldMatch<'g> {
	pub fn field(&self) -> &'g CwtRuleField {
		match self {
			Self::Field(field)
			| Self::Alias {
				wildcard: field, ..
			} => field,
		}
	}

	pub fn alias(&self) -> Option<&'g CwtAlias> {
		match self {
			Self::Field(_) => None,
			Self::Alias { alias, .. } => Some(alias),
		}
	}
}

#[derive(Clone, Copy)]
enum ResolvedNode<'g> {
	Root(&'g CwtTypeDef),
	Subtype(&'g CwtTypeDef, &'g CwtSubtype),
	Field(&'g CwtRuleField),
	Alias(&'g CwtAlias),
}

#[derive(Clone, Copy)]
enum RuleMatch<'g> {
	Field(&'g CwtRuleField),
	Alias {
		wildcard: &'g CwtRuleField,
		alias: &'g CwtAlias,
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

impl CwtSchemaGraph {
	pub fn bind_root(&self, file_path: &Path) -> Option<&CwtTypeDef> {
		let SchemaBinding::Bound { type_id, .. } = self.root_binding(file_path) else {
			return None;
		};
		self.types.get(&type_id)
	}

	pub fn root_binding(&self, file_path: &Path) -> SchemaBinding {
		let normalized = normalize_path(file_path);
		match self.matching_root_types(file_path).as_slice() {
			[] => SchemaBinding::Unbound {
				reason: format!("no root type matches `{normalized}`"),
			},
			[definition] => bound_node(definition, Vec::new()),
			_ => SchemaBinding::Dynamic {
				reason: "ambiguous-root-type",
			},
		}
	}

	pub fn bind_chain(&self, file_path: &Path, ast_path: &[&str]) -> SchemaBinding {
		if ast_path.is_empty() {
			return self.root_binding(file_path);
		}
		let matches = self.matching_root_types(file_path);
		if matches.is_empty() {
			return SchemaBinding::Unbound {
				reason: format!("no root type matches `{}`", normalize_path(file_path)),
			};
		}
		let attempts = matches
			.into_iter()
			.map(|definition| self.bind_chain_from_root(definition, ast_path))
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

	pub fn bind_fields<'g>(&'g self, parent: BindContext<'g>, key: &str) -> Vec<&'g CwtRuleField> {
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

	pub fn bind_context<'g>(
		&'g self,
		file_path: &Path,
		ast_path: &[&str],
	) -> Option<BindContext<'g>> {
		let target = self.bind_chain(file_path, ast_path);
		if !matches!(&target, SchemaBinding::Bound { .. }) {
			return None;
		}
		for definition in self.matching_root_types(file_path) {
			let Some((context, binding)) = self.bind_context_from_root(definition, ast_path) else {
				continue;
			};
			if binding == target {
				return Some(context);
			}
		}
		None
	}

	pub fn bind_field_match<'g>(
		&'g self,
		parent: BindContext<'g>,
		key: &str,
	) -> Option<BindFieldMatch<'g>> {
		let rule_sets = parent_rule_sets(parent)?;
		for rules in &rule_sets {
			if let Some(field) = rules.iter().find(|field| field.key == key) {
				return Some(BindFieldMatch::Field(field));
			}
		}
		for rules in &rule_sets {
			match self.match_alias_rules(rules, key) {
				Some(RuleMatch::Alias { wildcard, alias }) => {
					return Some(BindFieldMatch::Alias { wildcard, alias });
				}
				Some(RuleMatch::Dynamic { .. }) => return None,
				_ => {}
			}
		}
		for rules in &rule_sets {
			match match_dynamic_key_rules(rules, key) {
				Some(RuleMatch::Field(field)) => return Some(BindFieldMatch::Field(field)),
				Some(RuleMatch::Dynamic { .. }) => return None,
				_ => {}
			}
		}
		None
	}

	pub fn bind_field<'g>(
		&'g self,
		parent: BindContext<'g>,
		key: &str,
	) -> Option<&'g CwtRuleField> {
		self.bind_field_match(parent, key)
			.map(|field_match| field_match.field())
	}

	fn matching_root_types<'g>(&'g self, file_path: &Path) -> Vec<&'g CwtTypeDef> {
		let normalized = normalize_path(file_path);
		let mut matches = self
			.types
			.values()
			.filter_map(|definition| {
				let path = definition.path.as_deref()?;
				let normalized_path = normalize_schema_path(path);
				(normalized == normalized_path
					|| normalized.starts_with(&format!("{normalized_path}/")))
				.then_some((normalized_path.len(), definition))
			})
			.collect::<Vec<_>>();
		matches.sort_by(|lhs, rhs| rhs.0.cmp(&lhs.0).then_with(|| lhs.1.name.cmp(&rhs.1.name)));
		let Some((best_length, _)) = matches.first() else {
			return Vec::new();
		};
		let best_length = *best_length;
		matches
			.into_iter()
			.filter(|(length, _)| *length == best_length)
			.map(|(_, definition)| definition)
			.collect()
	}

	fn bind_chain_from_root<'g>(&'g self, root: &'g CwtTypeDef, ast_path: &[&str]) -> BindAttempt {
		let mut node = ResolvedNode::Root(root);
		let mut node_path = Vec::new();
		let mut consumed = 0;
		let mut root_instance_consumed = false;
		let mut segments = ast_path.iter().copied().peekable();
		while let Some(key) = segments.next() {
			if let ResolvedNode::Root(definition) = node {
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
				if definition.skip_root_key.as_deref() == Some(key) {
					consumed += 1;
					continue;
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
						node_path.push(format!(
							"alias:{}:{}",
							alias_category_name(&alias.category),
							alias.name
						));
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

	fn bind_context_from_root<'g>(
		&'g self,
		root: &'g CwtTypeDef,
		ast_path: &[&str],
	) -> Option<(BindContext<'g>, SchemaBinding)> {
		let mut node = ResolvedNode::Root(root);
		let mut node_path = Vec::new();
		let mut root_instance_consumed = false;
		let mut segments = ast_path.iter().copied().peekable();
		while let Some(key) = segments.next() {
			if let ResolvedNode::Root(definition) = node {
				match self.match_subtype(definition, key) {
					Some(Ok(subtype)) => {
						node = ResolvedNode::Subtype(definition, subtype);
						node_path.push(format!("subtype:{}", subtype_label(subtype)));
						continue;
					}
					Some(Err(_)) => return None,
					None => {}
				}
				if definition.skip_root_key.as_deref() == Some(key) {
					continue;
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
						node_path.push(format!(
							"alias:{}:{}",
							alias_category_name(&alias.category),
							alias.name
						));
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
					return Some((BindContext::RootType(root), bound_node(root, node_path)));
				}
				continue;
			}
			return None;
		}
		Some((context_for_node(node), bound_node(root, node_path)))
	}

	fn match_rules_for_node<'g>(
		&'g self,
		node: ResolvedNode<'g>,
		key: &str,
	) -> Option<RuleMatch<'g>> {
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

	fn match_rule_sets<'g>(
		&'g self,
		rule_sets: &[&'g [CwtRuleField]],
		key: &str,
	) -> Option<RuleMatch<'g>> {
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

	fn match_rule_sets_all<'g>(
		&'g self,
		rule_sets: &[&'g [CwtRuleField]],
		key: &str,
	) -> Vec<&'g CwtRuleField> {
		let mut matches = Vec::new();
		for rules in rule_sets {
			matches.extend(rules.iter().filter(|field| field.key == key));
		}
		matches
	}

	fn match_alias_rules<'g>(
		&'g self,
		rules: &'g [CwtRuleField],
		key: &str,
	) -> Option<RuleMatch<'g>> {
		let mut matches = Vec::new();
		for field in rules {
			let Some((head, payload)) = parse_marker(&field.key) else {
				continue;
			};
			if head != "alias_name" {
				continue;
			}
			let category = AliasCategory::from_name(payload);
			let Some(alias) = self.aliases.get(&(category, key.to_string())) else {
				continue;
			};
			matches.push((field, alias));
		}
		match matches.as_slice() {
			[] => None,
			[(wildcard, alias)] => Some(RuleMatch::Alias { wildcard, alias }),
			_ => Some(RuleMatch::Dynamic {
				reason: "ambiguous-alias-match",
			}),
		}
	}

	fn match_subtype<'g>(
		&'g self,
		definition: &'g CwtTypeDef,
		key: &str,
	) -> Option<Result<&'g CwtSubtype, &'static str>> {
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

fn match_dynamic_key_rules<'g>(rules: &'g [CwtRuleField], key: &str) -> Option<RuleMatch<'g>> {
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

fn parent_rule_sets(parent: BindContext<'_>) -> Option<Vec<&[CwtRuleField]>> {
	match parent {
		BindContext::RootType(root) => Some(vec![root.rules.as_slice()]),
		BindContext::Subtype(root, subtype) => {
			Some(vec![subtype.rules.as_slice(), root.rules.as_slice()])
		}
		BindContext::RuleField(field) => Some(vec![field_rules(field)?]),
		BindContext::AliasRules(rules) => Some(vec![rules]),
	}
}

fn context_for_node(node: ResolvedNode<'_>) -> BindContext<'_> {
	match node {
		ResolvedNode::Root(root) => BindContext::RootType(root),
		ResolvedNode::Subtype(root, subtype) => BindContext::Subtype(root, subtype),
		ResolvedNode::Field(field) => BindContext::RuleField(field),
		ResolvedNode::Alias(alias) => BindContext::AliasRules(alias.rules.as_slice()),
	}
}

fn field_rules(field: &CwtRuleField) -> Option<&[CwtRuleField]> {
	let CwtRuleValue::Block(fields) = &field.value else {
		return None;
	};
	Some(fields.as_slice())
}

fn subtype_matches(subtype: &CwtSubtype, key: &str) -> bool {
	subtype.name == key
		|| subtype
			.type_key_filter
			.as_ref()
			.is_some_and(|filter| filter.matches(key))
}

fn subtype_label(subtype: &CwtSubtype) -> &str {
	subtype
		.type_key_filter
		.as_ref()
		.and_then(|filter| filter.primary_label())
		.unwrap_or(&subtype.name)
}

fn root_type_accepts_instance_key(definition: &CwtTypeDef, key: &str) -> bool {
	definition.type_key_filter.as_ref().map_or_else(
		|| definition.subtypes.is_empty(),
		|filter| filter.matches(key),
	)
}

fn describe_node(node: ResolvedNode<'_>) -> String {
	match node {
		ResolvedNode::Root(definition) => format!("root type `{}`", definition.name.as_str()),
		ResolvedNode::Subtype(definition, subtype) => format!(
			"subtype `{}` of type `{}`",
			subtype_label(subtype),
			definition.name.as_str()
		),
		ResolvedNode::Field(field) => format!("field `{}`", field.key),
		ResolvedNode::Alias(alias) => format!(
			"alias `{}` in category `{}`",
			alias.name,
			alias_category_name(&alias.category)
		),
	}
}

fn bound_node(definition: &CwtTypeDef, path: Vec<String>) -> SchemaBinding {
	let node_id = if path.is_empty() {
		CwtNodeId(format!("type:{}:root", definition.name.as_str()))
	} else {
		CwtNodeId(format!(
			"type:{}:{}",
			definition.name.as_str(),
			path.join("/")
		))
	};
	SchemaBinding::Bound {
		type_id: definition.name.clone(),
		node_id,
	}
}

fn parse_marker(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

fn alias_category_name(category: &AliasCategory) -> &str {
	match category {
		AliasCategory::Trigger => "trigger",
		AliasCategory::Effect => "effect",
		AliasCategory::Modifier => "modifier",
		AliasCategory::Link => "link",
		AliasCategory::Other(name) => name.as_str(),
	}
}

fn normalize_schema_path(path: &str) -> String {
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
