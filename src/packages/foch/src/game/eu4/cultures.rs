//! EU4 culture identity indexing and scope-preserving AST adaptation.

pub(crate) mod adapt;
pub(crate) mod correspondence;
pub(crate) mod dag;
mod parameters;

use std::collections::{BTreeMap, BTreeSet};

use super::script::parser::{AstFile, AstStatement, AstValue, ScalarValue, SpanRange};
use crate::game::schema::query::{CompiledAliasCategory, CompiledRuleValue, CwtQuery, RuleContext};
use crate::model::{GamePath, GamePathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureLocation {
	pub relative_path: GamePathBuf,
	pub span: SpanRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureFinding {
	pub code: &'static str,
	pub message: String,
	pub location: Option<CultureLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureDefinition {
	pub id: String,
	pub group: String,
	pub body: AstValue,
	pub location: CultureLocation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CultureIndex {
	pub definitions: BTreeMap<String, CultureDefinition>,
	pub groups: BTreeMap<String, CultureLocation>,
	pub group_fields: BTreeMap<String, Vec<(String, AstValue)>>,
}

impl CultureIndex {
	pub(crate) fn from_documents(
		documents: &[(GamePathBuf, AstFile)],
	) -> Result<Self, Vec<CultureFinding>> {
		let mut result = Self::default();
		let mut findings = Vec::new();
		for (path, ast) in documents {
			if !is_culture_document(path) {
				continue;
			}
			for statement in &ast.statements {
				if matches!(statement, AstStatement::Comment { .. }) {
					continue;
				}
				let AstStatement::Assignment {
					key: group,
					key_span,
					value: AstValue::Block { items, .. },
					..
				} = statement
				else {
					findings.push(malformed(
						path,
						statement_span(statement),
						"expected culture group block",
					));
					continue;
				};
				if is_metadata(group) || group == "primary" {
					findings.push(malformed(
						path,
						key_span,
						"culture metadata cannot appear as a top-level group",
					));
					continue;
				}
				let group_location = location(path, key_span);
				if result
					.groups
					.insert(group.clone(), group_location.clone())
					.is_some()
				{
					findings.push(CultureFinding {
						code: "duplicate_culture_group",
						message: format!("culture group `{group}` has multiple definitions"),
						location: Some(group_location),
					});
				}
				for child in items {
					if matches!(child, AstStatement::Comment { .. }) {
						continue;
					}
					let AstStatement::Assignment {
						key,
						key_span,
						value,
						..
					} = child
					else {
						findings.push(malformed(
							path,
							statement_span(child),
							"expected culture or group metadata assignment",
						));
						continue;
					};
					if is_metadata(key) {
						validate_metadata(path, key, value, &mut findings);
						result
							.group_fields
							.entry(group.clone())
							.or_default()
							.push((key.clone(), value.clone()));
						continue;
					}
					let AstValue::Block { items, .. } = value else {
						findings.push(malformed(
							path,
							key_span,
							"expected culture definition block",
						));
						continue;
					};
					for field in items {
						match field {
							AstStatement::Comment { .. } => {}
							AstStatement::Assignment { key, value, .. }
								if is_metadata(key) || key == "primary" =>
							{
								validate_metadata(path, key, value, &mut findings)
							}
							_ => findings.push(malformed(
								path,
								statement_span(field),
								"unexpected nested culture definition or metadata",
							)),
						}
					}
					let definition = CultureDefinition {
						id: key.clone(),
						group: group.clone(),
						body: value.clone(),
						location: location(path, key_span),
					};
					if result.definitions.insert(key.clone(), definition).is_some() {
						findings.push(CultureFinding {
							code: "duplicate_culture",
							message: format!(
								"culture `{key}` has multiple definitions or group memberships"
							),
							location: Some(location(path, key_span)),
						});
					}
				}
			}
		}
		if findings.is_empty() {
			Ok(result)
		} else {
			Err(findings)
		}
	}
}

fn location(path: &GamePath, span: &SpanRange) -> CultureLocation {
	CultureLocation {
		relative_path: path.to_owned(),
		span: span.clone(),
	}
}

fn statement_span(statement: &AstStatement) -> &SpanRange {
	match statement {
		AstStatement::Assignment { span, .. }
		| AstStatement::Item { span, .. }
		| AstStatement::Comment { span, .. } => span,
	}
}

fn malformed(path: &GamePath, span: &SpanRange, message: &str) -> CultureFinding {
	CultureFinding {
		code: "malformed_culture_hierarchy",
		message: message.into(),
		location: Some(location(path, span)),
	}
}

fn is_culture_document(path: &GamePath) -> bool {
	path.is_inside(&["common", "cultures"], str::eq)
}

fn is_metadata(key: &str) -> bool {
	matches!(
		key,
		"male_names"
			| "female_names"
			| "dynasty_names"
			| "country"
			| "province"
			| "graphical_culture"
			| "second_graphical_culture"
	)
}

fn validate_metadata(
	path: &GamePath,
	key: &str,
	value: &AstValue,
	findings: &mut Vec<CultureFinding>,
) {
	let valid = match (key, value) {
		("graphical_culture" | "second_graphical_culture" | "primary", AstValue::Scalar { .. }) => {
			true
		}
		("country" | "province", AstValue::Block { .. }) => true,
		("male_names" | "female_names" | "dynasty_names", AstValue::Block { items, .. }) => {
			items.iter().all(|item| {
				matches!(
					item,
					AstStatement::Comment { .. }
						| AstStatement::Item {
							value: AstValue::Scalar { .. },
							..
						}
				)
			})
		}
		_ => false,
	};
	if !valid {
		findings.push(malformed(
			path,
			value.span(),
			&format!("malformed culture metadata `{key}`"),
		));
	}
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CultureRenameMap {
	mappings: BTreeMap<String, String>,
}

impl CultureRenameMap {
	pub(crate) fn checked(
		before: &CultureIndex,
		after: &CultureIndex,
		mappings: &BTreeMap<String, String>,
	) -> Result<Self, Vec<CultureFinding>> {
		let mut findings = Vec::new();
		let mut targets = BTreeSet::new();
		for (from, to) in mappings {
			let source = before.definitions.get(from);
			let message = if !is_literal_identity(from) || !is_literal_identity(to) {
				Some(
					"culture mapping endpoint is not a literal identifier or is ambiguous with an EU4 scope/tag value",
				)
			} else if source.is_none() {
				Some("culture mapping source does not resolve in the ancestor catalog")
			} else if !after.definitions.contains_key(to) {
				Some("culture mapping target does not resolve in the resulting catalog")
			} else if after.definitions.contains_key(from) {
				Some("culture mapping source still exists in the resulting catalog")
			} else if mappings.contains_key(to) {
				Some("culture identity mappings cannot contain self mappings, chains or cycles")
			} else if !targets.insert(to) {
				Some("many-to-one culture fusion is not an identity rename")
			} else if before.definitions.contains_key(to) {
				Some("culture rename collides with an existing ancestor identity")
			} else {
				None
			};
			if let Some(message) = message {
				findings.push(CultureFinding {
					code: "invalid_culture_mapping",
					message: format!("`{from}` -> `{to}`: {message}"),
					location: source.map(|definition| definition.location.clone()),
				});
			}
		}
		if findings.is_empty() {
			Ok(Self {
				mappings: mappings.clone(),
			})
		} else {
			Err(findings)
		}
	}

	pub(crate) fn mappings(&self) -> &BTreeMap<String, String> {
		&self.mappings
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureChange {
	pub from: String,
	pub to: String,
	pub definition: bool,
	pub location: CultureLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureGroupReference {
	pub group: String,
	pub location: CultureLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureReference {
	pub id: String,
	pub location: CultureLocation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CultureReferences {
	pub references: Vec<CultureReference>,
	pub group_references: Vec<CultureGroupReference>,
}

/// Collects typed uses for the shared semantic index without copying an AST
/// or building definition catalogs. Culture definition metadata is not script.
pub(crate) fn collect_culture_references(
	ast: &AstFile,
	relative_path: &GamePath,
) -> CultureReferences {
	let mut result = CultureReferences::default();
	if !is_culture_document(relative_path) {
		collect_references(&ast.statements, relative_path, &mut Vec::new(), &mut result);
	}
	result
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CultureTransform {
	pub ast: AstFile,
	pub changes: Vec<CultureChange>,
	pub findings: Vec<CultureFinding>,
	pub group_references: Vec<CultureGroupReference>,
	pub references: Vec<CultureReference>,
}

pub(crate) fn transform_cultures(
	ast: &AstFile,
	relative_path: &GamePath,
	mapping: &CultureRenameMap,
) -> CultureTransform {
	let mut result = CultureTransform {
		ast: ast.clone(),
		changes: Vec::new(),
		findings: Vec::new(),
		group_references: Vec::new(),
		references: Vec::new(),
	};
	let mut statements = std::mem::take(&mut result.ast.statements);
	if is_culture_document(relative_path) {
		match CultureIndex::from_documents(&[(relative_path.to_owned(), ast.clone())]) {
			Err(findings) => result.findings.extend(findings),
			Ok(_) => {
				for statement in &mut statements {
					if let AstStatement::Assignment {
						value: AstValue::Block { items, .. },
						..
					} = statement
					{
						for item in items {
							if let AstStatement::Assignment { key, key_span, .. } = item
								&& !is_metadata(key) && let Some(to) = mapping.mappings.get(key)
							{
								result.changes.push(CultureChange {
									from: key.clone(),
									to: to.clone(),
									definition: true,
									location: location(relative_path, key_span),
								});
								key.clone_from(to);
							}
						}
					}
				}
			}
		}
	} else {
		transform_statements(
			&mut statements,
			relative_path,
			&mut Vec::new(),
			mapping,
			&mut result,
		);
	}
	result.ast.statements = statements;
	if is_culture_document(relative_path)
		&& !result.changes.is_empty()
		&& let Err(findings) =
			CultureIndex::from_documents(&[(relative_path.to_owned(), result.ast.clone())])
	{
		result.ast = ast.clone();
		result.changes.clear();
		result.findings.extend(findings);
	}
	result
}

fn is_literal_identity(value: &str) -> bool {
	!value.is_empty()
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
		&& value.bytes().any(|byte| byte.is_ascii_alphabetic())
		&& !is_scope_selector(value)
		&& !matches!(
			value.to_ascii_lowercase().as_str(),
			"yes" | "no" | "noculture"
		)
}

// The compiled CWT currently retains the last overload of an alias. These
// aliases have a literal <culture> overload in effects.cwt / triggers.cwt,
// even where the retained overload is a country/province scope reference.
fn is_culture_alias(key: &str, category: &CompiledAliasCategory) -> bool {
	match category {
		CompiledAliasCategory::Trigger => matches!(
			key,
			"culture"
				| "primary_culture"
				| "accepted_culture"
				| "dominant_culture"
				| "ruler_culture"
				| "heir_culture"
				| "consort_culture"
				| "has_assimilated_culture"
		),
		CompiledAliasCategory::Effect => matches!(
			key,
			"change_culture"
				| "change_original_culture"
				| "change_primary_culture"
				| "add_accepted_culture"
				| "remove_accepted_culture"
				| "set_ruler_culture"
				| "set_heir_culture"
				| "set_consort_culture"
		),
		_ => false,
	}
}

fn is_type(value: &CompiledRuleValue, name: &str) -> bool {
	match value {
		CompiledRuleValue::Scalar(value) | CompiledRuleValue::Marker(value) => {
			value == &format!("<{name}>") || value == name
		}
		_ => false,
	}
}

fn script_context<'a>(
	schema: &'a CwtQuery,
	path: &GamePath,
	parents: &[String],
) -> Option<RuleContext<'a>> {
	let mut normalized = Vec::new();
	for parent in parents {
		let refs: Vec<_> = normalized.iter().map(String::as_str).collect();
		let context = schema.bind_context(path, &refs);
		let trigger_context = context.is_some_and(|context| {
			schema
				.bind_field_matches(context, "culture")
				.iter()
				.any(|field| {
					field
						.alias()
						.is_some_and(|alias| alias.category == CompiledAliasCategory::Trigger)
				})
		});
		let effect_context = context.is_some_and(|context| {
			schema
				.bind_field_matches(context, "change_culture")
				.iter()
				.any(|field| {
					field
						.alias()
						.is_some_and(|alias| alias.category == CompiledAliasCategory::Effect)
				})
		});
		// These selectors change the runtime scope, but retain the trigger or
		// effect grammar. Preserve them in the AST, skipping them only for CWT
		// lookup where the compiled graph cannot resolve their dynamic target.
		if (trigger_context || effect_context) && is_scope_selector(parent) {
			continue;
		}
		// Logical operators are engine built-ins; their CWT declarations are
		// commented out in scope_links.cwt. Their children retain trigger scope.
		if trigger_context
			&& matches!(
				parent.to_ascii_lowercase().as_str(),
				"and" | "or" | "not" | "nor" | "nand"
			) {
			continue;
		}
		let date = parent.split('.').collect::<Vec<_>>();
		if date.len() == 3
			&& date
				.iter()
				.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
			&& context
				.is_some_and(|context| schema.bind_field_match(context, "date_field").is_some())
		{
			normalized.push("date_field".to_string());
		} else {
			normalized.push(parent.clone());
		}
	}
	let refs: Vec<_> = normalized.iter().map(String::as_str).collect();
	schema.bind_context(path, &refs)
}

fn is_scope_selector(key: &str) -> bool {
	super::cwt::is_country_tag_text(key)
		|| super::cwt::is_province_id_text(key)
		|| key.starts_with("event_target:")
		|| key.starts_with("global_event_target:")
		|| key.split('.').all(|segment| {
			matches!(
				segment.to_ascii_lowercase().as_str(),
				"root" | "prev" | "from" | "this"
			) || super::cwt::rule_engine()
				.link(&segment.to_ascii_lowercase())
				.is_some()
		})
}

#[derive(Default)]
struct CultureFieldKinds {
	culture_key: bool,
	group_key: bool,
	culture_value: bool,
	group_value: bool,
}

fn reference_kinds(
	schema: &CwtQuery,
	context: Option<RuleContext<'_>>,
	key: &str,
) -> CultureFieldKinds {
	let mut result = CultureFieldKinds::default();
	let Some(context) = context else {
		return result;
	};
	for field in schema.bind_field_matches(context, key) {
		result.culture_key |= field.field().key == "<culture>";
		result.group_key |= field.field().key == "<culture_group>";
		result.culture_value |= is_type(field.value(), "culture")
			|| field
				.alias()
				.is_some_and(|alias| is_culture_alias(key, &alias.category));
		result.group_value |= is_type(field.value(), "culture_group")
			|| field.alias().is_some_and(|alias| {
				alias.category == CompiledAliasCategory::Trigger
					&& matches!(key, "culture_group" | "has_assimilated_culture_group")
			});
	}
	result
}

fn literal_scalar(value: &ScalarValue) -> Option<&str> {
	match value {
		ScalarValue::Identifier(text) | ScalarValue::String(text) if is_literal_identity(text) => {
			Some(text)
		}
		_ => None,
	}
}

fn group_key_context(path: &GamePath, parents: &[String], kinds: &CultureFieldKinds) -> bool {
	// The current CWT query does not retain every subtype block. This exact
	// EU4 context is declared as <culture_group> in governments_and_reforms.cwt.
	kinds.group_key
		|| (path.is_inside(&["common", "government_reforms"], str::eq)
			&& matches!(parents, [_, parent] if parent == "assimilation_cultures"))
}

fn collect_references(
	statements: &[AstStatement],
	path: &GamePath,
	parents: &mut Vec<String>,
	result: &mut CultureReferences,
) {
	let schema = super::cwt::rule_engine();
	let context = script_context(schema, path, parents);
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
		let kinds = reference_kinds(schema, context, key);
		if group_key_context(path, parents, &kinds) && is_literal_identity(key) {
			result.group_references.push(CultureGroupReference {
				group: key.clone(),
				location: location(path, key_span),
			});
		}
		if kinds.culture_key && is_literal_identity(key) {
			result.references.push(CultureReference {
				id: key.clone(),
				location: location(path, key_span),
			});
		}
		match value {
			AstValue::Block { items, .. } => {
				parents.push(key.clone());
				collect_references(items, path, parents, result);
				parents.pop();
			}
			AstValue::Scalar { value, span } if kinds.culture_value || kinds.group_value => {
				let Some(text) = literal_scalar(value) else {
					continue;
				};
				if kinds.culture_value {
					result.references.push(CultureReference {
						id: text.into(),
						location: location(path, span),
					});
				}
				if kinds.group_value {
					result.group_references.push(CultureGroupReference {
						group: text.into(),
						location: location(path, span),
					});
				}
			}
			_ => {}
		}
	}
}

fn transform_statements(
	statements: &mut [AstStatement],
	path: &GamePath,
	parents: &mut Vec<String>,
	mapping: &CultureRenameMap,
	result: &mut CultureTransform,
) {
	let schema = super::cwt::rule_engine();
	let context = script_context(schema, path, parents);
	let has_key_rename = statements.iter().any(|statement| {
		matches!(statement,
		AstStatement::Assignment { key, .. } if mapping.mappings.contains_key(key)
			&& reference_kinds(schema, context, key).culture_key)
	});
	let sibling_keys: BTreeSet<_> = if has_key_rename {
		statements
			.iter()
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect()
	} else {
		BTreeSet::new()
	};
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
		let kinds = reference_kinds(schema, context, key);
		if group_key_context(path, parents, &kinds) && is_literal_identity(key) {
			result.group_references.push(CultureGroupReference {
				group: key.clone(),
				location: location(path, key_span),
			});
		}
		// Dynamic CWT keys can refer to cultures as well: colonial region
		// weights use `<culture> = int`. Their reference location is the key,
		// while the numeric value and all neighboring resources stay intact.
		if kinds.culture_key && is_literal_identity(key) {
			if let Some(to) = mapping.mappings.get(key) {
				if sibling_keys.contains(to) {
					result.findings.push(CultureFinding {
						code: "culture_reference_key_collision",
						message: format!(
							"culture key `{key}` cannot be renamed to existing sibling `{to}`"
						),
						location: Some(location(path, key_span)),
					});
				} else {
					result.changes.push(CultureChange {
						from: key.clone(),
						to: to.clone(),
						definition: false,
						location: location(path, key_span),
					});
					key.clone_from(to);
				}
			}
			result.references.push(CultureReference {
				id: key.clone(),
				location: location(path, key_span),
			});
		}
		match value {
			AstValue::Block { items, .. } => {
				parents.push(key.clone());
				transform_statements(items, path, parents, mapping, result);
				parents.pop();
			}
			AstValue::Scalar { value, span } => {
				let Some(text) = literal_scalar(value).map(str::to_owned) else {
					continue;
				};
				if kinds.group_value {
					result.group_references.push(CultureGroupReference {
						group: text.clone(),
						location: location(path, span),
					});
				}
				if kinds.culture_value {
					result.references.push(CultureReference {
						id: mapping.mappings.get(&text).unwrap_or(&text).clone(),
						location: location(path, span),
					});
				}
				let Some(to) = mapping.mappings.get(&text) else {
					continue;
				};
				if kinds.culture_value {
					match value {
						ScalarValue::Identifier(text) | ScalarValue::String(text) => {
							text.clone_from(to)
						}
						_ => continue,
					}
					result.changes.push(CultureChange {
						from: text,
						to: to.clone(),
						definition: false,
						location: location(path, span),
					});
				} else if !kinds.group_value && key.contains("culture") {
					result.findings.push(CultureFinding {
						code: "unresolved_culture_reference",
						message: format!(
							"cannot prove that `{key} = {text}` is a literal culture reference in this context"
						),
						location: Some(location(path, span)),
					});
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn game_path(text: &str) -> crate::model::GamePathBuf {
		crate::model::GamePathBuf::parse(text).expect("test game path")
	}

	#[test]
	fn culture_group_keys_are_typed_and_ordinary_symbol_names_are_not() {
		let path = game_path("common/government_reforms/test.txt");
		let parsed = super::super::script::parser::parse_clausewitz_content(
			&path,
			"reform = { assimilation_cultures = { germanic = { discipline = 0.1 } } }",
		);
		let references = collect_culture_references(&parsed.ast, &path);
		assert_eq!(
			references
				.group_references
				.iter()
				.map(|reference| reference.group.as_str())
				.collect::<Vec<_>>(),
			["germanic"]
		);
		let path = game_path("common/scripted_effects/test.txt");
		let parsed = super::super::script::parser::parse_clausewitz_content(
			&path,
			"germanic = { add_prestige = 1 }",
		);
		assert!(
			collect_culture_references(&parsed.ast, &path)
				.group_references
				.is_empty()
		);
	}
	use crate::game::eu4::script::parser::{AstStatement, parse_clausewitz_content};

	fn parse(path: &str, source: &str) -> AstFile {
		let result = parse_clausewitz_content(&game_path(path), source);
		assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
		result.ast
	}

	fn index(source: &str) -> Result<CultureIndex, Vec<CultureFinding>> {
		let path = "common/cultures/test.txt";
		CultureIndex::from_documents(&[(game_path(path), parse(path, source))])
	}

	fn mapping() -> CultureRenameMap {
		CultureRenameMap::checked(
			&index("germanic = { old_culture = { male_names = { Johann } } }").unwrap(),
			&index("germanic = { new_culture = { male_names = { Johann } } }").unwrap(),
			&BTreeMap::from([("old_culture".into(), "new_culture".into())]),
		)
		.unwrap()
	}

	fn values(statements: &[AstStatement], key: &str) -> Vec<String> {
		let mut found = Vec::new();
		for statement in statements {
			if let AstStatement::Assignment {
				key: current,
				value,
				..
			} = statement
			{
				match value {
					AstValue::Scalar { value, .. } if current == key => found.push(value.as_text()),
					AstValue::Block { items, .. } => found.extend(values(items, key)),
					_ => {}
				}
			}
		}
		found
	}

	#[test]
	fn indexes_cultures_without_confusing_named_metadata_for_definitions() {
		let catalog = index(
			r#"germanic = {
			male_names = { Johann } female_names = { Anna } dynasty_names = { Habsburg }
			graphical_culture = westerngfx second_graphical_culture = easterngfx
			country = { discipline = 0.05 } province = { local_tax_modifier = 0.1 }
			old_culture = { primary = AUS male_names = { "old_culture" } }
		}"#,
		)
		.unwrap();
		assert_eq!(catalog.definitions.len(), 1);
		assert_eq!(catalog.definitions["old_culture"].group, "germanic");
		assert_eq!(
			catalog.definitions["old_culture"].location.span.start.line,
			5
		);
		assert!(catalog.groups.contains_key("germanic"));
	}

	#[test]
	fn rejects_duplicate_and_malformed_culture_hierarchies() {
		for source in [
			"g = { old_culture = {} old_culture = {} }",
			"g = { old_culture = {} } h = { old_culture = {} }",
			"g = { old_culture = {} } g = { new_culture = {} }",
			"g = old_culture",
			"g = { old_culture = yes }",
			"g = { stray }",
			"g = { old_culture = { nested_culture = {} } }",
			"male_names = {}",
			"country = {}",
			"graphical_culture = {}",
		] {
			assert!(
				index(source).is_err(),
				"accepted malformed hierarchy: {source}"
			);
		}
	}

	#[test]
	fn validates_identity_maps_against_both_catalogs() {
		let before = index("g = { a = {} b = {} }").unwrap();
		let after = index("g = { c = {} b = {} }").unwrap();
		for entries in [
			vec![("missing", "c")],
			vec![("a", "missing")],
			vec![("a", "c"), ("b", "c")],
			vec![("a", "b")],
			vec![("a", "a")],
			vec![("a", "b"), ("b", "a")],
		] {
			let mappings = entries
				.into_iter()
				.map(|(a, b)| (a.into(), b.into()))
				.collect();
			assert!(
				CultureRenameMap::checked(&before, &after, &mappings).is_err(),
				"accepted {mappings:?}"
			);
		}
		let mappings = BTreeMap::from([("a".into(), "c".into())]);
		assert_eq!(
			CultureRenameMap::checked(&before, &after, &mappings)
				.unwrap()
				.mappings(),
			&mappings
		);
	}

	#[test]
	fn renames_only_culture_definition_keys_and_preserves_names_and_comments() {
		let path = &game_path("common/cultures/test.txt");
		let ast = parse(
			path.as_str(),
			"germanic = { # old_culture\n old_culture = { male_names = { old_culture } } }",
		);
		let original = ast.clone();
		let result = transform_cultures(&ast, path, &mapping());
		assert_eq!(result.changes.len(), 1);
		assert!(result.changes[0].definition);
		let AstStatement::Assignment {
			value: AstValue::Block { items, .. },
			..
		} = &result.ast.statements[0]
		else {
			panic!()
		};
		assert!(matches!(&items[0], AstStatement::Comment { text, .. } if text == "old_culture"));
		assert!(matches!(&items[1], AstStatement::Assignment { key, .. } if key == "new_culture"));
		let catalog = CultureIndex::from_documents(&[(path.clone(), result.ast)]).unwrap();
		assert_eq!(
			catalog.definitions["new_culture"].body,
			CultureIndex::from_documents(&[(path.clone(), ast.clone())])
				.unwrap()
				.definitions["old_culture"]
				.body,
			"body retains original source spans"
		);
		assert_eq!(ast, original);
	}

	#[test]
	fn adapts_nested_event_reads_and_writes_without_changing_the_mechanism() {
		let path = &game_path("events/culture_mechanism.txt");
		let ast = parse(
			path.as_str(),
			r#"country_event = {
			id = culture.1 title = old_culture desc = "old_culture"
			trigger = { OR = { primary_culture = old_culture accepted_culture = old_culture } }
			immediate = {
				if = { limit = { any_owned_province = { culture = old_culture } }
					every_owned_province = { limit = { culture = old_culture } change_culture = old_culture }
					change_primary_culture = old_culture add_accepted_culture = old_culture
					set_ruler_culture = old_culture set_heir_culture = old_culture set_consort_culture = old_culture
					define_advisor = { type = artist culture = old_culture }
					set_country_flag = old_culture
				}
			}
		}"#,
		);
		let result = transform_cultures(&ast, path, &mapping());
		assert!(result.findings.is_empty(), "{:?}", result.findings);
		assert_eq!(result.changes.len(), 11, "{:?}", result);
		for key in [
			"primary_culture",
			"accepted_culture",
			"culture",
			"change_culture",
			"change_primary_culture",
			"add_accepted_culture",
			"set_ruler_culture",
			"set_heir_culture",
			"set_consort_culture",
		] {
			assert!(
				values(&result.ast.statements, key)
					.iter()
					.all(|value| value == "new_culture"),
				"unadapted {key}"
			);
		}
		assert_eq!(values(&result.ast.statements, "title"), ["old_culture"]);
		assert_eq!(values(&result.ast.statements, "desc"), ["old_culture"]);
		assert_eq!(
			values(&result.ast.statements, "set_country_flag"),
			["old_culture"]
		);
		assert_eq!(values(&ast.statements, "change_culture"), ["old_culture"]);
	}

	#[test]
	fn preserves_dynamic_values_and_tracks_group_predicates() {
		let path = &game_path("events/culture_mechanism.txt");
		let ast = parse(
			path.as_str(),
			r#"country_event = { id = culture.1
			trigger = { culture = ROOT culture = PREV culture = FROM culture = FRA culture = 123
				culture = event_target:old_culture culture = $CULTURE$ culture = new_variable:heir_culture
				culture_group = germanic }
			immediate = { change_culture = ROOT }
		}"#,
		);
		let result = transform_cultures(&ast, path, &mapping());
		assert_eq!(result.ast, ast);
		assert!(result.changes.is_empty());
		assert_eq!(result.group_references.len(), 1);
		assert_eq!(result.group_references[0].group, "germanic");
	}

	#[test]
	fn history_values_are_typed_but_unrelated_unknown_fields_are_not() {
		let path = &game_path("history/provinces/1 - Test.txt");
		let ast = parse(
			path.as_str(),
			"culture = old_culture 1500.1.1 = { culture = old_culture } name = old_culture",
		);
		let result = transform_cultures(&ast, path, &mapping());
		assert_eq!(result.changes.len(), 2);
		assert_eq!(values(&result.ast.statements, "name"), ["old_culture"]);
		let unknown = parse(
			"common/custom/test.txt",
			"custom = { culture = old_culture }",
		);
		let result = transform_cultures(&unknown, &unknown.path, &mapping());
		assert_eq!(result.ast, unknown);
		assert_eq!(result.findings.len(), 1);
		assert!(matches!(
			&result.ast.statements[0],
			AstStatement::Assignment {
				value: AstValue::Block { .. },
				..
			}
		));
	}

	#[test]
	fn adapts_country_history_and_character_culture_fields() {
		let path = &game_path("history/countries/AAA - Test.txt");
		let ast = parse(
			path.as_str(),
			r#"primary_culture = old_culture
			1500.1.1 = {
				monarch = { name = old_culture culture = old_culture }
				heir = { name = old_culture culture = old_culture }
				queen = { name = old_culture culture = old_culture }
			}
		"#,
		);
		let result = transform_cultures(&ast, path, &mapping());
		assert!(result.findings.is_empty(), "{:?}", result.findings);
		assert_eq!(result.changes.len(), 4);
		assert_eq!(
			values(&result.ast.statements, "name"),
			["old_culture", "old_culture", "old_culture"]
		);
	}

	#[test]
	fn adapts_reads_and_writes_inside_dynamic_scope_selectors() {
		let path = &game_path("events/test.txt");
		let ast = parse(
			path.as_str(),
			r#"country_event = { id = test.1
			trigger = { ROOT = { primary_culture = old_culture }
				FROM = { culture = old_culture } FRA = { culture = old_culture }
				event_target:recipient = { culture = old_culture } }
			immediate = { PREV = { change_culture = old_culture }
				123 = { change_culture = old_culture } ROOT = { set_ruler_culture = old_culture } }
		}"#,
		);
		let result = transform_cultures(&ast, path, &mapping());
		assert!(result.findings.is_empty(), "{:?}", result.findings);
		assert_eq!(result.changes.len(), 7);
		let reverse = CultureRenameMap::checked(
			&index("germanic = { new_culture = {} }").unwrap(),
			&index("germanic = { old_culture = {} }").unwrap(),
			&BTreeMap::from([("new_culture".into(), "old_culture".into())]),
		)
		.unwrap();
		assert_eq!(transform_cultures(&result.ast, path, &reverse).ast, ast);
	}

	#[test]
	fn rejects_definition_collision_without_partially_transforming_the_document() {
		let path = &game_path("common/cultures/test.txt");
		let ast = parse(path.as_str(), "g = { old_culture = {} new_culture = {} }");
		let result = transform_cultures(&ast, path, &mapping());
		assert_eq!(result.ast, ast);
		assert!(result.changes.is_empty());
		assert!(!result.findings.is_empty());
	}

	#[test]
	fn unknown_culture_commands_are_reported_without_rewriting() {
		// has_accepted_culture is not an EU4 trigger in the vendored CWT.
		let ast = parse(
			"events/test.txt",
			"country_event = { trigger = { has_accepted_culture = old_culture } }",
		);
		let result = transform_cultures(&ast, &ast.path, &mapping());
		assert_eq!(result.ast, ast);
		assert_eq!(result.findings.len(), 1);
		assert_eq!(result.findings[0].code, "unresolved_culture_reference");
	}

	#[test]
	fn records_typed_literal_references_even_when_no_mapping_is_applied() {
		let ast = parse(
			"events/test.txt",
			"country_event = { trigger = { culture = new_culture culture = ROOT culture_group = germanic } }",
		);
		let result = transform_cultures(&ast, &ast.path, &CultureRenameMap::default());
		assert_eq!(result.ast, ast);
		assert_eq!(result.references.len(), 1);
		assert_eq!(result.references[0].id, "new_culture");
		assert_eq!(result.group_references.len(), 1);
	}

	#[test]
	fn schema_scope_values_and_booleans_are_never_literal_culture_references() {
		let ast = parse(
			"events/test.txt",
			r#"country_event = {
			trigger = { culture = overlord culture = tribal_owner culture = native_sponsor_scope
				culture = yes culture = no culture_group = overlord }
			immediate = { change_culture = overlord }
		}"#,
		);
		let result = transform_cultures(&ast, &ast.path, &mapping());
		assert_eq!(result.ast, ast);
		assert!(result.references.is_empty(), "{:?}", result.references);
		assert!(result.group_references.is_empty());
		for id in [
			"overlord",
			"tribal_owner",
			"native_sponsor_scope",
			"yes",
			"no",
		] {
			let before = index(&format!("g = {{ {id} = {{}} }}")).unwrap();
			let after = index("g = { new_culture = {} }").unwrap();
			assert!(
				CultureRenameMap::checked(
					&before,
					&after,
					&BTreeMap::from([(id.into(), "new_culture".into())])
				)
				.is_err(),
				"accepted dynamic endpoint {id}"
			);
		}
	}

	#[test]
	fn linked_scope_selectors_preserve_typed_reference_context() {
		let ast = parse(
			"events/test.txt",
			r#"province_event = {
			trigger = { owner = { primary_culture = old_culture }
				controller = { accepted_culture = old_culture }
				ROOT.owner = { capital = { culture = old_culture } } }
			immediate = { owner = { change_primary_culture = old_culture }
				ROOT.owner.capital = { change_culture = old_culture }
				native_sponsor_scope = { set_ruler_culture = old_culture } }
		}"#,
		);
		let result = transform_cultures(&ast, &ast.path, &mapping());
		assert!(result.findings.is_empty(), "{:?}", result.findings);
		assert_eq!(result.changes.len(), 6);
		assert_eq!(result.references.len(), 6);
	}

	#[test]
	fn culture_weight_keys_are_typed_references_with_key_locations() {
		let source =
			"colonial_test = { culture = { old_culture = 10 } trade_goods = { old_culture = 5 } }";
		let ast = parse("common/colonial_regions/test.txt", source);
		let before = transform_cultures(&ast, &ast.path, &CultureRenameMap::default());
		assert_eq!(before.references.len(), 1);
		assert_eq!(before.references[0].id, "old_culture");
		assert_eq!(
			before.references[0].location.span.start.offset,
			source.find("old_culture").unwrap()
		);
		let result = transform_cultures(&ast, &ast.path, &mapping());
		assert_eq!(result.changes.len(), 1);
		assert!(!result.changes[0].definition);
		assert_eq!(result.references[0].id, "new_culture");
		assert_eq!(values(&result.ast.statements, "new_culture"), ["10"]);
		assert_eq!(values(&result.ast.statements, "old_culture"), ["5"]);
	}

	#[test]
	fn rejects_identity_mapping_when_the_source_still_exists_afterward() {
		let before = index("g = { old_culture = {} }").unwrap();
		let after = index("g = { old_culture = {} new_culture = {} }").unwrap();
		let mapping = BTreeMap::from([("old_culture".into(), "new_culture".into())]);
		assert!(CultureRenameMap::checked(&before, &after, &mapping).is_err());
	}

	#[test]
	fn culture_weight_key_collision_does_not_silently_combine_gameplay_weights() {
		let ast = parse(
			"common/colonial_regions/test.txt",
			"colonial_test = { culture = { old_culture = 10 new_culture = 5 } }",
		);
		let result = transform_cultures(&ast, &ast.path, &mapping());
		assert_eq!(result.ast, ast);
		assert!(result.changes.is_empty());
		assert!(!result.findings.is_empty());
	}

	#[test]
	fn borrowed_reference_collection_agrees_with_identity_transformation() {
		for (path, source) in [
			(
				"events/test.txt",
				"province_event = { trigger = { ROOT.owner = { primary_culture = old_culture culture_group = germanic } culture = ROOT culture = yes } immediate = { owner = { change_primary_culture = old_culture } } }",
			),
			(
				"history/provinces/1 - Test.txt",
				"culture = old_culture 1500.1.1 = { culture = new_culture }",
			),
			(
				"common/colonial_regions/test.txt",
				"colonial_test = { culture = { old_culture = 10 } trade_goods = { old_culture = 5 } }",
			),
			(
				"common/cultures/test.txt",
				"germanic = { old_culture = { male_names = { old_culture } } }",
			),
			(
				"common/custom/test.txt",
				"custom = { culture = old_culture }",
			),
		] {
			let ast = parse(path, source);
			let collected = collect_culture_references(&ast, &ast.path);
			let transformed = transform_cultures(&ast, &ast.path, &CultureRenameMap::default());
			assert_eq!(
				collected.references, transformed.references,
				"culture uses in {path}"
			);
			assert_eq!(
				collected.group_references, transformed.group_references,
				"culture groups in {path}"
			);
		}
	}
}
