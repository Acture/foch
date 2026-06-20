use foch_language::cwt::{
	CwtAlias, CwtComplexEnum, CwtEnum, CwtLink, CwtOption, CwtRange, CwtRule, CwtRuleBody,
	CwtScope, CwtSingleAlias, CwtSubtype, CwtType, CwtValueSet, CwtValueType, load_cwt_schema,
	parse_bracket_key,
};

#[test]
fn parses_nested_bracket_keys_at_outermost_close() {
	assert_eq!(
		parse_bracket_key("alias[effect:enum[country_tags]]"),
		Some(("alias", "effect:enum[country_tags]"))
	);
}

#[test]
fn rejects_non_bracket_keys() {
	assert_eq!(parse_bracket_key("alias"), None);
}

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
fn loads_value_sets_from_values_block() {
	let schema = load_cwt_schema(
		r#"
values = {
	value[variable] = {
		num_days
		threat
	}
	value_set[cooldown_token] = {
		parliament_debate
	}
}
"#,
	);

	assert_eq!(
		schema.value_sets,
		vec![
			CwtValueSet {
				name: "variable".to_string(),
				values: vec!["num_days".to_string(), "threat".to_string()],
			},
			CwtValueSet {
				name: "cooldown_token".to_string(),
				values: vec!["parliament_debate".to_string()],
			},
		]
	);
}

#[test]
fn loads_top_level_value_set_definition() {
	let schema = load_cwt_schema(
		r#"
value_set[scripted_token] = {
	first
	second
}
"#,
	);

	assert_eq!(
		schema.value_sets,
		vec![CwtValueSet {
			name: "scripted_token".to_string(),
			values: vec!["first".to_string(), "second".to_string()],
		}]
	);
}

#[test]
fn loads_top_level_single_alias_block() {
	let schema = load_cwt_schema(
		r#"
single_alias[clause] = {
	key = scalar
	## cardinality = 0..inf
	alias_name[trigger] = alias_match_left[trigger]
}
"#,
	);

	assert_eq!(
		schema.single_aliases,
		vec![CwtSingleAlias {
			name: "clause".to_string(),
			body: CwtRuleBody::Block(vec![
				CwtRule {
					key: "key".to_string(),
					body: CwtRuleBody::Leaf(CwtValueType::Scalar),
					cardinality: None,
					options: Vec::new(),
				},
				CwtRule {
					key: "alias_name[trigger]".to_string(),
					body: CwtRuleBody::Leaf(CwtValueType::AliasMatchLeft("trigger".to_string())),
					cardinality: Some("0..inf".to_string()),
					options: vec![CwtOption {
						key: "cardinality".to_string(),
						value: "0..inf".to_string(),
					}],
				},
			]),
		}]
	);
}

#[test]
fn loads_leaf_single_alias_body() {
	let schema = load_cwt_schema(
		r#"
single_alias[array] = value[array]
"#,
	);

	assert_eq!(
		schema.single_aliases,
		vec![CwtSingleAlias {
			name: "array".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Value("array".to_string())),
		}]
	);
}

#[test]
fn loads_complex_enums_from_enums_block() {
	let schema = load_cwt_schema(
		r#"
enums = {
	complex_enum[building_tag] = {
		path = "game/common/buildings"
		name = {
			name = scalar
			enum_name
		}
		start_from_root = yes
	}
}
"#,
	);

	assert_eq!(
		schema.complex_enums,
		vec![CwtComplexEnum {
			name: "building_tag".to_string(),
			path: Some("game/common/buildings".to_string()),
			start_from_root: true,
			name_rules: vec![
				CwtRule {
					key: "name".to_string(),
					body: CwtRuleBody::Leaf(CwtValueType::Scalar),
					cardinality: None,
					options: Vec::new(),
				},
				CwtRule {
					key: "enum_name".to_string(),
					body: CwtRuleBody::Leaf(CwtValueType::Literal("enum_name".to_string())),
					cardinality: None,
					options: Vec::new(),
				},
			],
		}]
	);
}

#[test]
fn loads_enum_and_complex_enum_from_same_enums_block() {
	let schema = load_cwt_schema(
		r#"
enums = {
	enum[power_categories] = { ADM DIP MIL }
	complex_enum[graphical_cultures] = {
		path = "game/common"
		name = {
			enum_name
		}
		start_from_root = yes
	}
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
	assert_eq!(
		schema.complex_enums,
		vec![CwtComplexEnum {
			name: "graphical_cultures".to_string(),
			path: Some("game/common".to_string()),
			start_from_root: true,
			name_rules: vec![CwtRule {
				key: "enum_name".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Literal("enum_name".to_string())),
				cardinality: None,
				options: Vec::new(),
			}],
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
			scope: vec!["country".to_string()],
			options: vec![CwtOption {
				key: "scope".to_string(),
				value: "country".to_string(),
			}],
		}]
	);
}

#[test]
fn captures_alias_options_and_scope_from_comments_and_body() {
	let schema = load_cwt_schema(
		r#"
## scope = { country province }
## push_scope = country
alias[effect:enum[country_tags]] = {
	scope = state
	push_scope = province
}
"#,
	);

	assert_eq!(
		schema.aliases,
		vec![CwtAlias {
			category: "effect".to_string(),
			name: "enum[country_tags]".to_string(),
			scope: vec![
				"country".to_string(),
				"province".to_string(),
				"state".to_string()
			],
			options: vec![
				CwtOption {
					key: "scope".to_string(),
					value: "country province".to_string(),
				},
				CwtOption {
					key: "push_scope".to_string(),
					value: "country".to_string(),
				},
				CwtOption {
					key: "scope".to_string(),
					value: "state".to_string(),
				},
				CwtOption {
					key: "push_scope".to_string(),
					value: "province".to_string(),
				}
			],
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
fn loads_links_scope_transitions() {
	let schema = load_cwt_schema(
		r#"
links = {
	owner = {
		input_scopes = { province }
		output_scope = country
	}
	emperor = {
		output_scope = country
	}
	capital = {
		input_scopes = { country province }
		output_scope = province
	}
}
"#,
	);

	assert_eq!(
		schema.links,
		vec![
			CwtLink {
				name: "owner".to_string(),
				input_scopes: vec!["province".to_string()],
				output_scope: Some("country".to_string()),
			},
			CwtLink {
				name: "emperor".to_string(),
				input_scopes: Vec::new(),
				output_scope: Some("country".to_string()),
			},
			CwtLink {
				name: "capital".to_string(),
				input_scopes: vec!["country".to_string(), "province".to_string()],
				output_scope: Some("province".to_string()),
			},
		]
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
fn captures_scalar_and_block_hash_hash_options() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		## scope = { country province }
		picture = <event_picture>
		## replace_scope = { this = country root = country }
		subtype[country_event] = {
		}
	}
}
"#,
	);

	assert_eq!(
		schema.types[0].options,
		vec![CwtOption {
			key: "scope".to_string(),
			value: "country province".to_string(),
		}]
	);
	assert_eq!(
		schema.types[0].subtypes[0].options,
		vec![CwtOption {
			key: "replace_scope".to_string(),
			value: "this = country root = country".to_string(),
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
single_alias[] = { key = scalar }
value_set[] = { seed }
types = {
	type[event] = {
		path = "game/events"
		subtype[] = { type_key_filter = country_event }
	}
	not_a_type = { value }
}
enums = {
	enum[power_categories] = { ADM DIP MIL }
	complex_enum[] = { path = "game/common" name = { enum_name } }
	complex_enum[missing_name_block] = { path = "game/common" }
}
"#,
	);

	assert_eq!(schema.types.len(), 1);
	assert_eq!(schema.types[0].name, "event");
	assert_eq!(schema.types[0].path.as_deref(), Some("game/events"));
	assert!(schema.types[0].subtypes.is_empty());
	assert_eq!(schema.enums.len(), 1);
	assert!(schema.value_sets.is_empty());
	assert!(schema.single_aliases.is_empty());
	assert!(schema.complex_enums.is_empty());
	assert!(schema.aliases.is_empty());
}

#[test]
fn classifies_cwt_value_type_tokens() {
	assert_eq!(CwtValueType::from_token("scalar"), CwtValueType::Scalar);
	assert_eq!(CwtValueType::from_token("bool"), CwtValueType::Bool);
	assert_eq!(CwtValueType::from_token("int"), CwtValueType::Int(None));
	assert_eq!(
		CwtValueType::from_token("int[0..100]"),
		CwtValueType::Int(Some(CwtRange {
			min: "0".to_string(),
			max: "100".to_string(),
		}))
	);
	assert_eq!(
		CwtValueType::from_token("float[0.0..1.0]"),
		CwtValueType::Float(Some(CwtRange {
			min: "0.0".to_string(),
			max: "1.0".to_string(),
		}))
	);
	assert_eq!(
		CwtValueType::from_token("percentage_field"),
		CwtValueType::Percentage
	);
	assert_eq!(
		CwtValueType::from_token("date_field"),
		CwtValueType::DateField
	);
	assert_eq!(
		CwtValueType::from_token("localisation"),
		CwtValueType::Localisation
	);
	assert_eq!(CwtValueType::from_token("filepath"), CwtValueType::Filepath);
	assert_eq!(CwtValueType::from_token("colour"), CwtValueType::Colour);
	assert_eq!(
		CwtValueType::from_token("<event_target>"),
		CwtValueType::TypeRef("event_target".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("enum[power_categories]"),
		CwtValueType::Enum("power_categories".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("value[event_target]"),
		CwtValueType::Value("event_target".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("value_set[cooldown_token]"),
		CwtValueType::ValueSet("cooldown_token".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("scope[country]"),
		CwtValueType::Scope("country".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("alias_name[trigger]"),
		CwtValueType::AliasName("trigger".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("alias_match_left[trigger]"),
		CwtValueType::AliasMatchLeft("trigger".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("single_alias_right[effect]"),
		CwtValueType::SingleAliasRight("effect".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("yes"),
		CwtValueType::Literal("yes".to_string())
	);
	assert_eq!(
		CwtValueType::from_token("enum[]"),
		CwtValueType::Unknown("enum[]".to_string())
	);
}

#[test]
fn attaches_top_level_rule_body_to_matching_type() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		path = "game/events"
		name_field = "id"
	}
}

event = {
	id = scalar
	title = localisation
	goto = <event_target>
	picture = enum[dlc_event_pictures]
	factor = int[0..100]
	## cardinality = 0..1
	hidden = bool
	trigger = {
		alias_name[trigger] = alias_match_left[trigger]
		chance = float[0.0..1.0]
	}
}
"#,
	);

	assert!(schema.rule_bodies.is_empty());
	assert_eq!(schema.types.len(), 1);
	assert_eq!(
		schema.types[0].rules,
		vec![
			CwtRule {
				key: "id".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Scalar),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "title".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Localisation),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "goto".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::TypeRef("event_target".to_string())),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "picture".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Enum("dlc_event_pictures".to_string())),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "factor".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Int(Some(CwtRange {
					min: "0".to_string(),
					max: "100".to_string(),
				}))),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "hidden".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Bool),
				cardinality: Some("0..1".to_string()),
				options: vec![CwtOption {
					key: "cardinality".to_string(),
					value: "0..1".to_string(),
				}],
			},
			CwtRule {
				key: "trigger".to_string(),
				body: CwtRuleBody::Block(vec![
					CwtRule {
						key: "alias_name[trigger]".to_string(),
						body: CwtRuleBody::Leaf(CwtValueType::AliasMatchLeft(
							"trigger".to_string()
						)),
						cardinality: None,
						options: Vec::new(),
					},
					CwtRule {
						key: "chance".to_string(),
						body: CwtRuleBody::Leaf(CwtValueType::Float(Some(CwtRange {
							min: "0.0".to_string(),
							max: "1.0".to_string(),
						}))),
						cardinality: None,
						options: Vec::new(),
					},
				]),
				cardinality: None,
				options: Vec::new(),
			},
		]
	);
}

#[test]
fn keeps_rule_body_without_matching_type_at_schema_level() {
	let schema = load_cwt_schema(
		r#"
types = {
	type[event] = {
		path = "game/events"
	}
}

decision = {
	major = yes
	color = {
		## cardinality = 3..3
		int[0..255]
	}
}
"#,
	);

	assert!(schema.types[0].rules.is_empty());
	assert_eq!(schema.rule_bodies.len(), 1);
	assert_eq!(schema.rule_bodies[0].key, "decision");
	assert_eq!(
		schema.rule_bodies[0].rules,
		vec![
			CwtRule {
				key: "major".to_string(),
				body: CwtRuleBody::Leaf(CwtValueType::Literal("yes".to_string())),
				cardinality: None,
				options: Vec::new(),
			},
			CwtRule {
				key: "color".to_string(),
				body: CwtRuleBody::Block(vec![CwtRule {
					key: "int[0..255]".to_string(),
					body: CwtRuleBody::Leaf(CwtValueType::Int(Some(CwtRange {
						min: "0".to_string(),
						max: "255".to_string(),
					}))),
					cardinality: Some("3..3".to_string()),
					options: vec![CwtOption {
						key: "cardinality".to_string(),
						value: "3..3".to_string(),
					}],
				}]),
				cardinality: None,
				options: Vec::new(),
			},
		]
	);
}
