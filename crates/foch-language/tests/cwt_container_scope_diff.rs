use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use foch_core::model::ScopeKind;
use foch_cwt::{BindContext, CwtRuleField, CwtRuleValue, CwtSchemaGraph, CwtTypeDef};
use foch_language::analyzer::content_family::CwtType;
use foch_language::analyzer::parser::{AstStatement, AstValue, parse_clausewitz_file};
use foch_language::analyzer::semantic_index::{
	classify_script_file, cwt_file_kind_container_scope_kind,
};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug)]
struct ContainerScopeCase {
	file_kind: &'static str,
	key: &'static str,
	legacy: ScopeKind,
}

#[derive(Debug)]
struct ContainerScopeMismatch {
	file_kind: String,
	key: String,
	hand: ScopeKind,
	cwt: Option<ScopeKind>,
	cwt_fields: Vec<String>,
}

#[derive(Debug)]
struct VanillaContainerScopeMismatch {
	file_kind: String,
	key: String,
	legacy: Option<ScopeKind>,
	cwt: Option<ScopeKind>,
	path: PathBuf,
	line: usize,
}

#[test]
#[ignore = "run manually as a parity gate before swapping production callers"]
fn container_scope_table_matches_cwt_helper() {
	let graph = schema_graph();
	let cases = legacy_cases();
	let mismatches = cases
		.iter()
		.filter_map(|case| {
			let cwt =
				cwt_file_kind_container_scope_kind(graph, CwtType::new(case.file_kind), case.key);
			(Some(case.legacy) != cwt).then(|| ContainerScopeMismatch {
				file_kind: case.file_kind.to_string(),
				key: case.key.to_string(),
				hand: case.legacy,
				cwt,
				cwt_fields: describe_cwt_fields(graph, case.file_kind, case.key),
			})
		})
		.collect::<Vec<_>>();
	println!(
		"container scope diff: matched={} mismatched={} total={}",
		cases.len().saturating_sub(mismatches.len()),
		mismatches.len(),
		cases.len()
	);
	assert!(
		mismatches.is_empty(),
		"container scope mismatches:\n{}",
		format_mismatches(&mismatches)
	);
}

#[test]
#[ignore = "requires a local EU4 install and is run manually as an acceptance gate"]
fn vanilla_corpus_container_scope_matches_cwt_helper() {
	let graph = schema_graph();
	let eu4_root = eu4_root();
	if !eu4_root.is_dir() {
		println!("EU4 install not found at {}", eu4_root.display());
		return;
	}

	let mut files_checked = 0usize;
	let mut keys_checked = 0usize;
	let mut mismatches = Vec::new();
	for entry in WalkDir::new(&eu4_root)
		.into_iter()
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_file())
		.filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("txt"))
	{
		files_checked += 1;
		let relative = entry.path().strip_prefix(&eu4_root).unwrap_or(entry.path());
		let file_kind = classify_script_file(relative);
		let parsed = parse_clausewitz_file(entry.path());
		walk_keys(&parsed.ast.statements, &mut |key, line| {
			keys_checked += 1;
			let legacy = legacy_container_scope_kind(file_kind.as_str(), key);
			let cwt = cwt_file_kind_container_scope_kind(graph, file_kind.clone(), key);
			if legacy != cwt && (legacy.is_some() || cwt.is_some()) {
				mismatches.push(VanillaContainerScopeMismatch {
					file_kind: file_kind.as_str().to_string(),
					key: key.to_string(),
					legacy,
					cwt,
					path: relative.to_path_buf(),
					line,
				});
			}
		});
	}

	println!(
		"vanilla container scope diff: files={} matched={} mismatched={} total={}",
		files_checked,
		keys_checked.saturating_sub(mismatches.len()),
		mismatches.len(),
		keys_checked
	);
	assert!(
		mismatches.is_empty(),
		"vanilla container scope mismatches:\n{}",
		format_vanilla_mismatches(&mismatches)
	);
}

fn schema_graph() -> &'static CwtSchemaGraph {
	static GRAPH: OnceLock<CwtSchemaGraph> = OnceLock::new();
	GRAPH.get_or_init(|| {
		CwtSchemaGraph::from_directory(&vendor_schema_dir()).expect("load vendored cwtools schema")
	})
}

fn vendor_schema_dir() -> PathBuf {
	workspace_root().join("vendor").join("cwtools-eu4-config")
}

fn workspace_root() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("crates dir")
		.parent()
		.expect("workspace root")
		.to_path_buf()
}

fn eu4_root() -> PathBuf {
	std::env::var_os("EU4_ROOT")
		.map(PathBuf::from)
		.or_else(|| {
			std::env::var_os("HOME").map(|home| {
				PathBuf::from(home)
					.join("Library")
					.join("Application Support")
					.join("Steam")
					.join("steamapps")
					.join("common")
					.join("Europa Universalis IV")
			})
		})
		.expect("resolve EU4 root")
}

fn legacy_cases() -> Vec<ContainerScopeCase> {
	let mut cases = Vec::new();
	extend_cases(
		&mut cases,
		"missions",
		ScopeKind::Trigger,
		&[
			"potential_on_load",
			"potential",
			"trigger",
			"provinces_to_highlight",
			"completed_by",
			"ai_weight",
		],
	);
	extend_cases(
		&mut cases,
		"missions",
		ScopeKind::Effect,
		&["effect", "on_completed", "on_cancelled"],
	);
	extend_cases(
		&mut cases,
		"new_diplomatic_actions",
		ScopeKind::Trigger,
		&["is_visible", "is_allowed", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"new_diplomatic_actions",
		ScopeKind::Effect,
		&["on_accept", "on_decline", "add_entry"],
	);
	extend_cases(
		&mut cases,
		"events",
		ScopeKind::Trigger,
		&["mean_time_to_happen"],
	);
	extend_cases(
		&mut cases,
		"ages",
		ScopeKind::Trigger,
		&[
			"can_start",
			"custom_trigger_tooltip",
			"calc_true_if",
			"ai_will_do",
		],
	);
	extend_cases(&mut cases, "ages", ScopeKind::Effect, &["effect"]);
	extend_cases(&mut cases, "buildings", ScopeKind::Trigger, &["ai_will_do"]);
	extend_cases(
		&mut cases,
		"buildings",
		ScopeKind::Effect,
		&[
			"on_built",
			"on_destroyed",
			"on_construction_started",
			"on_construction_canceled",
			"on_obsolete",
		],
	);
	extend_cases(
		&mut cases,
		"institutions",
		ScopeKind::Trigger,
		&[
			"history",
			"can_embrace",
			"potential",
			"custom_trigger_tooltip",
		],
	);
	extend_cases(&mut cases, "institutions", ScopeKind::Effect, &["on_start"]);
	extend_cases(
		&mut cases,
		"institutions",
		ScopeKind::Block,
		&["embracement_speed", "modifier"],
	);
	extend_cases(
		&mut cases,
		"province_triggered_modifiers",
		ScopeKind::Trigger,
		&["potential", "trigger"],
	);
	extend_cases(
		&mut cases,
		"province_triggered_modifiers",
		ScopeKind::Effect,
		&["on_activation", "on_deactivation"],
	);
	extend_cases(
		&mut cases,
		"triggered_modifiers",
		ScopeKind::Trigger,
		&["potential", "trigger"],
	);
	extend_cases(
		&mut cases,
		"triggered_modifiers",
		ScopeKind::Effect,
		&["on_activation", "on_deactivation"],
	);
	extend_cases(
		&mut cases,
		"scripted_triggers",
		ScopeKind::Trigger,
		&["trigger", "limit", "custom_trigger_tooltip"],
	);
	extend_cases(&mut cases, "ideas", ScopeKind::Effect, &["start", "bonus"]);
	extend_cases(
		&mut cases,
		"ideas",
		ScopeKind::Trigger,
		&["trigger", "ai_will_do"],
	);
	// Expanded from the legacy `key.ends_with("_trigger")` arm using the
	// current cwtools great_project schema keys.
	extend_cases(
		&mut cases,
		"great_projects",
		ScopeKind::Trigger,
		&[
			"build_trigger",
			"can_use_modifiers_trigger",
			"can_upgrade_trigger",
			"keep_trigger",
		],
	);
	extend_cases(
		&mut cases,
		"great_projects",
		ScopeKind::Effect,
		&[
			"on_built",
			"on_destroyed",
			"on_upgraded",
			"on_downgraded",
			"on_obtained",
			"on_lost",
		],
	);
	extend_cases(
		&mut cases,
		"government_reforms",
		ScopeKind::Effect,
		&[
			"on_enabled",
			"on_disabled",
			"on_enacted",
			"on_removed",
			"removed_effect",
		],
	);
	extend_cases(
		&mut cases,
		"government_reforms",
		ScopeKind::Trigger,
		&["ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"cb_types",
		ScopeKind::Trigger,
		&[
			"prerequisites_self",
			"prerequisites",
			"can_use",
			"can_take_province",
		],
	);
	extend_cases(
		&mut cases,
		"government_names",
		ScopeKind::Trigger,
		&["trigger"],
	);
	extend_cases(
		&mut cases,
		"customizable_localization",
		ScopeKind::Trigger,
		&["trigger"],
	);
	extend_cases(
		&mut cases,
		"religions",
		ScopeKind::Trigger,
		&["potential", "allow", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"religions",
		ScopeKind::Effect,
		&["effect", "on_convert"],
	);
	extend_cases(
		&mut cases,
		"subject_types",
		ScopeKind::Trigger,
		&[
			"is_potential_overlord",
			"can_fight",
			"can_rival",
			"can_ally",
			"can_marry",
		],
	);
	extend_cases(
		&mut cases,
		"subject_types",
		ScopeKind::Block,
		&["modifier_subject", "modifier_overlord"],
	);
	extend_cases(
		&mut cases,
		"rebel_types",
		ScopeKind::Trigger,
		&[
			"spawn_chance",
			"movement_evaluation",
			"can_negotiate_trigger",
			"can_enforce_trigger",
		],
	);
	extend_cases(
		&mut cases,
		"rebel_types",
		ScopeKind::Effect,
		&["siege_won_effect", "demands_enforced_effect"],
	);
	extend_cases(
		&mut cases,
		"disasters",
		ScopeKind::Trigger,
		&["potential", "can_start", "can_stop", "can_end"],
	);
	extend_cases(
		&mut cases,
		"disasters",
		ScopeKind::Effect,
		&["on_start", "on_end", "on_monthly"],
	);
	extend_cases(
		&mut cases,
		"disasters",
		ScopeKind::Block,
		&["progress", "modifier"],
	);
	extend_cases(
		&mut cases,
		"government_mechanics",
		ScopeKind::Trigger,
		&["available", "trigger"],
	);
	extend_cases(
		&mut cases,
		"government_mechanics",
		ScopeKind::Effect,
		&["on_max_reached", "on_min_reached"],
	);
	extend_cases(
		&mut cases,
		"government_mechanics",
		ScopeKind::Block,
		&[
			"powers",
			"scaled_modifier",
			"reverse_scaled_modifier",
			"modifier",
		],
	);
	extend_cases(
		&mut cases,
		"church_aspects",
		ScopeKind::Trigger,
		&["potential", "trigger", "ai_will_do"],
	);
	extend_cases(&mut cases, "church_aspects", ScopeKind::Effect, &["effect"]);
	extend_cases(
		&mut cases,
		"church_aspects",
		ScopeKind::Block,
		&["modifier"],
	);
	extend_cases(&mut cases, "factions", ScopeKind::Trigger, &["allow"]);
	extend_cases(&mut cases, "factions", ScopeKind::Block, &["modifier"]);
	extend_cases(&mut cases, "hegemons", ScopeKind::Trigger, &["allow"]);
	extend_cases(
		&mut cases,
		"hegemons",
		ScopeKind::Block,
		&["base", "scale", "max"],
	);
	extend_cases(
		&mut cases,
		"personal_deities",
		ScopeKind::Trigger,
		&["potential", "trigger", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"personal_deities",
		ScopeKind::Effect,
		&["effect", "removed_effect"],
	);
	extend_cases(
		&mut cases,
		"fetishist_cults",
		ScopeKind::Trigger,
		&["allow", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"peace_treaties",
		ScopeKind::Trigger,
		&["is_visible", "is_allowed", "ai_weight"],
	);
	extend_cases(&mut cases, "peace_treaties", ScopeKind::Effect, &["effect"]);
	extend_cases(
		&mut cases,
		"peace_treaties",
		ScopeKind::Block,
		&["warscore_cost"],
	);
	extend_cases(
		&mut cases,
		"policies",
		ScopeKind::Trigger,
		&["potential", "allow", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"policies",
		ScopeKind::Effect,
		&["effect", "removed_effect"],
	);
	extend_cases(
		&mut cases,
		"mercenary_companies",
		ScopeKind::Trigger,
		&["trigger"],
	);
	extend_cases(
		&mut cases,
		"mercenary_companies",
		ScopeKind::Block,
		&["modifier"],
	);
	extend_cases(
		&mut cases,
		"estate_agendas",
		ScopeKind::Trigger,
		&[
			"can_select",
			"task_requirements",
			"fail_if",
			"invalid_trigger",
			"provinces_to_highlight",
			"selection_weight",
		],
	);
	extend_cases(
		&mut cases,
		"estate_agendas",
		ScopeKind::Effect,
		&[
			"pre_effect",
			"immediate_effect",
			"on_invalid",
			"task_completed_effect",
		],
	);
	extend_cases(
		&mut cases,
		"estate_privileges",
		ScopeKind::Trigger,
		&["is_valid", "can_select", "can_revoke", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"estate_privileges",
		ScopeKind::Effect,
		&[
			"on_granted",
			"on_revoked",
			"on_invalid",
			"on_granted_province",
			"on_revoked_province",
			"on_invalid_province",
			"on_cooldown_expires",
		],
	);
	extend_cases(
		&mut cases,
		"estate_privileges",
		ScopeKind::Block,
		&[
			"benefits",
			"penalties",
			"modifier_by_land_ownership",
			"mechanics",
			"conditional_modifier",
			"influence_scaled_conditional_modifier",
			"loyalty_scaled_conditional_modifier",
		],
	);
	extend_cases(&mut cases, "estates", ScopeKind::Trigger, &["trigger"]);
	extend_cases(
		&mut cases,
		"estates",
		ScopeKind::Block,
		&[
			"country_modifier_happy",
			"country_modifier_neutral",
			"country_modifier_angry",
			"land_ownership_modifier",
			"province_independence_weight",
			"influence_modifier",
			"loyalty_modifier",
			"influence_from_dev_modifier",
		],
	);
	extend_cases(
		&mut cases,
		"parliament_bribes",
		ScopeKind::Trigger,
		&["trigger", "chance", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"parliament_bribes",
		ScopeKind::Effect,
		&["effect"],
	);
	extend_cases(
		&mut cases,
		"parliament_issues",
		ScopeKind::Trigger,
		&["allow", "chance", "ai_will_do"],
	);
	extend_cases(
		&mut cases,
		"parliament_issues",
		ScopeKind::Effect,
		&["effect", "on_issue_taken"],
	);
	extend_cases(
		&mut cases,
		"parliament_issues",
		ScopeKind::Block,
		&["modifier", "influence_scaled_modifier"],
	);
	extend_cases(
		&mut cases,
		"state_edicts",
		ScopeKind::Trigger,
		&["potential", "allow", "notify_trigger", "ai_will_do"],
	);
	extend_cases(&mut cases, "state_edicts", ScopeKind::Block, &["modifier"]);
	cases
}

fn extend_cases(
	cases: &mut Vec<ContainerScopeCase>,
	file_kind: &'static str,
	legacy: ScopeKind,
	keys: &'static [&'static str],
) {
	cases.extend(keys.iter().copied().map(|key| ContainerScopeCase {
		file_kind,
		key,
		legacy,
	}));
}

fn legacy_case_lookup() -> &'static HashMap<(&'static str, &'static str), ScopeKind> {
	static LOOKUP: OnceLock<HashMap<(&'static str, &'static str), ScopeKind>> = OnceLock::new();
	LOOKUP.get_or_init(|| {
		legacy_cases()
			.into_iter()
			.map(|case| ((case.file_kind, case.key), case.legacy))
			.collect()
	})
}

fn legacy_container_scope_kind(file_kind: &str, key: &str) -> Option<ScopeKind> {
	legacy_case_lookup().get(&(file_kind, key)).copied()
}

fn describe_cwt_fields(graph: &CwtSchemaGraph, file_kind: &str, key: &str) -> Vec<String> {
	let mut descriptions = Vec::new();
	for definition in root_type_candidates(graph, file_kind) {
		collect_field_descriptions(
			graph,
			&format!("type:{}", definition.name.as_str()),
			BindContext::RootType(definition),
			definition.rules.as_slice(),
			key,
			&mut descriptions,
		);
		for subtype in &definition.subtypes {
			collect_field_descriptions(
				graph,
				&format!("type:{}:subtype:{}", definition.name.as_str(), subtype.name),
				BindContext::AliasRules(subtype.rules.as_slice()),
				subtype.rules.as_slice(),
				key,
				&mut descriptions,
			);
		}
	}
	descriptions.sort();
	descriptions.dedup();
	descriptions
}

fn root_type_candidates<'g>(graph: &'g CwtSchemaGraph, file_kind: &str) -> Vec<&'g CwtTypeDef> {
	let mut matches = graph
		.types
		.values()
		.filter(|definition| {
			definition.name.as_str() == file_kind
				|| definition
					.path
					.as_deref()
					.is_some_and(|path| schema_path_matches_file_kind(path, file_kind))
		})
		.collect::<Vec<_>>();
	matches.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
	matches
}

fn schema_path_matches_file_kind(path: &str, file_kind: &str) -> bool {
	let normalized = path
		.trim_start_matches("game/")
		.trim_matches('/')
		.to_ascii_lowercase();
	normalized == file_kind || normalized.rsplit('/').next() == Some(file_kind)
}

fn collect_field_descriptions<'g>(
	graph: &'g CwtSchemaGraph,
	context: &str,
	parent: BindContext<'g>,
	rules: &'g [CwtRuleField],
	key: &str,
	descriptions: &mut Vec<String>,
) {
	for field in graph.bind_fields(parent, key) {
		descriptions.push(format!(
			"{} => {} = {}",
			context,
			field.key,
			format_rule_value(&field.value)
		));
	}
	for field in rules {
		let CwtRuleValue::Block(children) = &field.value else {
			continue;
		};
		collect_field_descriptions(
			graph,
			&format!("{context}/{}", field.key),
			BindContext::RuleField(field),
			children.as_slice(),
			key,
			descriptions,
		);
	}
}

fn format_rule_value(value: &CwtRuleValue) -> String {
	match value {
		CwtRuleValue::Scalar(value) => format!("Scalar({value})"),
		CwtRuleValue::Marker(value) => format!("Marker({value})"),
		CwtRuleValue::Block(fields) => {
			let preview = fields
				.iter()
				.take(4)
				.map(|field| field.key.as_str())
				.collect::<Vec<_>>()
				.join(", ");
			if fields.len() > 4 {
				format!("Block({preview}, …)")
			} else {
				format!("Block({preview})")
			}
		}
	}
}

fn format_mismatches(mismatches: &[ContainerScopeMismatch]) -> String {
	mismatches
		.iter()
		.map(|mismatch| {
			let fields = if mismatch.cwt_fields.is_empty() {
				"<none>".to_string()
			} else {
				mismatch.cwt_fields.join(" | ")
			};
			format!(
				"{}:{} hand={:?} cwt={} cwt_fields={}",
				mismatch.file_kind,
				mismatch.key,
				mismatch.hand,
				format_scope_kind_option(mismatch.cwt),
				fields,
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn format_scope_kind_option(value: Option<ScopeKind>) -> String {
	value.map_or_else(|| "None".to_string(), |kind| format!("{:?}", kind))
}

fn format_vanilla_mismatches(mismatches: &[VanillaContainerScopeMismatch]) -> String {
	mismatches
		.iter()
		.take(200)
		.map(|mismatch| {
			format!(
				"{}:{}:{}:{} hand={} cwt={}",
				mismatch.file_kind,
				mismatch.path.display(),
				mismatch.line,
				mismatch.key,
				format_scope_kind_option(mismatch.legacy),
				format_scope_kind_option(mismatch.cwt),
			)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn walk_keys(statements: &[AstStatement], visit: &mut impl FnMut(&str, usize)) {
	for statement in statements {
		walk_statement(statement, visit);
	}
}

fn walk_statement(statement: &AstStatement, visit: &mut impl FnMut(&str, usize)) {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value,
			..
		} => {
			visit(key, key_span.start.line);
			walk_value(value, visit);
		}
		AstStatement::Item { value, .. } => walk_value(value, visit),
		AstStatement::Comment { .. } => {}
	}
}

fn walk_value(value: &AstValue, visit: &mut impl FnMut(&str, usize)) {
	if let AstValue::Block { items, .. } = value {
		walk_keys(items, visit);
	}
}
