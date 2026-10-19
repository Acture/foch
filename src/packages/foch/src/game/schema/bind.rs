//! Binds script statements to the rules that describe them, one statement at
//! a time from the top of a file down.
//!
//! The walk is game-agnostic. A game supplies its syntax through
//! [`SchemaNode`] and [`SchemaValue`], the conventions the rule files leave to
//! the reader through [`SchemaDialect`], and, when it has them, the names a
//! workspace defines through [`DefinedNames`]. Each consumer drives its own
//! recursion and carries the [`BoundContext`] of a block into its children,
//! so a statement is bound against what its parent bound to, not by
//! re-reading the path from the top of the file.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use super::query::{
	CompiledAlias, CompiledAliasCategory, CompiledBindFieldMatch, CompiledFieldAttributes,
	CompiledLink, CompiledRoot, CompiledRuleCondition, CompiledRuleField, CompiledRuleValue,
	CompiledSubtype, CwtQuery, RuleContext, SchemaScalarType, parse_schema_marker,
};
use crate::model::GamePath;

/// A scalar as the script writes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchemaScalar<'a> {
	Identifier(&'a str),
	String(&'a str),
	Number(&'a str),
	Bool(bool),
}

impl<'a> SchemaScalar<'a> {
	pub(crate) fn text(self) -> Cow<'a, str> {
		match self {
			Self::Identifier(text) | Self::String(text) | Self::Number(text) => Cow::Borrowed(text),
			Self::Bool(true) => Cow::Borrowed("yes"),
			Self::Bool(false) => Cow::Borrowed("no"),
		}
	}
}

/// The value of a statement: a scalar, or a block of statements.
pub(crate) trait SchemaValue {
	type Node: SchemaNode<Value = Self>;

	fn scalar(&self) -> Option<SchemaScalar<'_>>;

	fn block(&self) -> Option<&[Self::Node]>;
}

/// A statement of a script: `key = value`, a value alone, or neither (a
/// comment).
pub(crate) trait SchemaNode: Sized {
	type Value: SchemaValue<Node = Self>;

	fn assignment(&self) -> Option<(&str, &Self::Value)>;

	fn item(&self) -> Option<&Self::Value>;
}

/// The one-to-one scope keywords CWTools reads without an alias: scope
/// switches and logical blocks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeKeyword {
	This,
	Root,
	From,
	Prev,
	PrevPrev,
	And,
	Or,
	Not,
	HiddenEffect,
	HiddenTrigger,
}

impl ScopeKeyword {
	/// Whether the keyword names a scope when used as a value.
	pub(crate) fn names_scope(self) -> bool {
		matches!(
			self,
			Self::This | Self::Root | Self::From | Self::Prev | Self::PrevPrev
		)
	}

	/// Whether the keyword switches to a scope this walk does not track.
	fn switches_untracked_scope(self) -> bool {
		matches!(self, Self::Root | Self::From | Self::Prev | Self::PrevPrev)
	}
}

/// What a game's script means where its rule files leave the reader to know.
pub(crate) trait SchemaDialect {
	fn scope_keyword(&self, key: &str) -> Option<ScopeKeyword>;

	/// A saved scope named in place (EU4 `event_target:name`).
	fn is_event_target(&self, text: &str) -> bool;

	/// A key that is a date, which the rule files write as `date_field`.
	fn is_date_key(&self, key: &str) -> bool;

	/// A key that is a parameter of a scripted definition (`$name$`).
	fn is_parameter_key(&self, key: &str) -> bool;

	/// Text known only once a call substitutes its parameters.
	fn holds_parameter(&self, text: &str) -> bool;

	/// Whether the game's reader takes unquoted `text` as a number.
	fn reads_as_number(&self, text: &str) -> bool;

	/// A value of a complex enum the game assigns at runtime, which no file
	/// defines.
	fn is_runtime_value(&self, complex_enum: &str, text: &str) -> bool;
}

/// The names a workspace defines: values of complex enums, and the names of
/// each type's definitions.
pub(crate) trait DefinedNames {
	fn complex_enum_values(&self, name: &str) -> Option<&[String]>;

	fn defines(&self, type_name: &str, name: &str) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchemaValueShape {
	Scalar,
	Block,
}

impl SchemaValueShape {
	pub(crate) fn of<V: SchemaValue>(value: &V) -> Self {
		if value.block().is_some() {
			Self::Block
		} else {
			Self::Scalar
		}
	}

	pub(crate) fn label(self) -> &'static str {
		match self {
			Self::Scalar => "scalar",
			Self::Block => "block",
		}
	}
}

pub(crate) fn rule_value_shape(value: &CompiledRuleValue) -> SchemaValueShape {
	match value {
		CompiledRuleValue::Scalar(_) | CompiledRuleValue::Marker(_) => SchemaValueShape::Scalar,
		CompiledRuleValue::Block(_) => SchemaValueShape::Block,
	}
}

/// What one statement binds to.
#[derive(Clone, Debug)]
pub(crate) struct StatementBinding<'a> {
	/// Every form of the key the parent's rules allow.
	pub(crate) alternatives: Vec<CompiledBindFieldMatch<'a>>,
	/// The form that fits the statement.
	pub(crate) matched: Option<CompiledBindFieldMatch<'a>>,
	/// How many of the alternatives are blocks.
	pub(crate) block_form_count: usize,
}

/// A block bound to its rules, carried into the block's statements.
pub(crate) struct BoundContext<'a, N> {
	rules: Option<RuleContext<'a>>,
	scopes: Rc<Vec<String>>,
	subtypes: Rc<HashSet<String>>,
	alias: Option<&'a CompiledAlias>,
	replace_scope: Option<&'a BTreeMap<String, String>>,
	path: KeyPath,
	document: &'a [N],
	/// Whether the parent chose this block's form by its contents, which
	/// the path alone cannot.
	chosen: bool,
}

impl<N> Clone for BoundContext<'_, N> {
	fn clone(&self) -> Self {
		Self {
			rules: self.rules,
			scopes: Rc::clone(&self.scopes),
			subtypes: Rc::clone(&self.subtypes),
			alias: self.alias,
			replace_scope: self.replace_scope,
			path: self.path.clone(),
			document: self.document,
			chosen: self.chosen,
		}
	}
}

impl<'a, N> BoundContext<'a, N> {
	/// The rules of the block, or `None` where the schema binds nothing.
	pub(crate) fn rules(&self) -> Option<RuleContext<'a>> {
		self.rules
	}

	/// The scopes the block runs in, by the schema's scope names.
	pub(crate) fn scopes(&self) -> &[String] {
		&self.scopes
	}

	pub(crate) fn subtypes(&self) -> &HashSet<String> {
		&self.subtypes
	}

	/// The alias whose body the block is.
	pub(crate) fn alias(&self) -> Option<&'a CompiledAlias> {
		self.alias
	}

	/// The scopes the block's rule replaces (`root`, `this`, `from`).
	pub(crate) fn replace_scope(&self) -> Option<&'a BTreeMap<String, String>> {
		self.replace_scope
	}

	/// Whether the block is the file itself.
	pub(crate) fn is_file(&self) -> bool {
		self.path.is_empty()
	}

	/// Whether the block's rules accept the keys of an alias category where
	/// they apply (`alias_name[effect]`).
	pub(crate) fn admits(&self, category: &CompiledAliasCategory) -> bool {
		self.rules
			.is_some_and(|rules| rules_admit(rules, &self.subtypes, category))
	}
}

/// Binds the statements of one file.
pub(crate) struct SchemaBinder<'a> {
	pub(crate) engine: &'a CwtQuery,
	pub(crate) dialect: &'a dyn SchemaDialect,
	pub(crate) names: Option<&'a dyn DefinedNames>,
	pub(crate) file_path: &'a GamePath,
	/// How deep the keys that name definitions or their wrappers go.
	top_depth: usize,
	/// The root types whose paths match the file.
	roots: Vec<&'a CompiledRoot>,
	root_ids: Vec<usize>,
	/// The one root type the file belongs to, when only one matches.
	bind_root: Option<&'a CompiledRoot>,
}

impl<'a> SchemaBinder<'a> {
	pub(crate) fn new(
		engine: &'a CwtQuery,
		dialect: &'a dyn SchemaDialect,
		names: Option<&'a dyn DefinedNames>,
		file_path: &'a GamePath,
	) -> Self {
		let root_ids = engine.matching_root_ids(file_path);
		let roots = root_ids
			.iter()
			.map(|index| &engine.pack().roots[*index])
			.collect::<Vec<_>>();
		let top_depth = roots
			.iter()
			.filter(|root| !root.type_per_file)
			.map(|root| root.skip_root_keys.len() + 1)
			.max()
			.unwrap_or(0);
		Self {
			engine,
			dialect,
			names,
			file_path,
			top_depth,
			bind_root: (roots.len() == 1).then(|| roots[0]),
			roots,
			root_ids,
		}
	}

	/// The context of a file's top-level statements.
	pub(crate) fn file<N: SchemaNode>(&self, statements: &'a [N]) -> BoundContext<'a, N> {
		BoundContext {
			rules: self.context_for_path(statements, &[]),
			scopes: Rc::new(self.scopes_for_path(statements, &[])),
			subtypes: Rc::new(self.subtypes_for_path(statements, &[])),
			alias: None,
			replace_scope: None,
			path: KeyPath::default(),
			document: statements,
			chosen: false,
		}
	}

	/// The context of a block of triggers or effects read on its own, with no
	/// file around it: what a scripted effect's body or a host's condition
	/// block holds.
	pub(crate) fn alias_block<N: SchemaNode>(
		&self,
		category: &CompiledAliasCategory,
		statements: &'a [N],
	) -> Option<BoundContext<'a, N>> {
		let alias = self.engine.alias(category.clone(), "scope_field")?;
		Some(BoundContext {
			rules: Some(RuleContext::AliasRules(alias.rules.as_slice())),
			scopes: Rc::new(Vec::new()),
			subtypes: Rc::new(HashSet::new()),
			alias: None,
			replace_scope: None,
			path: KeyPath::default().push(""),
			document: statements,
			chosen: true,
		})
	}

	/// Whether `key`, bound as `binding`, switches scope: a scope keyword, a
	/// saved event target, a link, or an alias that pushes a scope.
	pub(crate) fn switches_scope(&self, key: &str, binding: &StatementBinding<'_>) -> bool {
		self.dialect
			.scope_keyword(key)
			.is_some_and(ScopeKeyword::names_scope)
			|| self.dialect.is_event_target(key)
			|| self.scope_link(key).is_some()
			|| binding
				.matched
				.and_then(|field_match| field_match.alias())
				.is_some_and(|alias| alias.attributes.push_scope.is_some())
	}

	/// Binds `key = value` in `context`.
	pub(crate) fn bind<N: SchemaNode>(
		&self,
		context: &BoundContext<'a, N>,
		key: &str,
		value: &N::Value,
	) -> StatementBinding<'a> {
		let alternatives = context
			.rules
			.map(|rules| self.field_alternatives(rules, key, &context.subtypes))
			.unwrap_or_default();
		// A key holding a `$parameter$` is known only once a call substitutes
		// it; its one checkable form is the scope a parameter opens
		// (`$province$ = { ... }`).
		let matched = self
			.select_alternative(&alternatives, value, &context.subtypes, &context.scopes)
			.filter(|_| !self.dialect.holds_parameter(key) || value.block().is_some());
		let block_form_count = alternatives
			.iter()
			.filter(|field_match| rule_value_shape(field_match.value()) == SchemaValueShape::Block)
			.count();
		StatementBinding {
			alternatives,
			matched,
			block_form_count,
		}
	}

	/// The context of the block `statement` assigns to `key`, bound as
	/// `binding`.
	pub(crate) fn child<N: SchemaNode>(
		&self,
		context: &BoundContext<'a, N>,
		statement: &'a N,
		key: &str,
		binding: &StatementBinding<'a>,
	) -> BoundContext<'a, N> {
		let path = context.path.push(key);
		let document = if context.path.is_empty() {
			self.definition_document(context.document, statement)
		} else {
			context.document
		};
		let chosen = binding
			.matched
			.filter(|_| binding.block_form_count > 1 || context.chosen)
			.filter(|field_match| rule_value_shape(field_match.value()) == SchemaValueShape::Block);
		let stepped = chosen.or_else(|| {
			binding.matched.filter(|_| {
				!context.chosen && binding.block_form_count <= 1 && path.len() > self.top_depth
			})
		});
		if let Some(field_match) = stepped {
			let (rules, scopes) = self.step(field_match, key, &context.scopes);
			return BoundContext {
				rules: Some(rules),
				scopes,
				subtypes: Rc::clone(&context.subtypes),
				alias: field_match.alias(),
				replace_scope: own_replace_scope(rules, field_match.alias()),
				path,
				document,
				chosen: chosen.is_some(),
			};
		}
		if self.roots.is_empty() {
			return BoundContext {
				rules: None,
				scopes: Rc::new(Vec::new()),
				subtypes: Rc::clone(&context.subtypes),
				alias: None,
				replace_scope: None,
				path,
				document,
				chosen: false,
			};
		}
		let keys = path.keys();
		let refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
		let rules = self.context_for_path(document, &refs);
		let subtypes = if refs.len() > self.top_depth {
			Rc::clone(&context.subtypes)
		} else {
			Rc::new(self.subtypes_for_path(document, &refs))
		};
		let alias = binding.matched.and_then(|field_match| field_match.alias());
		BoundContext {
			rules,
			scopes: Rc::new(self.scopes_for_path(document, &refs)),
			subtypes,
			alias,
			replace_scope: rules.and_then(|rules| own_replace_scope(rules, alias)),
			path,
			document,
			chosen: false,
		}
	}

	/// The rules and scopes of a block one step below its parent, by the
	/// form the parent bound its key to.
	fn step(
		&self,
		field_match: CompiledBindFieldMatch<'a>,
		key: &str,
		parent_scopes: &Rc<Vec<String>>,
	) -> (RuleContext<'a>, Rc<Vec<String>>) {
		let context = child_context_for_field_match(field_match);
		if let CompiledBindFieldMatch::Alias { alias, .. } = field_match {
			let pushed = self.alias_pushed_scopes(&alias.attributes);
			if !pushed.is_empty() {
				return (context, Rc::new(pushed));
			}
		}
		let own = self.context_own_scopes(context);
		if !own.is_empty() {
			return (context, Rc::new(own));
		}
		let scopes = self
			.scopes_switched_by_key(key, parent_scopes)
			.map_or_else(|| Rc::clone(parent_scopes), Rc::new);
		(context, scopes)
	}

	/// Definitions that share a key (every `country_event`) are told apart by
	/// position: lookups read the definition holding the statement being
	/// checked rather than the first one with that key. A per-file type reads
	/// the file.
	pub(crate) fn definition_document<N>(&self, statements: &'a [N], top: &'a N) -> &'a [N] {
		if self.bind_root.is_some_and(|root| root.type_per_file) {
			return statements;
		}
		std::slice::from_ref(top)
	}

	/// What the schema's own binder answers for `path` in this file.
	pub(crate) fn bind_context(&self, path: &[&str]) -> Option<RuleContext<'a>> {
		self.engine
			.bind_context_among(self.file_path, &self.root_ids, path)
	}

	/// The rules of the block `path` names, read from the top of the file.
	pub(crate) fn context_for_path<N: SchemaNode>(
		&self,
		document: &[N],
		path: &[&str],
	) -> Option<RuleContext<'a>> {
		#[cfg(test)]
		PATH_BINDINGS.with(|count| count.set(count.get() + 1));
		let root = self.root_owning_path(path);
		if path_is_above_definitions(root, path) {
			return None;
		}
		// Anchor at the shallowest level the binder resolves (the definition),
		// then walk the rest here so each nested key honours subtype
		// conditions, block forms and date keys, which the binder does not.
		for prefix_len in 0..=path.len() {
			if path_is_above_definitions(root, &path[..prefix_len]) {
				continue;
			}
			let Some(mut context) = self.engine_context(document, &path[..prefix_len]) else {
				continue;
			};
			let mut resolved_all = true;
			for (index, segment) in path.iter().enumerate().skip(prefix_len) {
				let subtypes = self.subtypes_for_path(document, &path[..index]);
				let Some(field_match) = self.block_field_match(context, segment, &subtypes) else {
					resolved_all = false;
					break;
				};
				context = child_context_for_field_match(field_match);
			}
			if resolved_all {
				return Some(context);
			}
		}
		None
	}

	/// The type the definition `path` runs through. A file several types
	/// share (EU4 `dlc_metadata/dlc_info`: `categories` and `dlcs`) belongs,
	/// below its top, to the one type whose `skip_root_key` wrappers the path
	/// follows.
	fn root_owning_path(&self, path: &[&str]) -> Option<&'a CompiledRoot> {
		if let Some(root) = self.bind_root {
			return Some(root);
		}
		if path.is_empty() {
			return None;
		}
		let roots = self
			.roots
			.iter()
			.copied()
			.filter(|root| {
				!root.type_per_file
					&& root
						.skip_root_keys
						.iter()
						.zip(path)
						.all(|(skip_key, key)| skip_key == "any" || skip_key == key)
			})
			.collect::<Vec<_>>();
		if let [root] = roots.as_slice() {
			return Some(root);
		}
		// A key that names one type's definition may also be a wrapper
		// another type skips (EU4 `government_reform` beside
		// `states_general_faction`); the type whose definitions the path
		// reaches owns it.
		let mut reaching = roots
			.into_iter()
			.filter(|root| path.len() > root.skip_root_keys.len());
		let root = reaching.next()?;
		reaching.next().is_none().then_some(root)
	}

	/// A top-level key names a definition even when a body rule shares the
	/// name: the binder reads `simple_dynamic_effect = { ... }` in a scripted
	/// effects file as a call of that effect, which the schema lists, not as
	/// its definition.
	fn definition_named_like_body_field(
		&self,
		path: &[&str],
		context: RuleContext<'_>,
	) -> Option<&'a CompiledRoot> {
		if matches!(context, RuleContext::RootType(_) | RuleContext::Subtype(..)) {
			return None;
		}
		let root = self.bind_root?;
		let (key, wrappers) = path.split_last()?;
		(!root.type_per_file
			&& wrappers.len() == root.skip_root_keys.len()
			&& root
				.type_key_filter
				.as_ref()
				.is_none_or(|filter| filter.matches(key)))
		.then_some(root)
	}

	fn engine_context<N: SchemaNode>(
		&self,
		document: &[N],
		path: &[&str],
	) -> Option<RuleContext<'a>> {
		// The binder reads a date key as an ordinary key; this walk resolves
		// dates to `date_field` segment by segment.
		if path.iter().any(|segment| self.dialect.is_date_key(segment)) {
			return None;
		}
		let Some(context) = self.bind_context(path) else {
			return self.shared_folder_definition_context(path);
		};
		// The engine may name a type for which the path is only a wrapper,
		// where another type's definition is what the path names.
		let context = match (context, self.root_owning_path(path)) {
			(RuleContext::RootType(found) | RuleContext::Subtype(found, _), Some(owner))
				if !std::ptr::eq(found, owner) =>
			{
				RuleContext::RootType(owner)
			}
			(context, _) => context,
		};
		if let Some(root) = self.definition_named_like_body_field(path, context) {
			return Some(RuleContext::RootType(root));
		}
		if context_describes_body(context) {
			return Some(context);
		}
		let (RuleContext::RootType(root) | RuleContext::Subtype(root, _)) = context else {
			return None;
		};
		let (_, parent_path) = path.split_last()?;
		let parent = self.context_for_path(document, parent_path)?;
		let subtypes = self.subtypes_for_path(document, parent_path);
		let type_key = format!("<{}>", root.name);
		rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, &subtypes))
			.find(|field| field.key == type_key)
			.map(RuleContext::RuleField)
	}

	/// When several types share a folder (EU4 `missions`), the binder cannot
	/// pick one for a top-level key. Only one of them declares definitions
	/// with a body at the top of the file, so that key names one of its
	/// definitions.
	fn shared_folder_definition_context(&self, path: &[&str]) -> Option<RuleContext<'a>> {
		if path.len() != 1 {
			return None;
		}
		let key = path[0];
		let mut roots = self.roots.iter().copied().filter(|root| {
			!root.type_per_file
				&& root.skip_root_keys.is_empty()
				&& root
					.type_key_filter
					.as_ref()
					.is_none_or(|filter| filter.matches(key))
				&& context_describes_body(RuleContext::RootType(root))
		});
		let root = roots.next()?;
		roots
			.next()
			.is_none()
			.then_some(RuleContext::RootType(root))
	}

	/// The scopes the block `path` names runs in, read from the top of the
	/// file.
	pub(crate) fn scopes_for_path<N: SchemaNode>(
		&self,
		document: &[N],
		path: &[&str],
	) -> Vec<String> {
		let mut active_scopes = Vec::new();
		let mut parent_context = None;
		for index in 0..=path.len() {
			let context = self.context_for_path(document, &path[..index]);
			if let Some(context) = context {
				let scopes = self.context_own_scopes(context);
				if !scopes.is_empty() {
					active_scopes = scopes;
				}
			}
			if let (Some(RuleContext::AliasRules(_)), Some(parent), Some(key)) = (
				context,
				parent_context,
				index
					.checked_sub(1)
					.and_then(|parent_len| path.get(parent_len)),
			) {
				let subtypes = self.subtypes_for_path(document, &path[..index - 1]);
				if let Some(CompiledBindFieldMatch::Alias { alias, .. }) =
					self.block_field_match(parent, key, &subtypes)
				{
					let scopes = self.alias_pushed_scopes(&alias.attributes);
					if !scopes.is_empty() {
						active_scopes = scopes;
					}
				}
			}
			if let Some(component) = path.get(index) {
				active_scopes = self.scopes_after_key(component, active_scopes);
			}
			parent_context = context;
		}
		active_scopes
	}

	/// An alias's own `scope` names where it may be used; only `push_scope`
	/// and `replace_scope` describe the scope its body runs in.
	fn alias_pushed_scopes(&self, attributes: &CompiledFieldAttributes) -> Vec<String> {
		attributes
			.push_scope
			.as_ref()
			.or_else(|| attributes.replace_scope.get("this"))
			.map(|scope| self.listed_scope(scope))
			.into_iter()
			.collect()
	}

	/// CWTools reads a scope name its scope list lacks as any scope, and the
	/// rule files write a few (`push_scope = { root = province this =
	/// province }`, `scope = { trade_node capital_scope }`).
	fn listed_scope(&self, scope: &str) -> String {
		if self.engine.is_unlisted_scope(scope) {
			"any".to_string()
		} else {
			scope.to_string()
		}
	}

	fn scopes_after_key(&self, key: &str, active_scopes: Vec<String>) -> Vec<String> {
		self.scopes_switched_by_key(key, &active_scopes)
			.unwrap_or(active_scopes)
	}

	/// The scopes a key switches to from `active_scopes`, when it switches.
	fn scopes_switched_by_key(&self, key: &str, active_scopes: &[String]) -> Option<Vec<String>> {
		if self
			.dialect
			.scope_keyword(key)
			.is_some_and(ScopeKeyword::switches_untracked_scope)
			|| self.dialect.is_event_target(key)
		{
			return Some(Vec::new());
		}
		self.link_output_scope(key, active_scopes)
			.map(|output_scope| vec![output_scope.to_string()])
	}

	fn context_own_scopes(&self, context: RuleContext<'_>) -> Vec<String> {
		let root_scopes = |root: &CompiledRoot| {
			root.push_scope
				.iter()
				.map(|scope| self.listed_scope(scope))
				.collect::<Vec<_>>()
		};
		match context {
			RuleContext::RootType(root) => root_scopes(root),
			RuleContext::Subtype(root, subtype) => {
				let scopes = self.scopes_from_attributes(&subtype.attributes);
				if scopes.is_empty() {
					root_scopes(root)
				} else {
					scopes
				}
			}
			RuleContext::RuleField(field) => self.scopes_from_attributes(&field.attributes),
			RuleContext::AliasRules(_) => Vec::new(),
		}
	}

	fn scopes_from_attributes(&self, attributes: &CompiledFieldAttributes) -> Vec<String> {
		if let Some(push_scope) = attributes.push_scope.as_ref() {
			return vec![self.listed_scope(push_scope)];
		}
		if let Some(this_scope) = attributes.replace_scope.get("this") {
			return vec![self.listed_scope(this_scope)];
		}
		attributes
			.scope
			.iter()
			.map(|scope| self.listed_scope(scope))
			.collect()
	}

	fn link_output_scope(&self, key: &str, active_scopes: &[String]) -> Option<&'a str> {
		let link = self.scope_link(key)?;
		if !self.link_accepts_active_scope(link, active_scopes) {
			return None;
		}
		link.output_scope.as_deref()
	}

	fn link_accepts_active_scope(&self, link: &CompiledLink, active_scopes: &[String]) -> bool {
		if active_scopes.is_empty() || link.input_scopes.is_empty() {
			return true;
		}
		link.input_scopes.iter().any(|input_scope| {
			active_scopes
				.iter()
				.any(|active_scope| self.engine.scope_matches(input_scope, active_scope))
		})
	}

	/// Whether an alias may be used in `active_scopes`.
	pub(crate) fn alias_scope_matches(
		&self,
		alias: &CompiledAlias,
		active_scopes: &[String],
	) -> bool {
		if active_scopes.is_empty()
			|| alias.attributes.scope.is_empty()
			|| alias
				.attributes
				.scope
				.iter()
				.any(|scope| self.engine.is_unlisted_scope(scope))
			|| self.is_declared_modifier(alias)
		{
			return true;
		}
		alias.attributes.scope.iter().any(|scope| {
			active_scopes
				.iter()
				.any(|active| self.engine.scope_matches(scope, active))
		})
	}

	/// The scope the schema's `modifiers` list gives a modifier is its
	/// category, not where it may be written: CWTools checks these modifiers
	/// in any scope, and vanilla writes country modifiers in province blocks
	/// (a trade good's `province = { land_forcelimit = 0.5 }`).
	fn is_declared_modifier(&self, alias: &CompiledAlias) -> bool {
		alias.category == CompiledAliasCategory::Modifier
			&& self
				.engine
				.declared_modifier(&alias.name)
				.is_some_and(|declared| std::ptr::eq(declared, alias))
	}

	/// The scope link a key names, ignoring case.
	pub(crate) fn scope_link(&self, key: &str) -> Option<&'a CompiledLink> {
		self.engine
			.link(key)
			.or_else(|| self.engine.link(&key.to_ascii_lowercase()))
	}

	/// The subtypes active in the definition `path` runs through.
	pub(crate) fn subtypes_for_path<N: SchemaNode>(
		&self,
		statements: &[N],
		path: &[&str],
	) -> HashSet<String> {
		let mut active = HashSet::new();
		if let Some(root_key) = path.first()
			&& let Some(RuleContext::Subtype(_, subtype)) = self.bind_context(&[*root_key])
		{
			active.insert(subtype.name.clone());
		}
		let Some(root) = self.definition_root(path) else {
			return active;
		};
		let root_statements = if root.type_per_file {
			Some(statements)
		} else {
			root_block_statements(root, statements, path)
		};
		let Some(root_statements) = root_statements else {
			return active;
		};
		let definition_key = definition_key(root, self.file_path, path);
		for subtype in &root.subtypes {
			if self.subtype_matches_body(subtype, definition_key, root_statements) {
				active.insert(subtype.name.clone());
			}
		}
		active
	}

	/// The type of the definition `path` runs through: the file's type, the
	/// one whose wrappers the path follows, or, in a folder several types
	/// share, the one its top-level key names.
	fn definition_root(&self, path: &[&str]) -> Option<&'a CompiledRoot> {
		if let Some(root) = self.root_owning_path(path) {
			return Some(root);
		}
		let root_key = path.first()?;
		match self.shared_folder_definition_context(&[*root_key])? {
			RuleContext::RootType(root) => Some(root),
			_ => None,
		}
	}

	/// Whether a subtype selects a definition body. A subtype's
	/// `type_key_filter` reads the definition's name (`defaults_reform`; a
	/// per-file type's file name, EU4 `history/diplomacy/hre.txt`); every
	/// subtype rule must hold for the body.
	pub(crate) fn subtype_matches_body<N: SchemaNode>(
		&self,
		subtype: &CompiledSubtype,
		definition_key: Option<&str>,
		statements: &[N],
	) -> bool {
		if let (Some(filter), Some(definition_key)) = (&subtype.type_key_filter, definition_key) {
			return filter.matches(definition_key)
				&& (subtype.rules.is_empty()
					|| self.rule_fields_match(&subtype.rules, statements));
		}
		!subtype.rules.is_empty() && self.rule_fields_match(&subtype.rules, statements)
	}

	/// Each rule holds when some statement matches it. A rule capped at
	/// `0..0` holds when none does; an optional rule (`0..1`) also holds when
	/// no statement has its key, as CWTools validates a subtype's fields.
	fn rule_fields_match<N: SchemaNode>(
		&self,
		rules: &[CompiledRuleField],
		statements: &[N],
	) -> bool {
		rules.iter().all(|rule| {
			let present = statements
				.iter()
				.any(|statement| self.rule_field_matches(rule, statement));
			match rule.attributes.cardinality {
				Some((_, Some(0))) => !present,
				Some((0, _)) => {
					present
						|| !statements
							.iter()
							.any(|statement| match statement.assignment() {
								Some((key, _)) => self.rule_key_matches(&rule.key, key),
								None => rule.key.is_empty() && statement.item().is_some(),
							})
				}
				_ => present,
			}
		})
	}

	fn rule_field_matches<N: SchemaNode>(&self, rule: &CompiledRuleField, statement: &N) -> bool {
		if rule.key.is_empty() {
			return statement
				.item()
				.is_some_and(|value| self.rule_value_matches(&rule.value, value));
		}
		let Some((key, value)) = statement.assignment() else {
			return false;
		};
		self.rule_key_matches(&rule.key, key) && self.rule_value_matches(&rule.value, value)
	}

	/// The rule for a value written alone in a block bound to `context`, the
	/// first whose value fits it.
	pub(crate) fn bind_item<N: SchemaNode>(
		&self,
		context: &BoundContext<'a, N>,
		value: &N::Value,
	) -> Option<&'a CompiledRuleField> {
		let shape = SchemaValueShape::of(value);
		let mut candidates = rule_fields(context.rules?).filter(|field| {
			field.key.is_empty()
				&& conditions_match(&field.conditions, &context.subtypes)
				&& rule_value_shape(&field.value) == shape
		});
		match value.scalar() {
			Some(scalar) => candidates.find(|field| {
				self.field_match_accepts_scalar(&CompiledBindFieldMatch::Field(field), scalar)
			}),
			None => candidates.next(),
		}
	}

	/// A rule key names a key literally or by its type (`date_field`,
	/// `scalar`).
	fn rule_key_matches(&self, rule_key: &str, key: &str) -> bool {
		if rule_key == key {
			return true;
		}
		if rule_key == "date_field" {
			return self.dialect.is_date_key(key);
		}
		self.primitive_key_matches(rule_key, key_scalar(key))
	}

	fn rule_value_matches<V: SchemaValue>(
		&self,
		rule_value: &CompiledRuleValue,
		value: &V,
	) -> bool {
		match (rule_value, value.scalar(), value.block()) {
			(
				CompiledRuleValue::Scalar(expected) | CompiledRuleValue::Marker(expected),
				Some(scalar),
				_,
			) => self.rule_value_accepts_scalar(rule_value, expected, scalar),
			(CompiledRuleValue::Block(rules), _, Some(items)) => {
				rules.is_empty() || self.rule_fields_match(rules, items)
			}
			_ => false,
		}
	}

	/// Whether a scalar fits a rule value written as a type (`scalar`, `int`,
	/// `enum[...]`) or, when the rule names no type, equals it literally.
	/// Kinds this check cannot resolve (definition references, scopes) are
	/// accepted.
	fn rule_value_accepts_scalar(
		&self,
		rule_value: &CompiledRuleValue,
		expected: &str,
		scalar: SchemaScalar<'_>,
	) -> bool {
		if let Some(scalar_type) = SchemaScalarType::from_rule_value(rule_value) {
			return self.scalar_type_matches(scalar_type, scalar);
		}
		if ANY_SCALAR_TYPES.contains(&expected) || is_angle_dynamic_key_marker(expected) {
			return true;
		}
		let text = scalar.text();
		match parse_schema_marker(expected) {
			Some(("enum", name)) => self
				.engine
				.enum_values(name)
				.is_none_or(|values| values.iter().any(|value| value == text.as_ref())),
			Some(_) => true,
			None => text == expected,
		}
	}

	/// Every form of `key` that `parent`'s rules allow, the one binding
	/// resolves to first.
	///
	/// The schema can list one key several times with different values (EU4
	/// event `desc` is a localisation key or a conditional block). Every form
	/// whose subtype conditions hold is an alternative for that key.
	pub(crate) fn field_alternatives(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Vec<CompiledBindFieldMatch<'a>> {
		if self.dialect.is_date_key(key) {
			let date_matches = self.conditioned_field_matches(parent, "date_field", subtypes);
			if !date_matches.is_empty() {
				return date_matches;
			}
		}
		// The game reads script keys without regard to case, and the rule
		// files spell some keys as the game files do not (`Regions` for
		// `regions`). A key the block names in another case is that key,
		// before any form a key of any name could take.
		if !rule_fields(parent).any(|field| field.key == key) {
			let mut folded_rule_keys = rule_fields(parent)
				.map(|field| field.key.as_str())
				.filter(|rule_key| rule_key.eq_ignore_ascii_case(key))
				.collect::<Vec<_>>();
			folded_rule_keys.sort_unstable();
			folded_rule_keys.dedup();
			let folded_matches = folded_rule_keys
				.into_iter()
				.flat_map(|rule_key| self.conditioned_field_matches(parent, rule_key, subtypes))
				.collect::<Vec<_>>();
			if !folded_matches.is_empty() {
				return folded_matches;
			}
		}
		let static_matches = self.conditioned_field_matches(parent, key, subtypes);
		if !static_matches.is_empty() {
			// A named key also fits a rule written as its type
			// (`trigger_switch` lists `advisor_events_adm = { ... }` beside
			// `scalar = { ... }`); the statement's contents choose, the named
			// form on a tie.
			let mut matches = static_matches;
			matches.extend(self.primitive_key_field_matches(parent, key, subtypes));
			return matches;
		}
		// The game reads script keys without regard to case (`ADM` for `adm`).
		let lower_key = key.to_ascii_lowercase();
		if lower_key != key {
			let folded_matches = self.conditioned_field_matches(parent, &lower_key, subtypes);
			if !folded_matches.is_empty() {
				return folded_matches;
			}
		}
		let dynamic_alias_matches = self.dynamic_alias_field_matches(parent, key, subtypes);
		if !dynamic_alias_matches.is_empty() {
			return dynamic_alias_matches;
		}
		self.dynamic_key_field_matches(parent, key, subtypes)
			.or_else(|| {
				self.scope_field_match(parent, key, subtypes)
					.map(|field_match| vec![field_match])
			})
			.unwrap_or_else(|| {
				let matches = self.primitive_key_field_matches(parent, key, subtypes);
				if matches.is_empty() {
					self.numeric_key_field_matches(parent, key, subtypes)
				} else {
					matches
				}
			})
	}

	/// A number where the rules take numbers of another kind or range as
	/// keys (`random_list` weights are `int`; mods write `0.95`) is that key,
	/// of the wrong type.
	fn numeric_key_field_matches(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Vec<CompiledBindFieldMatch<'a>> {
		if key.parse::<f64>().is_err() {
			return Vec::new();
		}
		rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, subtypes))
			.filter(|field| numeric_key_type(&field.key).is_some())
			.map(CompiledBindFieldMatch::Field)
			.collect()
	}

	/// The number type a rule key written as one (`int`, `float[0..1]`)
	/// requires, when `key` does not satisfy it.
	pub(crate) fn mistyped_key(&self, rule_key: &str, key: &str) -> Option<SchemaScalarType> {
		let expected = numeric_key_type(rule_key)?;
		(!self.scalar_type_matches(expected, key_scalar(key))).then_some(expected)
	}

	/// Rule keys written as a type (`scalar`, `int`, `int[1..10]`) accept any
	/// key of that type, such as the numbered entries of `random_events`.
	fn primitive_key_field_matches(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Vec<CompiledBindFieldMatch<'a>> {
		let key_scalar = key_scalar(key);
		rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, subtypes))
			.filter(|field| self.primitive_key_matches(&field.key, key_scalar))
			.map(CompiledBindFieldMatch::Field)
			.collect()
	}

	fn primitive_key_matches(&self, rule_key: &str, key: SchemaScalar<'_>) -> bool {
		let rule_value = CompiledRuleValue::Scalar(rule_key.to_string());
		if let Some(scalar_type) = SchemaScalarType::from_rule_value(&rule_value) {
			return self.scalar_type_matches(scalar_type, key);
		}
		rule_key != "date_field" && ANY_SCALAR_TYPES.contains(&rule_key)
	}

	fn conditioned_field_matches(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Vec<CompiledBindFieldMatch<'a>> {
		self.engine.bind_field_overloads(parent, key, |field| {
			conditions_match(&field.conditions, subtypes)
		})
	}

	/// A key on a path to a block names the form of that key that is a block.
	pub(crate) fn block_field_match(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Option<CompiledBindFieldMatch<'a>> {
		let alternatives = self.field_alternatives(parent, key, subtypes);
		alternatives
			.iter()
			.copied()
			.find(|field_match| rule_value_shape(field_match.value()) == SchemaValueShape::Block)
			.or_else(|| alternatives.first().copied())
	}

	fn scope_field_match(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Option<CompiledBindFieldMatch<'a>> {
		if !self.is_scope_field_key(key) {
			return None;
		}
		rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, subtypes))
			.find_map(|field| {
				let (head, category) = parse_schema_marker(&field.key)?;
				if head != "alias_name" {
					return None;
				}
				let alias = self
					.engine
					.alias(CompiledAliasCategory::from_name(category), "scope_field")?;
				Some(CompiledBindFieldMatch::Alias {
					wildcard: field,
					alias,
				})
			})
	}

	/// Whether a key switches scope through a keyword, a saved event target
	/// or a scope link, which the rule files accept through their
	/// `scope_field` alias.
	pub(crate) fn is_scope_field_key(&self, key: &str) -> bool {
		self.dialect.scope_keyword(key).is_some()
			|| self.dialect.is_event_target(key)
			|| self.scope_link(key).is_some()
	}

	/// A value naming a scope through a keyword, a saved event target or a
	/// scope link. Keywords and event targets have no tracked scope; a link
	/// must lead to the required one.
	fn scope_reference_matches(&self, required_scope: &str, text: &str) -> bool {
		if self
			.dialect
			.scope_keyword(text)
			.is_some_and(ScopeKeyword::names_scope)
			|| self.dialect.is_event_target(text)
		{
			return true;
		}
		self.scope_link(text).is_some_and(|link| {
			link.output_scope
				.as_deref()
				.is_none_or(|output_scope| self.engine.scope_matches(required_scope, output_scope))
		})
	}

	/// The dynamically named aliases (`<scripted_effect>`) that `key` names,
	/// the one binding resolves to first. A key can name several: a trade
	/// node is named by its location, so `45 = { ... }` is a province or a
	/// trade node, and the statement's contents choose between them.
	fn dynamic_alias_field_matches(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Vec<CompiledBindFieldMatch<'a>> {
		if is_dynamic_key_marker(key) {
			return Vec::new();
		}
		let mut matches = rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, subtypes))
			.filter_map(|field| {
				let (head, payload) = parse_schema_marker(&field.key)?;
				(head == "alias_name").then_some((field, CompiledAliasCategory::from_name(payload)))
			})
			.flat_map(|(field, category)| {
				self.engine
					.dynamic_aliases(&category)
					.filter(move |alias| self.dynamic_alias_matches(alias, key))
					.map(move |alias| CompiledBindFieldMatch::Alias {
						wildcard: field,
						alias,
					})
			})
			.collect::<Vec<_>>();
		matches.reverse();
		matches
	}

	/// The rule fields whose key, written as a marker, accepts `key`. Several
	/// forms of one marker key (`<province_id>` as a name or a block of names)
	/// are alternatives; fields of different markers leave the key ambiguous.
	fn dynamic_key_field_matches(
		&self,
		parent: RuleContext<'a>,
		key: &str,
		subtypes: &HashSet<String>,
	) -> Option<Vec<CompiledBindFieldMatch<'a>>> {
		if is_dynamic_key_marker(key) {
			return None;
		}
		let matches = rule_fields(parent)
			.filter(|field| conditions_match(&field.conditions, subtypes))
			.filter(|field| self.dynamic_key_field_accepts(field, key))
			.collect::<Vec<_>>();
		let first = matches.first()?;
		matches.iter().all(|field| field.key == first.key).then(|| {
			matches
				.into_iter()
				.map(CompiledBindFieldMatch::Field)
				.collect()
		})
	}

	fn dynamic_key_field_accepts(&self, field: &CompiledRuleField, key: &str) -> bool {
		let Some((head, name)) = parse_schema_marker(&field.key) else {
			return DefinitionKeyTemplate::parse(&field.key).is_some_and(|template| {
				self.names.is_some_and(|names| template.accepts(names, key))
			});
		};
		if head == "alias_name" {
			return false;
		}
		if field.key == SCRIPT_PARAMETER_ENUM {
			return !self.dialect.holds_parameter(key);
		}
		self.allowed_values_for_marker(head, name)
			.is_some_and(|values| values.contains(key))
	}

	fn dynamic_alias_matches(&self, alias: &CompiledAlias, key: &str) -> bool {
		if alias.name == SCRIPT_PARAMETER_KEY_ENUM {
			return self.dialect.is_parameter_key(key);
		}
		if let Some(template) = DefinitionKeyTemplate::parse(&alias.name) {
			return self.names.is_some_and(|names| template.accepts(names, key));
		}
		self.dynamic_alias_values(alias)
			.is_some_and(|values| values.contains(key))
	}

	/// Chooses the form of a key that fits a statement: for a block the
	/// block form that rejects the fewest of its statements, and for a scalar
	/// the first form that accepts the value. Among forms that fit equally,
	/// one allowed in the current scope wins (an alias can be defined once
	/// per scope). When none fits, the first form of the right shape (or the
	/// first form) stands.
	pub(crate) fn select_alternative<V: SchemaValue>(
		&self,
		alternatives: &[CompiledBindFieldMatch<'a>],
		value: &V,
		subtypes: &HashSet<String>,
		active_scopes: &[String],
	) -> Option<CompiledBindFieldMatch<'a>> {
		let actual = SchemaValueShape::of(value);
		let out_of_scope = |field_match: &CompiledBindFieldMatch<'a>| {
			field_match
				.alias()
				.is_some_and(|alias| !self.alias_scope_matches(alias, active_scopes))
		};
		let mut same_shape = alternatives
			.iter()
			.copied()
			.filter(|field_match| rule_value_shape(field_match.value()) == actual)
			.collect::<Vec<_>>();
		same_shape.sort_by_key(out_of_scope);
		let first_same_shape = same_shape.first().copied();
		let accepted = match (value.scalar(), value.block()) {
			(Some(scalar), _) => same_shape
				.into_iter()
				.find(|field_match| self.field_match_accepts_scalar(field_match, scalar)),
			(None, Some(items)) => same_shape.into_iter().min_by_key(|field_match| {
				self.block_mismatch_count(
					child_context_for_field_match(*field_match),
					items,
					subtypes,
				)
			}),
			(None, None) => None,
		};
		accepted
			.or(first_same_shape)
			.or_else(|| alternatives.first().copied())
	}

	/// How many of a block's statements a candidate form rejects: keys it
	/// lacks, and values no form of their key accepts.
	fn block_mismatch_count<N: SchemaNode>(
		&self,
		rule_context: RuleContext<'a>,
		statements: &[N],
		subtypes: &HashSet<String>,
	) -> usize {
		statements
			.iter()
			.filter(|statement| {
				let Some((key, value)) = statement.assignment() else {
					return false;
				};
				let alternatives = self.field_alternatives(rule_context, key, subtypes);
				let actual = SchemaValueShape::of(value);
				!alternatives.iter().any(|field_match| {
					rule_value_shape(field_match.value()) == actual
						&& value.scalar().is_none_or(|scalar| {
							self.field_match_accepts_scalar(field_match, scalar)
						})
				})
			})
			.count()
	}

	/// Whether a rule form accepts a scalar: the type it names, and the
	/// values it lists when it lists them.
	pub(crate) fn field_match_accepts_scalar(
		&self,
		field_match: &CompiledBindFieldMatch<'_>,
		scalar: SchemaScalar<'_>,
	) -> bool {
		if self.dialect.holds_parameter(&scalar.text()) {
			return true;
		}
		// A value written literally is that value, before any form a value of
		// a type could take: `random_events` lists `int = 0` beside
		// `int = <event.country>`.
		if let Some(literal) = literal_rule_value(field_match.value()) {
			return scalar.text().eq_ignore_ascii_case(literal);
		}
		if let Some(expected) = SchemaScalarType::from_rule_value(field_match.value())
			&& !self.scalar_type_matches(expected, scalar)
		{
			return false;
		}
		self.allowed_values(field_match.value())
			.is_none_or(|values| self.allowed_values_accept(&values, &scalar.text()))
	}

	/// Whether `scalar` satisfies the schema's declared primitive type.
	///
	/// This is schema conformance, deliberately strict: it asks whether the
	/// file says what the schema declares, not what the game would read from
	/// it.
	pub(crate) fn scalar_type_matches(
		&self,
		scalar_type: SchemaScalarType,
		scalar: SchemaScalar<'_>,
	) -> bool {
		match scalar_type {
			SchemaScalarType::Int { range } => self.number_text(scalar).is_some_and(|value| {
				value
					.parse::<i64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number)))
			}),
			SchemaScalarType::Float { range } => self.number_text(scalar).is_some_and(|value| {
				value
					.parse::<f64>()
					.is_ok_and(|number| range.is_none_or(|range| range.contains(number)))
			}),
			SchemaScalarType::Bool => matches!(scalar, SchemaScalar::Bool(_)),
		}
	}

	/// The numeral a scalar spells. Readers of numbers take a leading `+` as
	/// a sign, and read a quoted numeral as the number it spells.
	pub(crate) fn number_text<'s>(&self, scalar: SchemaScalar<'s>) -> Option<&'s str> {
		match scalar {
			SchemaScalar::Number(value) => Some(value),
			SchemaScalar::String(value) if self.dialect.reads_as_number(value) => Some(value),
			SchemaScalar::Identifier(value) | SchemaScalar::String(value) => {
				value.strip_prefix('+').filter(|unsigned| {
					!unsigned.starts_with('-') && self.dialect.reads_as_number(unsigned)
				})
			}
			SchemaScalar::Bool(_) => None,
		}
	}

	/// The values a rule value names (`enum[...]`, `value[...]`,
	/// `scope[...]`).
	pub(crate) fn allowed_values(
		&self,
		value: &'a CompiledRuleValue,
	) -> Option<SchemaAllowedValues<'a>> {
		let value = match value {
			CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => value,
			CompiledRuleValue::Block(_) => return None,
		};
		let (head, name) = allowed_value_marker(value)?;
		self.allowed_values_for_marker(head, name)
	}

	/// The values a dynamically named alias stands for.
	pub(crate) fn dynamic_alias_values(
		&self,
		alias: &'a CompiledAlias,
	) -> Option<SchemaAllowedValues<'a>> {
		let (head, name) = allowed_value_marker(&alias.name)?;
		self.allowed_values_for_marker(head, name)
	}

	pub(crate) fn allowed_values_for_marker<'m>(
		&self,
		head: &'m str,
		name: &'m str,
	) -> Option<SchemaAllowedValues<'m>>
	where
		'a: 'm,
	{
		let (kind, values) = match head {
			"enum" => {
				if let Some(values) = self.engine.enum_values(name) {
					(SchemaAllowedValueKind::Enum, Cow::Borrowed(values))
				} else if let Some(complex_enum) = self.engine.complex_enum(name) {
					(
						SchemaAllowedValueKind::ComplexEnum,
						Cow::Borrowed(self.names?.complex_enum_values(&complex_enum.name)?),
					)
				} else {
					return None;
				}
			}
			"value" => (
				SchemaAllowedValueKind::Value,
				Cow::Borrowed(self.engine.value_set_values(name)?),
			),
			"value_set" => (
				SchemaAllowedValueKind::ValueSet,
				Cow::Borrowed(self.engine.value_set_values(name)?),
			),
			"scope" => (
				SchemaAllowedValueKind::Scope,
				Cow::Owned(self.engine.scope_values(name)),
			),
			_ => return None,
		};
		(!values.is_empty()).then_some(SchemaAllowedValues {
			kind,
			name,
			values,
			runtime: (kind == SchemaAllowedValueKind::ComplexEnum)
				.then_some(self.dialect)
				.map(|dialect| RuntimeValues { dialect, name }),
		})
	}

	/// Whether `text` is one of `values`, or names a scope `values` requires.
	pub(crate) fn allowed_values_accept(
		&self,
		values: &SchemaAllowedValues<'_>,
		text: &str,
	) -> bool {
		values.contains(text)
			|| (values.kind == SchemaAllowedValueKind::Scope
				&& self.scope_reference_matches(values.name, text))
	}
}

#[cfg(test)]
std::thread_local! {
	static PATH_BINDINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How often this thread has bound a block by its path from the top of a
/// file since the last call, the walk's one step that costs more than a
/// statement's own binding.
#[cfg(test)]
pub(crate) fn take_path_bindings() -> usize {
	PATH_BINDINGS.with(std::cell::Cell::take)
}

/// The keys from the top of a file to a block, shared by the block and its
/// children.
#[derive(Clone, Debug, Default)]
struct KeyPath(Option<Rc<KeyPathNode>>);

#[derive(Debug)]
struct KeyPathNode {
	key: String,
	len: usize,
	parent: KeyPath,
}

impl KeyPath {
	fn len(&self) -> usize {
		self.0.as_ref().map_or(0, |node| node.len)
	}

	fn is_empty(&self) -> bool {
		self.0.is_none()
	}

	fn push(&self, key: &str) -> KeyPath {
		KeyPath(Some(Rc::new(KeyPathNode {
			key: key.to_string(),
			len: self.len() + 1,
			parent: self.clone(),
		})))
	}

	fn keys(&self) -> Vec<String> {
		let mut keys = Vec::with_capacity(self.len());
		let mut node = self.0.as_ref();
		while let Some(current) = node {
			keys.push(current.key.clone());
			node = current.parent.0.as_ref();
		}
		keys.reverse();
		keys
	}
}

/// The values of a complex enum the game also assigns at runtime.
#[derive(Clone, Copy)]
struct RuntimeValues<'m> {
	dialect: &'m dyn SchemaDialect,
	name: &'m str,
}

/// The values a rule names, with where they come from.
#[derive(Clone)]
pub(crate) struct SchemaAllowedValues<'m> {
	pub(crate) kind: SchemaAllowedValueKind,
	pub(crate) name: &'m str,
	pub(crate) values: Cow<'m, [String]>,
	runtime: Option<RuntimeValues<'m>>,
}

impl std::fmt::Debug for SchemaAllowedValues<'_> {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("SchemaAllowedValues")
			.field("kind", &self.kind)
			.field("name", &self.name)
			.field("values", &self.values)
			.finish()
	}
}

impl SchemaAllowedValues<'_> {
	/// Values compare without regard to case, as CWTools compares them
	/// against enums and definitions (vanilla writes `type = adm` for `ADM`),
	/// and word by word, as the rule files and the game indent a quoted
	/// effect differently.
	pub(crate) fn contains(&self, text: &str) -> bool {
		self.values
			.iter()
			.any(|value| allowed_value_matches(value, text))
			|| self
				.runtime
				.is_some_and(|runtime| runtime.dialect.is_runtime_value(runtime.name, text))
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchemaAllowedValueKind {
	Enum,
	ComplexEnum,
	Value,
	ValueSet,
	Scope,
}

impl SchemaAllowedValueKind {
	pub(crate) fn label(self) -> &'static str {
		match self {
			Self::Enum => "enum",
			Self::ComplexEnum => "complex_enum",
			Self::Value => "value",
			Self::ValueSet => "value_set",
			Self::Scope => "scope",
		}
	}
}

fn allowed_value_matches(value: &str, text: &str) -> bool {
	let mut value_words = value.split_ascii_whitespace();
	let mut text_words = text.split_ascii_whitespace();
	loop {
		match (value_words.next(), text_words.next()) {
			(None, None) => return true,
			(Some(value_word), Some(text_word)) if value_word.eq_ignore_ascii_case(text_word) => {}
			_ => return false,
		}
	}
}

/// The `enum[...]`, `value[...]`, `value_set[...]` or `scope[...]` marker a
/// rule value is written as.
pub(crate) fn allowed_value_marker(text: &str) -> Option<(&str, &str)> {
	match parse_schema_marker(text) {
		Some((head @ ("enum" | "value" | "value_set" | "scope"), name)) => Some((head, name)),
		_ => None,
	}
}

/// The parameters a call of a scripted effect or trigger passes. CWTools
/// fills this enum from the parameters the definitions use; the rule files do
/// not list them, so any name is one.
pub(crate) const SCRIPT_PARAMETER_ENUM: &str = "enum[scripted_effect_params]";

/// The scope a scripted effect or trigger receives as a parameter
/// (`$province$ = { ... }`). CWTools fills this enum from the parameters it
/// finds; the rule files do not list it.
pub(crate) const SCRIPT_PARAMETER_KEY_ENUM: &str = "enum[scripted_effect_params_dollar]";

/// CWT type words that accept any scalar text when only presence matters.
pub(crate) const ANY_SCALAR_TYPES: &[&str] = &[
	"scalar",
	"localisation",
	"localisation_synced",
	"localisation_inline",
	"localization",
	"filepath",
	"date_field",
	"percentage_field",
	"variable_field",
	"int_variable_field",
	"value_field",
	"int_value_field",
];

/// CWT words that name a type of value rather than spell one.
const VALUE_TYPE_WORDS: &[&str] = &[
	"bool",
	"int",
	"float",
	"scope_field",
	"scalar",
	"localisation",
	"localisation_synced",
	"localisation_inline",
	"localization",
	"filepath",
	"date_field",
	"percentage_field",
	"variable_field",
	"int_variable_field",
	"value_field",
	"int_value_field",
];

/// The text a rule value spells when it names no type (`0`, `yes`, `ROOT`).
fn literal_rule_value(value: &CompiledRuleValue) -> Option<&str> {
	let CompiledRuleValue::Scalar(text) = value else {
		return None;
	};
	(!text.is_empty()
		&& !text.contains(['[', '<', '-'])
		&& !text.contains("..")
		&& !VALUE_TYPE_WORDS.contains(&text.as_str()))
	.then_some(text.as_str())
}

/// A key written as a type (`<scripted_effect>`), alone or between fixed
/// text (`monthly_<government_mechanic_power>`), stands for the name of any
/// workspace definition of that type in that place. A subtype qualifier
/// (`<area.used>`) does not narrow it here.
pub(crate) struct DefinitionKeyTemplate<'k> {
	prefix: &'k str,
	pub(crate) type_name: &'k str,
	suffix: &'k str,
}

impl DefinitionKeyTemplate<'_> {
	pub(crate) fn parse(key: &str) -> Option<DefinitionKeyTemplate<'_>> {
		if key.chars().any(char::is_whitespace) {
			return None;
		}
		let open = key.find('<')?;
		let close = open + key[open..].find('>')?;
		let (prefix, inner, suffix) = (&key[..open], &key[open + 1..close], &key[close + 1..]);
		if inner.is_empty() || suffix.contains(['<', '>']) {
			return None;
		}
		Some(DefinitionKeyTemplate {
			prefix,
			type_name: inner
				.split_once('.')
				.map_or(inner, |(type_name, _)| type_name),
			suffix,
		})
	}

	fn accepts(&self, names: &dyn DefinedNames, key: &str) -> bool {
		key.strip_prefix(self.prefix)
			.and_then(|rest| rest.strip_suffix(self.suffix))
			.is_some_and(|name| !name.is_empty() && names.defines(self.type_name, name))
	}
}

/// The rule fields a context lists, a subtype's before its type's.
pub(crate) fn rule_fields(
	context: RuleContext<'_>,
) -> std::iter::Chain<
	std::slice::Iter<'_, CompiledRuleField>,
	std::slice::Iter<'_, CompiledRuleField>,
> {
	let (first, second): (&[CompiledRuleField], &[CompiledRuleField]) = match context {
		RuleContext::RootType(root) => (&root.rules, &[]),
		RuleContext::Subtype(root, subtype) => (&subtype.rules, &root.rules),
		RuleContext::RuleField(field) => match &field.value {
			CompiledRuleValue::Block(children) => (children, &[]),
			_ => (&[], &[]),
		},
		RuleContext::AliasRules(rules) => (rules, &[]),
	};
	first.iter().chain(second.iter())
}

/// The rule that accepts the keys of an alias category in `rules`
/// (`alias_name[effect]`).
pub(crate) fn alias_wildcard<'a>(
	rules: RuleContext<'a>,
	subtypes: &HashSet<String>,
	category: &CompiledAliasCategory,
) -> Option<&'a CompiledRuleField> {
	rule_fields(rules).find(|field| {
		conditions_match(&field.conditions, subtypes)
			&& parse_schema_marker(&field.key) == Some(("alias_name", category.as_str()))
	})
}

/// Whether `rules` accept the keys of an alias category.
pub(crate) fn rules_admit(
	rules: RuleContext<'_>,
	subtypes: &HashSet<String>,
	category: &CompiledAliasCategory,
) -> bool {
	alias_wildcard(rules, subtypes, category).is_some()
}

pub(crate) fn conditions_match(
	conditions: &[CompiledRuleCondition],
	subtypes: &HashSet<String>,
) -> bool {
	conditions.iter().all(|condition| match condition {
		CompiledRuleCondition::SubtypeActive(label) => subtypes.contains(label),
		CompiledRuleCondition::SubtypeInactive(label) => !subtypes.contains(label),
	})
}

pub(crate) fn child_context_for_field_match(
	field_match: CompiledBindFieldMatch<'_>,
) -> RuleContext<'_> {
	match field_match {
		CompiledBindFieldMatch::Field(field) => RuleContext::RuleField(field),
		CompiledBindFieldMatch::Alias { alias, .. } => {
			RuleContext::AliasRules(alias.rules.as_slice())
		}
	}
}

/// The `replace_scope` a block's own rule declares, or the alias's it is the
/// body of.
fn own_replace_scope<'a>(
	context: RuleContext<'a>,
	alias: Option<&'a CompiledAlias>,
) -> Option<&'a BTreeMap<String, String>> {
	let attributes = match context {
		RuleContext::RootType(_) => None,
		RuleContext::Subtype(_, subtype) => Some(&subtype.attributes),
		RuleContext::RuleField(field) => Some(&field.attributes),
		RuleContext::AliasRules(_) => alias.map(|alias| &alias.attributes),
	};
	attributes
		.map(|attributes| &attributes.replace_scope)
		.filter(|replace_scope| !replace_scope.is_empty())
}

/// The schema binder answers the file root and every `skip_root_key` wrapper
/// with the definition body itself, but keys at those levels name
/// definitions or wrappers, not body fields.
fn path_is_above_definitions(root: Option<&CompiledRoot>, path: &[&str]) -> bool {
	let Some(root) = root else {
		return false;
	};
	!root.type_per_file
		&& path.len() <= root.skip_root_keys.len()
		&& root
			.skip_root_keys
			.iter()
			.zip(path)
			.all(|(skip_key, key)| skip_key == "any" || skip_key == key)
}

/// A type declared without body rules (EU4 `mission`) only names
/// definitions; whatever contains them describes their bodies. A field bound
/// on the path to a block must be a block form of its key.
fn context_describes_body(context: RuleContext<'_>) -> bool {
	match context {
		RuleContext::RootType(_) | RuleContext::Subtype(..) => {
			rule_fields(context).next().is_some()
		}
		RuleContext::RuleField(field) => matches!(field.value, CompiledRuleValue::Block(_)),
		RuleContext::AliasRules(_) => true,
	}
}

/// The body of the definition `path` runs through, below any
/// `skip_root_key` wrappers. Callers narrow the document to the definition
/// being read, so the first statement with each key is that definition.
fn root_block_statements<'s, N: SchemaNode>(
	root: &CompiledRoot,
	statements: &'s [N],
	path: &[&str],
) -> Option<&'s [N]> {
	let mut current = statements;
	for segment in path.iter().take(root.skip_root_keys.len() + 1) {
		current = current.iter().find_map(|statement| {
			let (key, value) = statement.assignment()?;
			if key != *segment {
				return None;
			}
			value.block()
		})?;
	}
	Some(current)
}

/// The name of the definition `path` runs through: a per-file type's file
/// name, otherwise the key below its `skip_root_key` wrappers.
pub(crate) fn definition_key<'k>(
	root: &CompiledRoot,
	file_path: &'k GamePath,
	path: &[&'k str],
) -> Option<&'k str> {
	if root.type_per_file {
		return file_path.file_stem();
	}
	path.get(root.skip_root_keys.len()).copied()
}

fn numeric_key_type(rule_key: &str) -> Option<SchemaScalarType> {
	SchemaScalarType::from_rule_value(&CompiledRuleValue::Scalar(rule_key.to_string())).filter(
		|scalar_type| {
			matches!(
				scalar_type,
				SchemaScalarType::Int { .. } | SchemaScalarType::Float { .. }
			)
		},
	)
}

fn key_scalar(key: &str) -> SchemaScalar<'_> {
	if key.parse::<f64>().is_ok() {
		SchemaScalar::Number(key)
	} else {
		SchemaScalar::Identifier(key)
	}
}

/// A rule key written as a marker: a type, an enum, a value set or a scope.
pub(crate) fn is_dynamic_key_marker(key: &str) -> bool {
	is_angle_dynamic_key_marker(key)
		|| matches!(
			parse_schema_marker(key),
			Some(("enum" | "value" | "value_set" | "scope", _))
		)
}

pub(crate) fn is_angle_dynamic_key_marker(key: &str) -> bool {
	key.len() > 2
		&& key.starts_with('<')
		&& key.ends_with('>')
		&& !key.chars().any(char::is_whitespace)
}
