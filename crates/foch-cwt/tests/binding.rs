use std::path::{Path, PathBuf};

use foch_cwt::{AliasCategory, BindContext, CwtRuleValue, CwtSchemaGraph, SchemaBinding};
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
		event.subtypes[0].type_key_filter.as_deref(),
		Some("country_event")
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
fn bind_context_tracks_subtypes_and_root_instances() {
	let graph = load_binding_graph();

	let event_context = graph
		.bind_context(Path::new("events/example.txt"), &["country_event"])
		.expect("bind event subtype context");
	let BindContext::Subtype(event, subtype) = event_context else {
		panic!("expected subtype context, got {event_context:?}");
	};
	assert_eq!(event.name.as_str(), "event");
	assert_eq!(subtype.type_key_filter.as_deref(), Some("country_event"));

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
