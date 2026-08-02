use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use foch_core::model::ConflictKind;
use foch_cwt::{
	CompiledRoot, CompiledRuleField, CompiledRuleValue, RuleContext, RuleEngine, SchemaBinding,
	SchemaSource, default_compiled_rule_cache_dir, load_rule_engine_from_dir,
};
use foch_language::analyzer::content_family::{BlockMergePolicy, GameId, GameProfile};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtMergeSuggestion {
	pub suggested_identity_source: Option<CwtMergeIdentity>,
	pub suggested_block_policy: Option<BlockMergePolicy>,
	pub schema_provenance: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtMergeIdentity {
	AssignmentKey,
	FieldValue(String),
}

pub(crate) fn cwt_rule_engine_for_profile(profile: &dyn GameProfile) -> Option<Arc<RuleEngine>> {
	match profile.game_id() {
		GameId::Eu4 => eu4_cwt_rule_engine(),
	}
}

pub fn suggest_for_conflict(
	engine: &RuleEngine,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<CwtMergeSuggestion> {
	let SchemaBinding::Bound { type_id, .. } = engine.bind_chain(file_path, ast_path) else {
		return None;
	};
	let definition = engine.root(type_id.as_str())?;
	Some(CwtMergeSuggestion {
		suggested_identity_source: Some(match &definition.name_field {
			Some(field) => CwtMergeIdentity::FieldValue(field.clone()),
			None => CwtMergeIdentity::AssignmentKey,
		}),
		suggested_block_policy: rule_field_for_path(engine, file_path, ast_path)
			.and_then(|field| block_policy_for_value(&field.value)),
		schema_provenance: format!("{}:<{}>", path_namespace(file_path), type_id.as_str()),
	})
}

pub(crate) fn classify_conflict_kind(
	engine: &RuleEngine,
	file_path: &Path,
	ast_path: &[&str],
	reason: &str,
) -> Option<ConflictKind> {
	if reason.contains("deep merge of replaced block has ")
		&& !matches!(
			engine.root_binding(file_path),
			SchemaBinding::Unbound { .. }
		) {
		return Some(ConflictKind::DeepMergeable);
	}

	let rule_fields = conflict_rule_fields_for_path(engine, file_path, ast_path);
	let has_single_cardinality_match = rule_fields.iter().any(|field| {
		field
			.attributes
			.cardinality
			.and_then(|(_, max)| max)
			.is_some_and(|max| max <= 1)
	});
	let has_explicit_multi_cardinality = rule_fields.iter().any(|field| {
		field
			.attributes
			.cardinality
			.and_then(|(_, max)| max)
			.is_some_and(|max| max > 1)
	});
	if has_single_cardinality_match && !has_explicit_multi_cardinality {
		return Some(ConflictKind::SchemaCardinalityViolation);
	}

	if reason.starts_with("policy:")
		&& ast_path.len() > 1
		&& !matches!(
			engine.root_binding(file_path),
			SchemaBinding::Unbound { .. }
		) {
		return Some(ConflictKind::DeepMergeable);
	}

	if (reason.contains("sibling mods inserted divergent statements at the same key")
		|| reason.contains("multiple mods replace the same block with different content"))
		&& (root_name_field(engine, file_path).is_some()
			|| (!rule_fields.is_empty()
				&& rule_fields
					.iter()
					.all(|field| matches!(field.value, CompiledRuleValue::Block(_)))))
	{
		return Some(ConflictKind::DeepMergeable);
	}

	None
}

fn eu4_cwt_rule_engine() -> Option<Arc<RuleEngine>> {
	static EU4_CWT_RULE_ENGINE: OnceLock<Option<Arc<RuleEngine>>> = OnceLock::new();
	EU4_CWT_RULE_ENGINE
		.get_or_init(load_eu4_cwt_rule_engine)
		.clone()
}

fn load_eu4_cwt_rule_engine() -> Option<Arc<RuleEngine>> {
	let root = cwt_schema_search_roots()
		.into_iter()
		.find(|root| root.is_dir())?;
	let cache_dir = default_compiled_rule_cache_dir();
	load_rule_engine_from_dir(
		&root,
		SchemaSource::UserProvided { path: root.clone() },
		Some(&cache_dir),
	)
	.ok()
	.map(|load| load.engine)
}

fn cwt_schema_search_roots() -> Vec<PathBuf> {
	let mut roots = std::env::var_os("FOCH_CWTOOLS_SCHEMA_DIR")
		.map(PathBuf::from)
		.into_iter()
		.collect::<Vec<_>>();
	let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("crates dir")
		.parent()
		.expect("workspace root")
		.to_path_buf();
	roots.push(workspace_root.join("vendor").join("cwtools-eu4-config"));
	roots.push(workspace_root.join("output").join("cwtools-eu4-config"));
	roots
}

fn conflict_rule_fields_for_path<'e>(
	engine: &'e RuleEngine,
	file_path: &Path,
	ast_path: &[&str],
) -> Vec<&'e CompiledRuleField> {
	let Some(root) = engine.bind_root(file_path) else {
		return Vec::new();
	};
	let mut matches = fields_for_segments(root, ast_path);
	if let [_root_instance, rest @ ..] = ast_path
		&& !rest.is_empty()
	{
		matches.extend(fields_for_segments(root, rest));
	}
	matches
}

fn fields_for_segments<'e>(
	root: &'e CompiledRoot,
	ast_path: &[&str],
) -> Vec<&'e CompiledRuleField> {
	if ast_path.is_empty() {
		return Vec::new();
	}
	let mut current_rule_sets = vec![root.rules.as_slice()];
	current_rule_sets.extend(root.subtypes.iter().map(|subtype| subtype.rules.as_slice()));
	let mut last_matches = Vec::new();
	for (index, segment) in ast_path.iter().enumerate() {
		let mut matches = Vec::new();
		for rules in &current_rule_sets {
			matches.extend(rules.iter().filter(|field| field.key == *segment));
		}
		if matches.is_empty() {
			return Vec::new();
		}
		last_matches = matches;
		if index + 1 == ast_path.len() {
			return last_matches;
		}
		current_rule_sets = last_matches
			.iter()
			.filter_map(|field| match &field.value {
				CompiledRuleValue::Block(fields) => Some(fields.as_slice()),
				_ => None,
			})
			.collect();
		if current_rule_sets.is_empty() {
			return Vec::new();
		}
	}
	last_matches
}

fn rule_field_for_path<'e>(
	engine: &'e RuleEngine,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<&'e CompiledRuleField> {
	let mut context = RuleContext::RootType(engine.bind_root(file_path)?);
	let mut last_field = None;
	for (index, segment) in ast_path.iter().enumerate() {
		let field = engine.bind_field(context, segment)?;
		last_field = Some(field);
		if index + 1 == ast_path.len() {
			break;
		}
		let CompiledRuleValue::Block(_) = &field.value else {
			return None;
		};
		context = RuleContext::RuleField(field);
	}
	last_field
}

fn root_name_field<'e>(engine: &'e RuleEngine, file_path: &Path) -> Option<&'e str> {
	engine.bind_root(file_path)?.name_field.as_deref()
}

fn block_policy_for_value(value: &CompiledRuleValue) -> Option<BlockMergePolicy> {
	Some(match value {
		CompiledRuleValue::Block(_) => BlockMergePolicy::Recursive,
		CompiledRuleValue::Scalar(_) | CompiledRuleValue::Marker(_) => BlockMergePolicy::Replace,
	})
}

fn path_namespace(file_path: &Path) -> String {
	let normalized = file_path.to_string_lossy().replace('\\', "/");
	let components = normalized
		.split('/')
		.filter(|segment| !segment.is_empty())
		.collect::<Vec<_>>();
	match components.as_slice() {
		[] => "unknown".to_string(),
		[only] => (*only).to_string(),
		[first, second, ..] if *first == "common" => (*second).to_string(),
		[first, ..] => (*first).to_string(),
	}
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::*;

	#[test]
	fn suggests_field_value_identity_from_name_field() {
		let engine = schema_pack_engine("events");
		let suggestion = suggest_for_conflict(&engine, Path::new("events/example.txt"), &[])
			.expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(CwtMergeIdentity::FieldValue("id".to_string()))
		);
		assert_eq!(suggestion.suggested_block_policy, None);
		assert_eq!(suggestion.schema_provenance, "events:<event>");
	}

	#[test]
	fn suggests_assignment_key_when_schema_has_no_name_field() {
		let engine = binding_engine();
		let suggestion =
			suggest_for_conflict(&engine, Path::new("missions/example.txt"), &["my_mission"])
				.expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(CwtMergeIdentity::AssignmentKey)
		);
		assert_eq!(suggestion.schema_provenance, "missions:<mission>");
	}

	#[test]
	fn classifies_root_name_field_conflicts_as_deep_mergeable() {
		let engine = schema_pack_engine("events");
		assert_eq!(
			classify_conflict_kind(
				&engine,
				Path::new("events/example.txt"),
				&["country_event"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn classifies_tree_policy_conflicts_below_bound_roots_as_deep_mergeable() {
		let engine = schema_pack_engine("events");
		assert_eq!(
			classify_conflict_kind(
				&engine,
				Path::new("events/example.txt"),
				&["country_event", "immediate"],
				"policy: divergent block revisions remain unresolved"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn classifies_single_cardinality_fields_as_schema_cardinality_violations() {
		let engine = binding_engine();
		assert_eq!(
			classify_conflict_kind(
				&engine,
				Path::new("missions/example.txt"),
				&["provinces_to_highlight"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::SchemaCardinalityViolation)
		);
	}

	#[test]
	#[ignore = "requires vendor/cwtools-eu4-config, output/cwtools-eu4-config, or FOCH_CWTOOLS_SCHEMA_DIR"]
	fn classifies_vendor_country_history_cardinality_conflict() {
		let engine = eu4_cwt_rule_engine().expect("eu4 vendor rules");
		assert_eq!(
			classify_conflict_kind(
				&engine,
				Path::new("history/countries/TES - Test.txt"),
				&["government_rank"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::SchemaCardinalityViolation)
		);
	}

	#[test]
	#[ignore = "requires vendor/cwtools-eu4-config, output/cwtools-eu4-config, or FOCH_CWTOOLS_SCHEMA_DIR"]
	fn classifies_vendor_recursive_block_conflict_as_deep_mergeable() {
		let engine = eu4_cwt_rule_engine().expect("eu4 vendor rules");
		assert_eq!(
			classify_conflict_kind(
				&engine,
				Path::new("common/government_reforms/test.txt"),
				&["test_reform"],
				"deep merge of replaced block has 1 unresolved sub-conflict(s)"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn returns_none_for_unbound_schema_path() {
		let engine = schema_pack_engine("events");
		assert!(
			suggest_for_conflict(&engine, Path::new("events/example.txt"), &["missing"]).is_none()
		);
	}

	fn schema_pack_engine(name: &str) -> RuleEngine {
		let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.expect("crates dir")
			.join("foch-cwt")
			.join("tests")
			.join("fixtures")
			.join("schema-pack")
			.join(name);
		let graph =
			foch_cwt::CwtSchemaGraph::from_directory(&root).expect("load schema-pack graph");
		RuleEngine::from_graph(&graph)
	}

	fn binding_engine() -> RuleEngine {
		let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.expect("crates dir")
			.join("foch-cwt")
			.join("tests")
			.join("fixtures")
			.join("binding");
		let graph = foch_cwt::CwtSchemaGraph::from_directory(&root).expect("load binding graph");
		RuleEngine::from_graph(&graph)
	}
}
