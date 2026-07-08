use std::path::{Path, PathBuf};

use foch_cwt::{
	CompiledRulePack, CompiledRuleValue, CompiledSeverity, CwtSchemaGraph, RuleContext, RuleEngine,
	RuleEngineLoadStatus, SchemaBinding, SchemaSource, load_rule_engine_from_dir,
};
use foch_syntax::ParadoxTree;

#[test]
fn compiled_engine_matches_graph_root_and_chain_binding() {
	let graph = load_binding_graph();
	let engine = RuleEngine::from_graph(&graph);

	let root_path = Path::new("events/example.txt");
	assert_eq!(
		engine.root_binding(root_path),
		graph.root_binding(root_path)
	);
	let root = engine.bind_root(root_path).expect("bind event root");
	assert_eq!(
		root.subtypes
			.iter()
			.find(|subtype| subtype.name == "country")
			.and_then(|subtype| subtype.attributes.push_scope.as_deref()),
		Some("country")
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
	assert_eq!(alias.value, CompiledRuleValue::Scalar("int".to_string()));
	assert_eq!(alias.attributes.scope, vec!["country".to_string()]);
}

#[test]
fn compiled_engine_binds_angle_bracket_dynamic_fields() {
	let schema = r#"
	types = {
		type[mission] = {
			path = "game/missions"
		}
	}

	mission = {
		mission_tree = {
			<mission_stage> = {
				trigger = bool
			}
		}
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let engine = RuleEngine::from_graph(&graph);
	let path = Path::new("missions/example.txt");
	let ast_path = ["demo_mission", "mission_tree", "conquest", "trigger"];

	assert_eq!(
		engine.bind_chain(path, &ast_path),
		graph.bind_chain(path, &ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } = engine.bind_chain(path, &ast_path) else {
		panic!("expected compiled dynamic field binding");
	};
	assert_eq!(type_id.as_str(), "mission");
	assert_eq!(
		node_id.0,
		"type:mission:field:mission_tree/field:<mission_stage>/field:trigger"
	);

	let context = engine
		.bind_context(path, &["demo_mission", "mission_tree"])
		.expect("bind mission tree context");
	let field_match = engine
		.bind_field_match(context, "conquest")
		.expect("bind dynamic mission stage");
	assert_eq!(field_match.field().key, "<mission_stage>");
}

#[test]
fn compiled_engine_matches_root_type_key_filter_exclusions() {
	let schema = r#"
	types = {
		type[idea_group] = {
			path = "game/common/ideas"
			subtype[selectable] = {
				category = scalar
			}
		}
		## type_key_filter <> { start trigger bonus ai_will_do }
		type[idea] = {
			path = "game/common/ideas"
			skip_root_key = any
		}
	}

	idea_group = {
		subtype[selectable] = {
			category = scalar
		}
	}

	idea = {
		idea_only = bool
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let engine = RuleEngine::from_graph(&graph);
	let path = Path::new("common/ideas/example.txt");
	let ast_path = ["sample_group", "sample_idea", "idea_only"];

	assert_eq!(
		engine.bind_chain(path, &ast_path),
		graph.bind_chain(path, &ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } = engine.bind_chain(path, &ast_path) else {
		panic!("expected compiled idea binding");
	};
	assert_eq!(type_id.as_str(), "idea");
	assert_eq!(node_id.0, "type:idea:field:idea_only");

	let excluded_path = ["sample_group", "start", "idea_only"];
	assert_eq!(
		engine.bind_chain(path, &excluded_path),
		graph.bind_chain(path, &excluded_path)
	);
	assert!(
		!matches!(
			engine.bind_chain(path, &excluded_path),
			SchemaBinding::Bound { ref type_id, .. } if type_id.as_str() == "idea"
		),
		"compiled engine must not bind excluded root key to idea type"
	);
}

#[test]
fn compiled_engine_matches_path_file_root_matching() {
	let schema = r#"
	types = {
		type[map_fallback] = {
			path = "game/map"
		}
		type[area] = {
			path = "game/map"
			path_file = "area.txt"
		}
		type[region] = {
			path = "game/map"
			path_file = "region.txt"
		}
	}

	map_fallback = {
		fallback_only = bool
	}

	area = {
		area_only = bool
	}

	region = {
		region_only = bool
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let engine = RuleEngine::from_graph(&graph);
	let area_path = Path::new("map/area.txt");
	let area_ast_path = ["sample_area", "area_only"];

	assert_eq!(
		engine.bind_chain(area_path, &area_ast_path),
		graph.bind_chain(area_path, &area_ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } = engine.bind_chain(area_path, &area_ast_path)
	else {
		panic!("expected compiled area binding");
	};
	assert_eq!(type_id.as_str(), "area");
	assert_eq!(node_id.0, "type:area:field:area_only");

	let region_path = Path::new("map/region.txt");
	let region_ast_path = ["sample_region", "region_only"];
	assert_eq!(
		engine.bind_chain(region_path, &region_ast_path),
		graph.bind_chain(region_path, &region_ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } =
		engine.bind_chain(region_path, &region_ast_path)
	else {
		panic!("expected compiled region binding");
	};
	assert_eq!(type_id.as_str(), "region");
	assert_eq!(node_id.0, "type:region:field:region_only");

	let fallback_path = Path::new("map/other.txt");
	let fallback_ast_path = ["sample_map", "fallback_only"];
	assert_eq!(
		engine.bind_chain(fallback_path, &fallback_ast_path),
		graph.bind_chain(fallback_path, &fallback_ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } =
		engine.bind_chain(fallback_path, &fallback_ast_path)
	else {
		panic!("expected compiled fallback binding");
	};
	assert_eq!(type_id.as_str(), "map_fallback");
	assert_eq!(node_id.0, "type:map_fallback:field:fallback_only");
}

#[test]
fn compiled_engine_matches_ordered_skip_root_key_chain() {
	let schema = r#"
	types = {
		type[game_age] = {
			path = "game/common/ages"
		}
		type[game_age_ability] = {
			path = "game/common/ages"
			skip_root_key = { any abilities }
		}
	}

	game_age = {
		start = int
	}

	game_age_ability = {
		power = bool
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let engine = RuleEngine::from_graph(&graph);
	let path = Path::new("common/ages/example.txt");
	let ast_path = ["age_of_discovery", "abilities", "free_war_taxes", "power"];

	assert_eq!(
		engine.bind_chain(path, &ast_path),
		graph.bind_chain(path, &ast_path)
	);
	let SchemaBinding::Bound { type_id, node_id } = engine.bind_chain(path, &ast_path) else {
		panic!("expected compiled game_age_ability binding");
	};
	assert_eq!(type_id.as_str(), "game_age_ability");
	assert_eq!(node_id.0, "type:game_age_ability:field:power");

	let missing_fixed_wrapper = ["age_of_discovery", "free_war_taxes", "power"];
	assert_eq!(
		engine.bind_chain(path, &missing_fixed_wrapper),
		graph.bind_chain(path, &missing_fixed_wrapper)
	);
	assert!(
		!matches!(
			engine.bind_chain(path, &missing_fixed_wrapper),
			SchemaBinding::Bound { ref type_id, .. } if type_id.as_str() == "game_age_ability"
		),
		"ability type must require the ordered skip_root_key chain"
	);
}

#[test]
fn compiled_engine_matches_scope_hierarchy_metadata() {
	let graph = CwtSchemaGraph::from_directory(&schema_pack_fixture_dir())
		.expect("load schema-pack fixture graph");
	let engine = RuleEngine::from_graph(&graph);
	assert_eq!(engine.pack().scope_definitions.len(), 2);

	assert!(engine.scope_matches("country", "country"));
	assert!(engine.scope_matches("province", "province"));
	assert!(engine.scope_matches("country", "province"));
	assert!(!engine.scope_matches("province", "country"));
	assert!(engine.scope_matches("unknown", "unknown"));
	assert!(!engine.scope_matches("country", "unknown"));
	assert!(!engine.scope_matches("unknown", "country"));
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
fn compiled_binary_pack_roundtrips_severity_attributes() {
	let schema = r#"
	types = {
		type[event] = {
			path = "game/events"
		}
	}

	event = {
		## severity = warning
		gentle_bool = bool
		trigger = {
			alias_name[trigger] = alias_match_left[trigger]
		}
	}

	## scope = country
	## severity = info
	alias[trigger:gentle_trigger] = bool
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let pack = CompiledRulePack::from_graph(&graph);
	let decoded = CompiledRulePack::from_bytes(&pack.to_bytes().expect("encode compiled pack"))
		.expect("decode compiled pack");
	let engine = RuleEngine::new(decoded);
	let event = engine
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");
	let field = engine
		.bind_field(RuleContext::RootType(event), "gentle_bool")
		.expect("bind severity field");
	assert_eq!(field.attributes.severity, Some(CompiledSeverity::Warning));

	let trigger = engine
		.bind_field(RuleContext::RootType(event), "trigger")
		.expect("bind trigger field");
	let alias_match = engine
		.bind_field_match(RuleContext::RuleField(trigger), "gentle_trigger")
		.expect("bind severity alias");
	assert_eq!(
		alias_match.alias().map(|alias| alias.attributes.severity),
		Some(Some(CompiledSeverity::Info))
	);
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

fn schema_pack_fixture_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures")
		.join("schema-pack")
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
