use foch_language::cwt::{
	CwtAlias, CwtEnum, CwtOption, CwtScope, CwtSubtype, CwtType, load_cwt_schema,
};

#[test]
fn loads_type_metadata_and_subtypes() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		path = "game/events"
		name_field = "id"
		subtype[country_event] = {
			type_key_filter = country_event
		}
	}
}
"#,
	);

	assert_eq!(
		schema.types,
		vec![CwtType {
			name: "event".to_string(),
			path: Some("game/events".to_string()),
			name_field: Some("id".to_string()),
			subtypes: vec![CwtSubtype {
				name: "country_event".to_string(),
				type_key_filter: vec!["country_event".to_string()],
				options: Vec::new(),
			}],
			..Default::default()
		}]
	);
}

#[test]
fn loads_file_identity_flags_and_skip_root_keys() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[government_rank] = {
		name_from_file = yes
		type_per_file = yes
		skip_root_key = governments
	}
	type[mission] = {
		skip_root_key = { missions any }
	}
}
"#,
	);

	assert_eq!(schema.types[0].name, "government_rank");
	assert!(schema.types[0].name_from_file);
	assert!(schema.types[0].type_per_file);
	assert_eq!(schema.types[0].skip_root_key, vec!["governments"]);
	assert_eq!(schema.types[1].skip_root_key, vec!["missions", "any"]);
}

#[test]
fn loads_enums() {
	let schema = load_cwt_schema(
		r#"
enums = {
	enum[power_categories] = { ADM DIP MIL }
}
"#,
	);

	assert_eq!(
		schema.enums,
		vec![CwtEnum {
			name: "power_categories".to_string(),
			values: vec!["ADM".to_string(), "DIP".to_string(), "MIL".to_string()],
		}]
	);
}

#[test]
fn loads_top_level_aliases() {
	let schema = load_cwt_schema(
		r#"
alias[trigger:add_happiness] = {
	scope = country
}
"#,
	);

	assert_eq!(
		schema.aliases,
		vec![CwtAlias {
			category: "trigger".to_string(),
			name: "add_happiness".to_string(),
		}]
	);
}

#[test]
fn loads_scopes_and_aliases() {
	let schema = load_cwt_schema(
		r#"
scopes = {
	Country = {
		aliases = { country c }
	}
}
"#,
	);

	assert_eq!(
		schema.scopes,
		vec![CwtScope {
			name: "Country".to_string(),
			aliases: vec!["country".to_string(), "c".to_string()],
		}]
	);
}

#[test]
fn attaches_hash_hash_options_to_next_assignment() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		## cardinality = 0..1
		picture = <event_picture>
		## severity = warning
		subtype[country_event] = {
			type_key_filter = country_event
		}
	}
}
"#,
	);

	assert_eq!(
		schema.types[0].options,
		vec![CwtOption {
			key: "cardinality".to_string(),
			value: "0..1".to_string(),
		}]
	);
	assert_eq!(
		schema.types[0].subtypes[0].options,
		vec![CwtOption {
			key: "severity".to_string(),
			value: "warning".to_string(),
		}]
	);
}

#[test]
fn promotes_type_key_filter_option_to_subtype_filter() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		## type_key_filter = country_event
		subtype[country_event] = {
		}
	}
}
"#,
	);

	assert_eq!(
		schema.types[0].subtypes[0].type_key_filter,
		vec!["country_event"]
	);
	assert_eq!(
		schema.types[0].subtypes[0].options,
		vec![CwtOption {
			key: "type_key_filter".to_string(),
			value: "country_event".to_string(),
		}]
	);
}

#[test]
fn ignores_unknown_and_malformed_constructs_without_losing_known_schema() {
	let schema = load_cwt_schema(
		r#"
unknown = { definitely = ignored }
alias[missing_separator] = yes
types = {
	type[event] = {
		path = "game/events"
		subtype[] = { type_key_filter = country_event }
	}
	not_a_type = { value }
}
enums = {
	enum[power_categories] = { ADM DIP MIL }
}
"#,
	);

	assert_eq!(schema.types.len(), 1);
	assert_eq!(schema.types[0].name, "event");
	assert_eq!(schema.types[0].path.as_deref(), Some("game/events"));
	assert!(schema.types[0].subtypes.is_empty());
	assert_eq!(schema.enums.len(), 1);
	assert!(schema.aliases.is_empty());
}
