use std::path::{Path, PathBuf};

use foch_cwt::{
	CompiledRulePack, CompiledRuleValue, CwtSchemaGraph, RuleContext, RuleEngine,
	RuleEngineLoadStatus, SchemaBinding, SchemaSource, load_rule_engine_from_dir,
};

#[test]
fn compiled_engine_matches_graph_root_and_chain_binding() {
	let graph = load_binding_graph();
	let engine = RuleEngine::from_graph(&graph);

	let root_path = Path::new("events/example.txt");
	assert_eq!(
		engine.root_binding(root_path),
		graph.root_binding(root_path)
	);

	let ast_path = ["country_event", "trigger", "is_year"];
	assert_eq!(
		engine.bind_chain(root_path, &ast_path),
		graph.bind_chain(root_path, &ast_path)
	);

	let SchemaBinding::Bound { type_id, node_id } = engine.bind_chain(root_path, &ast_path) else {
		panic!("expected compiled binding to resolve alias path");
	};
	assert_eq!(type_id.as_str(), "event");
	assert_eq!(
		node_id.0,
		"type:event:subtype:country_event/field:trigger/alias:trigger:is_year"
	);
}

#[test]
fn compiled_engine_projects_field_and_alias_metadata() {
	let graph = load_binding_graph();
	let engine = RuleEngine::from_graph(&graph);
	let event = engine
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");
	let iterator = engine
		.bind_field(RuleContext::RootType(event), "every_country")
		.expect("bind every_country");
	assert_eq!(
		iterator.attributes.scope,
		vec!["country".to_string(), "province".to_string()]
	);
	assert_eq!(iterator.attributes.push_scope.as_deref(), Some("country"));
	assert_eq!(
		iterator.attributes.description.as_deref(),
		Some("Scopes to all countries.")
	);

	let trigger = engine
		.bind_field(RuleContext::RootType(event), "trigger")
		.expect("bind trigger field");
	let alias_match = engine
		.bind_field_match(RuleContext::RuleField(trigger), "is_year")
		.expect("bind trigger alias");
	assert_eq!(alias_match.field().key, "alias_name[trigger]");
	let alias = alias_match.alias().expect("alias metadata");
	assert_eq!(alias.name, "is_year");
	assert_eq!(alias.attributes.scope, vec!["country".to_string()]);
}

#[test]
fn compiled_binary_pack_roundtrips_and_keeps_binding_semantics() {
	let graph = load_binding_graph();
	let pack = CompiledRulePack::from_graph(&graph);
	let bytes = pack.to_bytes().expect("encode compiled pack");
	let decoded = CompiledRulePack::from_bytes(&bytes).expect("decode compiled pack");
	assert_eq!(decoded, pack);

	let engine = RuleEngine::new(decoded);
	let mission = engine
		.bind_root(Path::new("missions/example.txt"))
		.expect("bind mission root after roundtrip");
	let field = engine
		.bind_field(RuleContext::RootType(mission), "provinces_to_highlight")
		.expect("bind mission field after roundtrip");
	assert_eq!(field.attributes.cardinality, Some((0, Some(1))));
	assert!(matches!(field.value, CompiledRuleValue::Block(_)));
}

#[test]
fn compiled_rule_cache_reuses_binary_pack_when_source_unchanged() {
	let root = binding_fixture_dir();
	let cache = tempfile::tempdir().expect("create compiled rule cache tempdir");

	let first = load_rule_engine_from_dir(
		&root,
		SchemaSource::UserProvided { path: root.clone() },
		Some(cache.path()),
	)
	.expect("compile fixture schema into cache");
	assert_eq!(first.status, RuleEngineLoadStatus::CompiledFromSource);
	assert!(first.cache_path.as_ref().is_some_and(|path| path.is_file()));
	assert!(first.timings.cache_read.is_some());
	assert!(first.timings.source_compile.is_some());
	assert!(first.engine.alias_count() > 0);

	let second = load_rule_engine_from_dir(
		&root,
		SchemaSource::UserProvided { path: root.clone() },
		Some(cache.path()),
	)
	.expect("load fixture schema from compiled cache");
	assert_eq!(second.status, RuleEngineLoadStatus::CacheHit);
	assert!(second.timings.cache_read.is_some());
	assert!(second.timings.source_compile.is_none());
	assert_eq!(second.source_id, first.source_id);
	assert_eq!(second.engine.alias_count(), first.engine.alias_count());
	assert_eq!(
		second.engine.root_binding(Path::new("events/example.txt")),
		first.engine.root_binding(Path::new("events/example.txt"))
	);
}

#[test]
fn compiled_vendor_pack_preserves_cwtools_alias_binding() {
	let Some(root) = vendor_schema_dir() else {
		eprintln!("skipping compiled CWTools vendor test: schema directory not available");
		return;
	};
	let graph = CwtSchemaGraph::from_directory(&root).expect("load CWTools schema graph");
	let engine = RuleEngine::from_graph(&graph);
	assert_eq!(engine.alias_count(), graph.aliases.len());
	assert!(engine.alias_count() > 2_000);

	let context = engine
		.bind_context(
			Path::new("events/example.txt"),
			&["country_event", "trigger"],
		)
		.expect("bind event trigger context");
	let field_match = engine
		.bind_field_match(context, "is_year")
		.expect("bind real CWTools trigger alias");
	assert_eq!(field_match.field().key, "alias_name[trigger]");
	assert_eq!(
		field_match.alias().map(|alias| alias.name.as_str()),
		Some("is_year")
	);

	let bytes = engine
		.pack()
		.to_bytes()
		.expect("encode vendor compiled pack");
	let decoded = CompiledRulePack::from_bytes(&bytes).expect("decode vendor compiled pack");
	let roundtripped = RuleEngine::new(decoded);
	assert_eq!(roundtripped.alias_count(), engine.alias_count());
}

fn load_binding_graph() -> CwtSchemaGraph {
	CwtSchemaGraph::from_directory(&binding_fixture_dir()).expect("load binding fixture graph")
}

fn binding_fixture_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures")
		.join("binding")
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
