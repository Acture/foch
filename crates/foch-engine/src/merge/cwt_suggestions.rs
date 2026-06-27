use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use foch_core::model::ConflictKind;
use foch_cwt::{BindContext, CwtRuleField, CwtRuleValue, CwtSchemaGraph, SchemaBinding};
use foch_language::analyzer::content_family::{
	BlockMergePolicy, GameId, GameProfile, MergeKeySource,
};

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

// Building blocks for the opt-in `cwt_suggested` resolution policy
// (apply_resolution_policies). Preserved from the local CWT-merge line but not
// yet re-wired into origin's restructured materialize pipeline after the
// squash-merge integration; tracked as a follow-up.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtPolicyError {
	MissingHint { path: PathBuf, reason: String },
	AmbiguousHint { path: PathBuf, reason: &'static str },
}

impl std::fmt::Display for CwtPolicyError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::MissingHint { path, reason } => {
				write!(
					f,
					"{} has no usable CWT merge hint: {reason}",
					path.display()
				)
			}
			Self::AmbiguousHint { path, reason } => {
				write!(
					f,
					"{} has ambiguous CWT merge hints: {reason}",
					path.display()
				)
			}
		}
	}
}

pub(crate) fn cwt_schema_graph_for_profile(
	profile: &dyn GameProfile,
) -> Option<Arc<CwtSchemaGraph>> {
	match profile.game_id() {
		GameId::Eu4 => eu4_cwt_schema_graph(),
	}
}

pub fn suggest_for_conflict(
	graph: &CwtSchemaGraph,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<CwtMergeSuggestion> {
	let SchemaBinding::Bound { type_id, .. } = graph.bind_chain(file_path, ast_path) else {
		return None;
	};
	let definition = graph.types.get(&type_id)?;
	Some(CwtMergeSuggestion {
		suggested_identity_source: Some(match &definition.name_field {
			Some(field) => CwtMergeIdentity::FieldValue(field.clone()),
			None => CwtMergeIdentity::AssignmentKey,
		}),
		suggested_block_policy: rule_field_for_path(graph, file_path, ast_path)
			.and_then(|field| block_policy_for_value(&field.value)),
		schema_provenance: format!("{}:<{}>", path_namespace(file_path), type_id.as_str()),
	})
}

#[allow(dead_code)]
pub(crate) fn merge_key_source_for_file(
	graph: &CwtSchemaGraph,
	file_path: &Path,
) -> Result<MergeKeySource, CwtPolicyError> {
	match graph.root_binding(file_path) {
		SchemaBinding::Bound { type_id, .. } => {
			let definition =
				graph
					.types
					.get(&type_id)
					.ok_or_else(|| CwtPolicyError::MissingHint {
						path: file_path.to_path_buf(),
						reason: format!("missing type definition for `{}`", type_id.as_str()),
					})?;
			Ok(match &definition.name_field {
				Some(field) => MergeKeySource::FieldValue(intern_merge_key_field(field)),
				None => MergeKeySource::AssignmentKey,
			})
		}
		SchemaBinding::Dynamic { reason } => Err(CwtPolicyError::AmbiguousHint {
			path: file_path.to_path_buf(),
			reason,
		}),
		SchemaBinding::Unbound { reason } => Err(CwtPolicyError::MissingHint {
			path: file_path.to_path_buf(),
			reason,
		}),
	}
}

pub(crate) fn classify_conflict_kind(
	graph: &CwtSchemaGraph,
	file_path: &Path,
	ast_path: &[&str],
	reason: &str,
) -> Option<ConflictKind> {
	if reason.contains("deep merge of replaced block has ")
		&& !matches!(graph.root_binding(file_path), SchemaBinding::Unbound { .. })
	{
		return Some(ConflictKind::DeepMergeable);
	}

	let rule_fields = conflict_rule_fields_for_path(graph, file_path, ast_path);
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

	if (reason.contains("sibling mods inserted divergent statements at the same key")
		|| reason.contains("multiple mods replace the same block with different content"))
		&& (root_name_field(graph, file_path).is_some()
			|| (!rule_fields.is_empty()
				&& rule_fields
					.iter()
					.all(|field| matches!(field.value, CwtRuleValue::Block(_)))))
	{
		return Some(ConflictKind::DeepMergeable);
	}

	None
}

#[allow(dead_code)]
fn intern_merge_key_field(field: &str) -> &'static str {
	Box::leak(field.to_owned().into_boxed_str())
}

fn eu4_cwt_schema_graph() -> Option<Arc<CwtSchemaGraph>> {
	static EU4_CWT_SCHEMA_GRAPH: OnceLock<Option<Arc<CwtSchemaGraph>>> = OnceLock::new();
	EU4_CWT_SCHEMA_GRAPH
		.get_or_init(load_eu4_cwt_schema_graph)
		.clone()
}

fn load_eu4_cwt_schema_graph() -> Option<Arc<CwtSchemaGraph>> {
	cwt_schema_search_roots()
		.into_iter()
		.find(|root| root.is_dir())
		.and_then(|root| CwtSchemaGraph::from_directory(&root).ok())
		.map(Arc::new)
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

fn conflict_rule_fields_for_path<'g>(
	graph: &'g CwtSchemaGraph,
	file_path: &Path,
	ast_path: &[&str],
) -> Vec<&'g CwtRuleField> {
	let Some(root) = graph.bind_root(file_path) else {
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

fn fields_for_segments<'g>(
	root: &'g foch_cwt::CwtTypeDef,
	ast_path: &[&str],
) -> Vec<&'g CwtRuleField> {
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
				CwtRuleValue::Block(fields) => Some(fields.as_slice()),
				_ => None,
			})
			.collect();
		if current_rule_sets.is_empty() {
			return Vec::new();
		}
	}
	last_matches
}

fn rule_field_for_path<'g>(
	graph: &'g CwtSchemaGraph,
	file_path: &Path,
	ast_path: &[&str],
) -> Option<&'g CwtRuleField> {
	let mut context = BindContext::RootType(graph.bind_root(file_path)?);
	let mut last_field = None;
	for (index, segment) in ast_path.iter().enumerate() {
		let field = graph.bind_field(context, segment)?;
		last_field = Some(field);
		if index + 1 == ast_path.len() {
			break;
		}
		let CwtRuleValue::Block(_) = &field.value else {
			return None;
		};
		context = BindContext::RuleField(field);
	}
	last_field
}

fn root_name_field<'g>(graph: &'g CwtSchemaGraph, file_path: &Path) -> Option<&'g str> {
	graph.bind_root(file_path)?.name_field.as_deref()
}

fn block_policy_for_value(value: &CwtRuleValue) -> Option<BlockMergePolicy> {
	Some(match value {
		CwtRuleValue::Block(_) => BlockMergePolicy::Recursive,
		CwtRuleValue::Scalar(_) | CwtRuleValue::Marker(_) => BlockMergePolicy::Replace,
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
		let graph = schema_pack_graph("events");
		let suggestion =
			suggest_for_conflict(&graph, Path::new("events/example.txt"), &[]).expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(CwtMergeIdentity::FieldValue("id".to_string()))
		);
		assert_eq!(suggestion.suggested_block_policy, None);
		assert_eq!(suggestion.schema_provenance, "events:<event>");
	}

	#[test]
	fn suggests_assignment_key_when_schema_has_no_name_field() {
		let graph = binding_graph();
		let suggestion =
			suggest_for_conflict(&graph, Path::new("missions/example.txt"), &["my_mission"])
				.expect("suggestion");
		assert_eq!(
			suggestion.suggested_identity_source,
			Some(CwtMergeIdentity::AssignmentKey)
		);
		assert_eq!(suggestion.schema_provenance, "missions:<mission>");
	}

	#[test]
	fn merge_key_source_for_file_uses_name_field() {
		let graph = schema_pack_graph("events");
		assert_eq!(
			merge_key_source_for_file(&graph, Path::new("events/example.txt")),
			Ok(MergeKeySource::FieldValue("id"))
		);
	}

	#[test]
	fn merge_key_source_for_file_falls_back_to_assignment_key() {
		let graph = binding_graph();
		assert_eq!(
			merge_key_source_for_file(&graph, Path::new("missions/example.txt")),
			Ok(MergeKeySource::AssignmentKey)
		);
	}

	#[test]
	fn merge_key_source_for_file_reports_missing_root_hint() {
		let graph = schema_pack_graph("events");
		let err = merge_key_source_for_file(&graph, Path::new("decisions/example.txt"))
			.expect_err("missing root hint should fail");
		assert!(matches!(err, CwtPolicyError::MissingHint { .. }));
		assert!(err.to_string().contains("no usable CWT merge hint"));
	}

	#[test]
	fn merge_key_source_for_file_reports_ambiguous_root_hint() {
		let graph = ambiguous_root_graph();
		let err = merge_key_source_for_file(&graph, Path::new("common/foo/example.txt"))
			.expect_err("ambiguous root hint should fail");
		assert!(matches!(err, CwtPolicyError::AmbiguousHint { .. }));
		assert!(err.to_string().contains("ambiguous CWT merge hints"));
	}

	#[test]
	fn classifies_root_name_field_conflicts_as_deep_mergeable() {
		let graph = schema_pack_graph("events");
		assert_eq!(
			classify_conflict_kind(
				&graph,
				Path::new("events/example.txt"),
				&["country_event"],
				"sibling mods inserted divergent statements at the same key"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn classifies_single_cardinality_fields_as_schema_cardinality_violations() {
		let graph = binding_graph();
		assert_eq!(
			classify_conflict_kind(
				&graph,
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
		let graph = eu4_cwt_schema_graph().expect("eu4 vendor graph");
		assert_eq!(
			classify_conflict_kind(
				&graph,
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
		let graph = eu4_cwt_schema_graph().expect("eu4 vendor graph");
		assert_eq!(
			classify_conflict_kind(
				&graph,
				Path::new("common/government_reforms/test.txt"),
				&["test_reform"],
				"deep merge of replaced block has 1 unresolved sub-conflict(s)"
			),
			Some(ConflictKind::DeepMergeable)
		);
	}

	#[test]
	fn returns_none_for_unbound_schema_path() {
		let graph = schema_pack_graph("events");
		assert!(
			suggest_for_conflict(&graph, Path::new("events/example.txt"), &["missing"]).is_none()
		);
	}

	fn schema_pack_graph(name: &str) -> CwtSchemaGraph {
		let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.expect("crates dir")
			.join("foch-cwt")
			.join("tests")
			.join("fixtures")
			.join("schema-pack")
			.join(name);
		CwtSchemaGraph::from_directory(&root).expect("load schema-pack graph")
	}

	fn binding_graph() -> CwtSchemaGraph {
		let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.expect("crates dir")
			.join("foch-cwt")
			.join("tests")
			.join("fixtures")
			.join("binding");
		CwtSchemaGraph::from_directory(&root).expect("load binding graph")
	}

	fn ambiguous_root_graph() -> CwtSchemaGraph {
		let temp = tempfile::tempdir().expect("tempdir");
		std::fs::write(
			temp.path().join("ambiguous.cwt"),
			r#"
	types = {
		type[first] = {
			path = "game/common/foo"
			name_field = "id"
		}
		type[second] = {
			path = "game/common/foo"
			name_field = "name"
		}
	}
		"#,
		)
		.expect("write schema");
		CwtSchemaGraph::from_directory(temp.path()).expect("load ambiguous graph")
	}
}
