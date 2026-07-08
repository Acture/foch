use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use foch_cwt::{
	AliasCategory, CwtAlias, CwtRuleField, CwtRuleValue, CwtSchemaGraph, CwtSubtype, CwtTypeDef,
	SchemaBinding, SchemaPack, SchemaSource,
};
use foch_syntax::ParadoxTree;
use serde::Serialize;
use serde_json::Value;
use walkdir::WalkDir;

#[derive(Serialize)]
struct FixtureBaseline {
	pack_id: String,
	types: Vec<TypeBaseline>,
	aliases: Vec<AliasBaseline>,
	enums: BTreeMap<String, Vec<String>>,
	value_sets: BTreeMap<String, Vec<String>>,
	scopes: Vec<String>,
	bindings: Vec<BindingBaseline>,
}

#[derive(Serialize)]
struct VendorBaseline {
	schema_commit: Option<String>,
	file_count: usize,
	pack_id: String,
	type_count: usize,
	alias_count: usize,
	enum_count: usize,
	value_set_count: usize,
	scope_count: usize,
	selected_types: Vec<TypeBaseline>,
	selected_aliases: Vec<AliasBaseline>,
	selected_bindings: Vec<BindingBaseline>,
	known_scopes: Vec<String>,
}

#[derive(Serialize)]
struct TypeBaseline {
	name: String,
	path: Option<String>,
	name_field: Option<String>,
	push_scope: Option<String>,
	type_per_file: bool,
	name_from_file: bool,
	skip_root_keys: Vec<String>,
	subtypes: Vec<SubtypeBaseline>,
	rules: Vec<RuleFieldBaseline>,
}

#[derive(Serialize)]
struct SubtypeBaseline {
	name: String,
	rules: Vec<RuleFieldBaseline>,
}

#[derive(Serialize)]
struct AliasBaseline {
	category: String,
	name: String,
	rules: Vec<RuleFieldBaseline>,
}

#[derive(Serialize)]
struct RuleFieldBaseline {
	key: String,
	value: RuleValueBaseline,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuleValueBaseline {
	Scalar { value: String },
	Marker { value: String },
	Block { fields: Vec<RuleFieldBaseline> },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum BindingBaseline {
	Bound {
		path: String,
		type_id: String,
		node_id: String,
	},
	Dynamic {
		path: String,
		reason: String,
	},
	Unbound {
		path: String,
		reason: String,
	},
}

#[test]
fn fixture_schema_pack_matches_baseline() {
	let root = fixture_schema_dir();
	assert_schema_parses_cleanly(&root);
	let actual = build_fixture_baseline(&root);
	assert_json_fixture(&fixture_file("binding_baseline.json"), &actual);
}

#[test]
fn fixture_root_binding_prefers_exact_path_and_reports_ambiguity() {
	let root = fixture_schema_dir();
	let graph = CwtSchemaGraph::from_directory(&root).expect("load schema fixture");

	let event_binding = graph.root_binding(Path::new("events/example.txt"));
	let SchemaBinding::Bound { type_id, .. } = event_binding else {
		panic!("expected bound events root, got {event_binding:?}");
	};
	assert_eq!(type_id.as_str(), "event");

	let missions_binding = graph.root_binding(Path::new("missions/example.txt"));
	let SchemaBinding::Dynamic { reason } = missions_binding else {
		panic!("expected dynamic missions root, got {missions_binding:?}");
	};
	assert_eq!(reason, "ambiguous-root-type");

	let missing_binding = graph.root_binding(Path::new("common/example.txt"));
	let SchemaBinding::Unbound { reason } = missing_binding else {
		panic!("expected unbound common root, got {missing_binding:?}");
	};
	assert_eq!(reason, "no root type matches `common/example.txt`");
}

#[test]
fn cwtools_schema_pack_matches_baseline() {
	let Some(root) = vendor_schema_dir() else {
		eprintln!("skipping CWTools schema baseline: vendor schema directory not available");
		return;
	};
	assert_schema_parses_cleanly(&root);
	let actual = build_vendor_baseline(&root);
	assert_json_fixture(&fixture_file("cwtools_binding_baseline.json"), &actual);
}

fn build_fixture_baseline(root: &Path) -> FixtureBaseline {
	let pack = load_pack(root);
	let graph = pack.graph.as_ref();
	FixtureBaseline {
		pack_id: pack.id.to_hex(),
		types: sorted_type_baselines(graph),
		aliases: sorted_alias_baselines(graph),
		enums: sorted_string_map(&graph.enums),
		value_sets: sorted_string_map(&graph.value_sets),
		scopes: graph.scopes.clone(),
		bindings: vec![
			binding_baseline(
				"events/example.txt",
				graph.root_binding(Path::new("events/example.txt")),
			),
			binding_baseline(
				"missions/example.txt",
				graph.root_binding(Path::new("missions/example.txt")),
			),
			binding_baseline(
				"common/example.txt",
				graph.root_binding(Path::new("common/example.txt")),
			),
		],
	}
}

fn build_vendor_baseline(root: &Path) -> VendorBaseline {
	let pack = load_pack(root);
	let graph = pack.graph.as_ref();
	VendorBaseline {
		schema_commit: git_head(root),
		file_count: cwt_files(root).len(),
		pack_id: pack.id.to_hex(),
		type_count: graph.types.len(),
		alias_count: graph.aliases.len(),
		enum_count: graph.enums.len(),
		value_set_count: graph.value_sets.len(),
		scope_count: graph.scopes.len(),
		selected_types: [
			"decision",
			"event",
			"game_age_ability",
			"mission",
			"mission_series",
			"opinion_modifier",
		]
		.into_iter()
		.filter_map(|name| {
			graph
				.types
				.values()
				.find(|definition| definition.name.as_str() == name)
		})
		.map(type_baseline)
		.collect(),
		selected_aliases: [
			(AliasCategory::Trigger, "is_year"),
			(AliasCategory::Effect, "add_prestige"),
			(
				AliasCategory::Other("modifier_rule".to_string()),
				"modifier",
			),
		]
		.into_iter()
		.filter_map(|(category, name)| graph.aliases.get(&(category, name.to_string())))
		.map(alias_baseline)
		.collect(),
		selected_bindings: vec![
			binding_baseline(
				"events/example.txt",
				graph.root_binding(Path::new("events/example.txt")),
			),
			binding_baseline(
				"decisions/example.txt",
				graph.root_binding(Path::new("decisions/example.txt")),
			),
			binding_baseline(
				"missions/example.txt",
				graph.root_binding(Path::new("missions/example.txt")),
			),
			binding_baseline(
				"common/opinion_modifiers/example.txt",
				graph.root_binding(Path::new("common/opinion_modifiers/example.txt")),
			),
		],
		known_scopes: graph
			.scopes
			.iter()
			.filter(|scope| matches!(scope.as_str(), "country" | "province" | "trade_node"))
			.cloned()
			.collect(),
	}
}

fn load_pack(root: &Path) -> SchemaPack {
	SchemaPack::load_from_dir(
		root,
		SchemaSource::UserProvided {
			path: root.to_path_buf(),
		},
	)
	.expect("load schema pack")
}

fn sorted_type_baselines(graph: &CwtSchemaGraph) -> Vec<TypeBaseline> {
	let mut types = graph.types.values().map(type_baseline).collect::<Vec<_>>();
	types.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
	types
}

fn sorted_alias_baselines(graph: &CwtSchemaGraph) -> Vec<AliasBaseline> {
	let mut aliases = graph
		.aliases
		.values()
		.map(alias_baseline)
		.collect::<Vec<_>>();
	aliases.sort_by(|lhs, rhs| {
		lhs.category
			.cmp(&rhs.category)
			.then_with(|| lhs.name.cmp(&rhs.name))
	});
	aliases
}

fn type_baseline(definition: &CwtTypeDef) -> TypeBaseline {
	TypeBaseline {
		name: definition.name.as_str().to_string(),
		path: definition.path.clone(),
		name_field: definition.name_field.clone(),
		push_scope: definition.push_scope.clone(),
		type_per_file: definition.type_per_file,
		name_from_file: definition.name_from_file,
		skip_root_keys: definition.skip_root_keys.clone(),
		subtypes: definition.subtypes.iter().map(subtype_baseline).collect(),
		rules: definition.rules.iter().map(rule_field_baseline).collect(),
	}
}

fn subtype_baseline(subtype: &CwtSubtype) -> SubtypeBaseline {
	SubtypeBaseline {
		name: subtype.name.clone(),
		rules: subtype.rules.iter().map(rule_field_baseline).collect(),
	}
}

fn alias_baseline(alias: &CwtAlias) -> AliasBaseline {
	AliasBaseline {
		category: alias_category_name(&alias.category).to_string(),
		name: alias.name.clone(),
		rules: alias.rules.iter().map(rule_field_baseline).collect(),
	}
}

fn rule_field_baseline(field: &CwtRuleField) -> RuleFieldBaseline {
	RuleFieldBaseline {
		key: field.key.clone(),
		value: rule_value_baseline(&field.value),
	}
}

fn rule_value_baseline(value: &CwtRuleValue) -> RuleValueBaseline {
	match value {
		CwtRuleValue::Scalar(value) => RuleValueBaseline::Scalar {
			value: value.clone(),
		},
		CwtRuleValue::Marker(value) => RuleValueBaseline::Marker {
			value: value.clone(),
		},
		CwtRuleValue::Block(fields) => RuleValueBaseline::Block {
			fields: fields.iter().map(rule_field_baseline).collect(),
		},
	}
}

fn binding_baseline(path: &str, binding: SchemaBinding) -> BindingBaseline {
	match binding {
		SchemaBinding::Bound { type_id, node_id } => BindingBaseline::Bound {
			path: path.to_string(),
			type_id: type_id.as_str().to_string(),
			node_id: node_id.0,
		},
		SchemaBinding::Dynamic { reason } => BindingBaseline::Dynamic {
			path: path.to_string(),
			reason: reason.to_string(),
		},
		SchemaBinding::Unbound { reason } => BindingBaseline::Unbound {
			path: path.to_string(),
			reason,
		},
	}
}

fn sorted_string_map(
	map: &std::collections::HashMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
	map.iter()
		.map(|(key, values)| (key.clone(), values.clone()))
		.collect()
}

fn assert_schema_parses_cleanly(root: &Path) {
	let failures = cwt_files(root)
		.into_iter()
		.filter_map(|path| {
			let bytes = fs::read(&path).expect("read schema file");
			let tree = ParadoxTree::parse(&bytes).expect("parse schema file");
			tree.has_error().then(|| relative_display(root, &path))
		})
		.collect::<Vec<_>>();
	assert!(
		failures.is_empty(),
		"tree-sitter produced parse errors for: {}",
		failures.join(", ")
	);
}

fn assert_json_fixture(path: &Path, actual: &impl Serialize) {
	let actual = serde_json::to_value(actual).expect("serialize baseline");
	if should_regenerate_baselines() || !path.exists() {
		fs::write(
			path,
			serde_json::to_string_pretty(&actual).expect("format baseline json"),
		)
		.expect("write baseline json");
	}
	let expected =
		serde_json::from_str::<Value>(&fs::read_to_string(path).expect("read baseline json"))
			.expect("parse baseline json");
	assert_eq!(expected, actual);
}

fn should_regenerate_baselines() -> bool {
	std::env::var_os("FOCH_REGENERATE_BASELINES").is_some()
}

fn git_head(root: &Path) -> Option<String> {
	let output = Command::new("git")
		.arg("-C")
		.arg(root)
		.arg("rev-parse")
		.arg("HEAD")
		.output()
		.ok()?;
	output
		.status
		.success()
		.then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn fixture_schema_dir() -> PathBuf {
	fixture_file("schema-pack")
}

fn fixture_file(path: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures")
		.join(path)
}

fn vendor_schema_dir() -> Option<PathBuf> {
	let from_env = std::env::var_os("FOCH_CWTOOLS_SCHEMA_DIR").map(PathBuf::from);
	let candidate_paths = [
		from_env,
		Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/cwtools-eu4-config")),
		Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../output/cwtools-eu4-config")),
	];
	candidate_paths
		.into_iter()
		.flatten()
		.find(|path| path.is_dir())
}

fn cwt_files(root: &Path) -> Vec<PathBuf> {
	let mut files = WalkDir::new(root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("cwt"))
		.map(|entry| entry.into_path())
		.collect::<Vec<_>>();
	files.sort();
	files
}

fn relative_display(root: &Path, path: &Path) -> String {
	path.strip_prefix(root)
		.unwrap_or(path)
		.to_string_lossy()
		.replace('\\', "/")
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
