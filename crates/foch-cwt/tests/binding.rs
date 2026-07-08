use std::path::{Path, PathBuf};

use foch_cwt::{
	AliasCategory, BindContext, CwtRuleCondition, CwtRuleValue, CwtSchemaGraph, CwtSeverity,
	CwtTypeKeyFilter, SchemaBinding,
};
use foch_syntax::ParadoxTree;

#[test]
fn bind_chain_binds_root_and_subtype() {
	let graph = load_binding_graph();

	let root_binding = graph.bind_chain(Path::new("events/example.txt"), &[]);
	let SchemaBinding::Bound { type_id, node_id } = root_binding else {
		panic!("expected bound root, got {root_binding:?}");
	};
	assert_eq!(type_id.as_str(), "event");
	assert_eq!(node_id.0, "type:event:root");

	let subtype_binding = graph.bind_chain(Path::new("events/example.txt"), &["country_event"]);
	let SchemaBinding::Bound { type_id, node_id } = subtype_binding else {
		panic!("expected bound subtype, got {subtype_binding:?}");
	};
	assert_eq!(type_id.as_str(), "event");
	assert_eq!(node_id.0, "type:event:subtype:country_event");

	let event = graph
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");
	assert_eq!(event.subtypes.len(), 1);
	assert_eq!(
		event.subtypes[0].type_key_filter,
		Some(CwtTypeKeyFilter::Exact(vec!["country_event".to_string()]))
	);
	assert_eq!(
		event.subtypes[0].attributes.push_scope.as_deref(),
		Some("country")
	);
}

#[test]
fn bind_field_projects_attributes_and_alias_expansion() {
	let graph = load_binding_graph();
	let event = graph
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");

	let iterator = graph
		.bind_field(BindContext::RootType(event), "every_country")
		.expect("bind every_country field");
	assert_eq!(
		iterator.attributes.scope,
		vec!["country".to_string(), "province".to_string()]
	);
	assert_eq!(iterator.attributes.push_scope.as_deref(), Some("country"));
	assert_eq!(
		iterator.attributes.description.as_deref(),
		Some("Scopes to all countries.")
	);
	assert_eq!(
		iterator.attributes.raw,
		vec![("required".to_string(), String::new())]
	);

	let trigger = graph
		.bind_field(BindContext::RootType(event), "trigger")
		.expect("bind trigger field");
	let alias_field = graph
		.bind_field(BindContext::RuleField(trigger), "is_year")
		.expect("expand trigger alias field");
	assert_eq!(alias_field.key, "alias_name[trigger]");

	let alias = graph
		.aliases
		.get(&(AliasCategory::Trigger, "is_year".to_string()))
		.expect("lookup trigger alias");
	assert_eq!(alias.value, CwtRuleValue::Scalar("int".to_string()));
	assert_eq!(alias.attributes.scope, vec!["country".to_string()]);
	assert_eq!(alias.attributes.push_scope.as_deref(), Some("country"));
}

#[test]
fn bind_field_projects_severity_attributes() {
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
	let event = graph
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");
	let field = graph
		.bind_field(BindContext::RootType(event), "gentle_bool")
		.expect("bind severity field");
	assert_eq!(field.attributes.severity, Some(CwtSeverity::Warning));

	let alias = graph
		.aliases
		.get(&(AliasCategory::Trigger, "gentle_trigger".to_string()))
		.expect("lookup severity alias");
	assert_eq!(alias.attributes.severity, Some(CwtSeverity::Info));
}

#[test]
fn rule_subtype_blocks_project_conditional_fields() {
	let schema = r#"
	types = {
		type[event] = {
			path = "game/events"
			subtype[hidden] = {
				hidden = yes
			}
		}
	}

	event = {
		subtype[hidden] = {
			hidden_only = bool
		}
		subtype[!hidden] = {
			visible_only = bool
		}
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let event = graph
		.bind_root(Path::new("events/example.txt"))
		.expect("bind event root");
	assert!(
		event
			.rules
			.iter()
			.all(|field| !field.key.starts_with("subtype["))
	);
	let hidden_only = event
		.rules
		.iter()
		.find(|field| field.key == "hidden_only")
		.expect("hidden-only conditional field");
	assert_eq!(
		hidden_only.conditions,
		vec![CwtRuleCondition::SubtypeActive("hidden".to_string())]
	);
	let visible_only = event
		.rules
		.iter()
		.find(|field| field.key == "visible_only")
		.expect("visible-only conditional field");
	assert_eq!(
		visible_only.conditions,
		vec![CwtRuleCondition::SubtypeInactive("hidden".to_string())]
	);
}

#[test]
fn bind_chain_binds_aliases_and_reports_unbound_keys() {
	let graph = load_binding_graph();

	let alias_binding = graph.bind_chain(
		Path::new("events/example.txt"),
		&["country_event", "trigger", "is_year"],
	);
	let SchemaBinding::Bound { type_id, node_id } = alias_binding else {
		panic!("expected bound alias, got {alias_binding:?}");
	};
	assert_eq!(type_id.as_str(), "event");
	assert_eq!(
		node_id.0,
		"type:event:subtype:country_event/field:trigger/alias:trigger:is_year"
	);

	let unbound = graph.bind_chain(
		Path::new("events/example.txt"),
		&["country_event", "nonexistent_key"],
	);
	let SchemaBinding::Unbound { reason } = unbound else {
		panic!("expected unbound binding, got {unbound:?}");
	};
	assert!(reason.contains("nonexistent_key"));
	assert!(reason.contains("country_event"));
}

#[test]
fn bind_chain_binds_dynamic_mission_fields() {
	let graph = load_binding_graph();

	let binding = graph.bind_chain(
		Path::new("missions/example.txt"),
		&["my_mission", "provinces_to_highlight"],
	);
	let SchemaBinding::Bound { type_id, node_id } = binding else {
		panic!("expected bound mission field, got {binding:?}");
	};
	assert_eq!(type_id.as_str(), "mission");
	assert_eq!(node_id.0, "type:mission:field:provinces_to_highlight");

	let mission = graph
		.bind_root(Path::new("missions/example.txt"))
		.expect("bind mission root");
	let field = graph
		.bind_field(BindContext::RootType(mission), "provinces_to_highlight")
		.expect("bind provinces_to_highlight field");
	assert_eq!(field.attributes.cardinality, Some((0, Some(1))));
	assert_eq!(
		field
			.attributes
			.replace_scope
			.get("this")
			.map(String::as_str),
		Some("province")
	);
	assert_eq!(
		field
			.attributes
			.replace_scope
			.get("root")
			.map(String::as_str),
		Some("country")
	);
}

#[test]
fn bind_chain_binds_angle_bracket_dynamic_fields() {
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

	let binding = graph.bind_chain(
		Path::new("missions/example.txt"),
		&["demo_mission", "mission_tree", "conquest", "trigger"],
	);
	let SchemaBinding::Bound { type_id, node_id } = binding else {
		panic!("expected bound dynamic field, got {binding:?}");
	};
	assert_eq!(type_id.as_str(), "mission");
	assert_eq!(
		node_id.0,
		"type:mission:field:mission_tree/field:<mission_stage>/field:trigger"
	);

	let context = graph
		.bind_context(
			Path::new("missions/example.txt"),
			&["demo_mission", "mission_tree"],
		)
		.expect("bind mission tree context");
	let field_match = graph
		.bind_field_match(context, "conquest")
		.expect("bind dynamic mission stage");
	assert_eq!(field_match.field().key, "<mission_stage>");
}

#[test]
fn bind_field_accepts_plural_replace_scopes_option() {
	let schema = r#"
	types = {
		type[incident] = {
			path = "game/common/incidents"
		}
	}

	incident = {
		## replace_scopes = { this = country root = country }
		immediate = {
			alias_name[effect] = alias_match_left[effect]
		}
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let incident = graph
		.bind_root(Path::new("common/incidents/example.txt"))
		.expect("bind incident root");
	let field = graph
		.bind_field(BindContext::RootType(incident), "immediate")
		.expect("bind immediate field");
	assert_eq!(
		field
			.attributes
			.replace_scope
			.get("this")
			.map(String::as_str),
		Some("country")
	);
	assert_eq!(
		field
			.attributes
			.replace_scope
			.get("root")
			.map(String::as_str),
		Some("country")
	);
}

#[test]
fn bind_chain_uses_root_type_key_filter_exclusions() {
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
	let idea = graph
		.types
		.values()
		.find(|definition| definition.name.as_str() == "idea")
		.expect("idea type");
	assert_eq!(
		idea.type_key_filter,
		Some(CwtTypeKeyFilter::Exclude(vec![
			"start".to_string(),
			"trigger".to_string(),
			"bonus".to_string(),
			"ai_will_do".to_string(),
		]))
	);
	assert_eq!(idea.skip_root_keys, vec!["any".to_string()]);

	let binding = graph.bind_chain(
		Path::new("common/ideas/example.txt"),
		&["sample_group", "sample_idea", "idea_only"],
	);
	let SchemaBinding::Bound { type_id, node_id } = binding else {
		panic!("expected idea root binding, got {binding:?}");
	};
	assert_eq!(type_id.as_str(), "idea");
	assert_eq!(node_id.0, "type:idea:field:idea_only");

	let excluded = graph.bind_chain(
		Path::new("common/ideas/example.txt"),
		&["sample_group", "start", "idea_only"],
	);
	assert!(
		!matches!(
			excluded,
			SchemaBinding::Bound { ref type_id, .. } if type_id.as_str() == "idea"
		),
		"excluded key must not bind to idea type: {excluded:?}"
	);
}

#[test]
fn bind_chain_uses_path_file_for_root_matching() {
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
	let area = graph
		.types
		.values()
		.find(|definition| definition.name.as_str() == "area")
		.expect("area type");
	assert_eq!(area.path_file.as_deref(), Some("area.txt"));

	let area_binding = graph.bind_chain(Path::new("map/area.txt"), &["sample_area", "area_only"]);
	let SchemaBinding::Bound { type_id, node_id } = area_binding else {
		panic!("expected area binding, got {area_binding:?}");
	};
	assert_eq!(type_id.as_str(), "area");
	assert_eq!(node_id.0, "type:area:field:area_only");

	let region_binding = graph.bind_chain(
		Path::new("map/region.txt"),
		&["sample_region", "region_only"],
	);
	let SchemaBinding::Bound { type_id, node_id } = region_binding else {
		panic!("expected region binding, got {region_binding:?}");
	};
	assert_eq!(type_id.as_str(), "region");
	assert_eq!(node_id.0, "type:region:field:region_only");

	let fallback_binding =
		graph.bind_chain(Path::new("map/other.txt"), &["sample_map", "fallback_only"]);
	let SchemaBinding::Bound { type_id, node_id } = fallback_binding else {
		panic!("expected fallback map binding, got {fallback_binding:?}");
	};
	assert_eq!(type_id.as_str(), "map_fallback");
	assert_eq!(node_id.0, "type:map_fallback:field:fallback_only");
}

#[test]
fn bind_chain_uses_ordered_skip_root_key_chain() {
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
	let ability = graph
		.types
		.values()
		.find(|definition| definition.name.as_str() == "game_age_ability")
		.expect("game_age_ability type");
	assert_eq!(
		ability.skip_root_keys,
		vec!["any".to_string(), "abilities".to_string()]
	);

	let binding = graph.bind_chain(
		Path::new("common/ages/example.txt"),
		&["age_of_discovery", "abilities", "free_war_taxes", "power"],
	);
	let SchemaBinding::Bound { type_id, node_id } = binding else {
		panic!("expected nested ability binding, got {binding:?}");
	};
	assert_eq!(type_id.as_str(), "game_age_ability");
	assert_eq!(node_id.0, "type:game_age_ability:field:power");

	let missing_fixed_wrapper = graph.bind_chain(
		Path::new("common/ages/example.txt"),
		&["age_of_discovery", "free_war_taxes", "power"],
	);
	assert!(
		!matches!(
			missing_fixed_wrapper,
			SchemaBinding::Bound { ref type_id, .. } if type_id.as_str() == "game_age_ability"
		),
		"ability type must require the ordered skip_root_key chain: {missing_fixed_wrapper:?}"
	);
}

#[test]
fn bind_context_tracks_subtypes_and_root_instances() {
	let graph = load_binding_graph();

	let event_context = graph
		.bind_context(Path::new("events/example.txt"), &["country_event"])
		.expect("bind event subtype context");
	let BindContext::Subtype(event, subtype) = event_context else {
		panic!("expected subtype context, got {event_context:?}");
	};
	assert_eq!(event.name.as_str(), "event");
	assert_eq!(
		subtype.type_key_filter,
		Some(CwtTypeKeyFilter::Exact(vec!["country_event".to_string()]))
	);

	let mission_context = graph
		.bind_context(Path::new("missions/example.txt"), &["my_mission"])
		.expect("bind mission root instance context");
	let BindContext::RootType(mission) = mission_context else {
		panic!("expected root context, got {mission_context:?}");
	};
	assert_eq!(mission.name.as_str(), "mission");
}

#[test]
fn bind_field_match_returns_alias_metadata() {
	let graph = load_binding_graph();
	let context = graph
		.bind_context(
			Path::new("events/example.txt"),
			&["country_event", "trigger"],
		)
		.expect("bind trigger context");
	let alias_match = graph
		.bind_field_match(context, "is_year")
		.expect("bind trigger alias");
	assert_eq!(alias_match.field().key, "alias_name[trigger]");
	let alias = alias_match.alias().expect("alias metadata");
	assert_eq!(alias.name, "is_year");
}

#[test]
fn bind_fields_returns_all_direct_matches() {
	let schema = r#"
	types = {
		type[estate_privilege] = {
			path = "game/common/estate_privileges"
		}
	}

	estate_privilege = {
		can_revoke = bool
		can_revoke = {
			alias_name[trigger] = alias_match_left[trigger]
		}
	}
	"#;
	let tree = ParadoxTree::parse(schema.as_bytes()).expect("parse inline schema");
	let graph = CwtSchemaGraph::from_paradox_tree(&tree);
	let privilege = graph
		.bind_root(Path::new("common/estate_privileges/example.txt"))
		.expect("bind privilege root");
	let matches = graph.bind_fields(BindContext::RootType(privilege), "can_revoke");
	assert_eq!(matches.len(), 2);
	assert!(matches.iter().any(|field| matches!(
		&field.value,
		CwtRuleValue::Scalar(value) if value == "bool"
	)));
	assert!(
		matches
			.iter()
			.any(|field| matches!(&field.value, CwtRuleValue::Block(_)))
	);
}

fn load_binding_graph() -> CwtSchemaGraph {
	CwtSchemaGraph::from_directory(&binding_fixture_dir()).expect("load binding fixture graph")
}

fn binding_fixture_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures")
		.join("binding")
}
