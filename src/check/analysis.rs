use crate::check::model::{
	AnalysisMode, Finding, FindingChannel, ScopeType, SemanticDiagnostics, SemanticIndex, Severity,
	SymbolKind,
};
use crate::check::semantic_index::resolve_scripted_effect_reference_targets;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct AnalyzeOptions {
	pub mode: AnalysisMode,
}

pub fn analyze_visibility(index: &SemanticIndex, _options: &AnalyzeOptions) -> SemanticDiagnostics {
	let mut diagnostics = SemanticDiagnostics::default();
	diagnostics.strict.extend(check_s001_duplicates(index));
	diagnostics
		.strict
		.extend(check_s002_unresolved_calls(index));
	diagnostics.strict.extend(check_s003_invisible_alias(index));
	diagnostics.strict.extend(check_s004_unbound_params(index));
	diagnostics
		.advisory
		.extend(check_a001_unknown_scope_type(index));
	diagnostics
		.advisory
		.extend(check_a002_weak_type_conflict(index));
	diagnostics
		.advisory
		.extend(check_a003_cross_mod_override(index));
	diagnostics
		.advisory
		.extend(check_a004_unresolved_flag_symbol(index));
	diagnostics
		.advisory
		.extend(check_a005_missing_localisation_key(index));
	diagnostics
}

fn check_s001_duplicates(index: &SemanticIndex) -> Vec<Finding> {
	let mut grouped: HashMap<(SymbolKind, String), Vec<_>> = HashMap::new();
	for def in &index.definitions {
		if !matches!(def.kind, SymbolKind::Event | SymbolKind::ScriptedEffect) {
			continue;
		}
		grouped
			.entry((def.kind, def.name.clone()))
			.or_default()
			.push(def);
	}

	let mut findings = Vec::new();
	for ((kind, name), defs) in grouped {
		if defs.len() < 2 {
			continue;
		}
		let evidence = defs
			.iter()
			.map(|item| {
				format!(
					"{}:{}:{}:{}",
					item.mod_id,
					item.path.display(),
					item.line,
					item.column
				)
			})
			.collect::<Vec<_>>()
			.join("; ");
		let Some(last) = defs.last() else {
			continue;
		};
		findings.push(Finding {
			rule_id: "S001".to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!("重复定义: {} {}", symbol_kind_text(kind), name),
			mod_id: Some(last.mod_id.clone()),
			path: Some(last.path.clone()),
			evidence: Some(evidence),
			line: Some(last.line),
			column: Some(last.column),
			confidence: Some(1.0),
		});
	}
	findings
}

fn check_s002_unresolved_calls(index: &SemanticIndex) -> Vec<Finding> {
	let mut defined_events = HashSet::new();
	for def in &index.definitions {
		if def.kind == SymbolKind::Event {
			defined_events.insert(def.name.clone());
		}
	}

	let mut seen = HashSet::new();
	let mut findings = Vec::new();
	for reference in &index.references {
		if !matches!(
			reference.kind,
			SymbolKind::Event | SymbolKind::ScriptedEffect
		) {
			continue;
		}
		match reference.kind {
			SymbolKind::Event => {
				if defined_events.contains(reference.name.as_str()) {
					continue;
				}
			}
			SymbolKind::ScriptedEffect => {
				if !resolve_scripted_effect_reference_targets(index, reference).is_empty() {
					continue;
				}
			}
			_ => {}
		}

		let dedup_key = format!(
			"{:?}:{}:{}:{}:{}",
			reference.kind,
			reference.path.display(),
			reference.line,
			reference.column,
			reference.name
		);
		if !seen.insert(dedup_key) {
			continue;
		}

		findings.push(Finding {
			rule_id: "S002".to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!(
				"未解析调用: {} {}",
				symbol_kind_text(reference.kind),
				reference.name
			),
			mod_id: Some(reference.mod_id.clone()),
			path: Some(reference.path.clone()),
			evidence: None,
			line: Some(reference.line),
			column: Some(reference.column),
			confidence: Some(0.95),
		});
	}
	findings
}

fn check_s003_invisible_alias(index: &SemanticIndex) -> Vec<Finding> {
	let mut seen = HashSet::new();
	let mut findings = Vec::new();
	for usage in &index.alias_usages {
		if is_alias_visible(index, usage.scope_id, usage.alias.as_str()) {
			continue;
		}
		let dedup_key = format!(
			"{}:{}:{}:{}",
			usage.path.display(),
			usage.line,
			usage.column,
			usage.alias
		);
		if !seen.insert(dedup_key) {
			continue;
		}
		findings.push(Finding {
			rule_id: "S003".to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!("不可见别名引用: {}", usage.alias),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("scope_id={}", usage.scope_id)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.9),
		});
	}
	findings
}

fn check_s004_unbound_params(index: &SemanticIndex) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut seen = HashSet::new();
	for reference in &index.references {
		if reference.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		let targets = resolve_scripted_effect_reference_targets(index, reference);
		let Some(def_idx) = targets.first().copied() else {
			continue;
		};
		let Some(def) = index.definitions.get(def_idx) else {
			continue;
		};

		let provided: HashSet<&str> = reference
			.provided_params
			.iter()
			.map(String::as_str)
			.collect();
		for required in &def.required_params {
			if provided.contains(required.as_str()) {
				continue;
			}
			let dedup_key = format!(
				"{}:{}:{}:{}",
				reference.path.display(),
				reference.line,
				reference.column,
				required
			);
			if !seen.insert(dedup_key) {
				continue;
			}
			findings.push(Finding {
				rule_id: "S004".to_string(),
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message: format!("参数未绑定: {} 缺失 {}", reference.name, required),
				mod_id: Some(reference.mod_id.clone()),
				path: Some(reference.path.clone()),
				evidence: Some(format!("定义位置 {}:{}", def.path.display(), def.line)),
				line: Some(reference.line),
				column: Some(reference.column),
				confidence: Some(0.88),
			});
		}
	}
	findings
}

fn check_a001_unknown_scope_type(index: &SemanticIndex) -> Vec<Finding> {
	let type_sensitive_keys: HashSet<&str> = [
		"ROOT",
		"FROM",
		"THIS",
		"PREV",
		"country_event",
		"every_owned_province",
		"owner",
		"capital_scope",
	]
	.into_iter()
	.collect();

	let mut findings = Vec::new();
	for usage in &index.key_usages {
		if usage.this_type != ScopeType::Unknown {
			continue;
		}
		if !type_sensitive_keys.contains(usage.key.as_str()) {
			continue;
		}
		findings.push(Finding {
			rule_id: "A001".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("类型不确定路径: key={} 在 Unknown scope", usage.key),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("scope_id={}", usage.scope_id)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.6),
		});
	}
	findings
}

fn check_a002_weak_type_conflict(index: &SemanticIndex) -> Vec<Finding> {
	let country_only_keys: HashSet<&str> = [
		"country_event",
		"set_country_flag",
		"has_country_flag",
		"add_prestige",
		"add_stability",
	]
	.into_iter()
	.collect();

	let mut findings = Vec::new();
	for usage in &index.key_usages {
		if usage.this_type != ScopeType::Province {
			continue;
		}
		if !country_only_keys.contains(usage.key.as_str()) {
			continue;
		}
		findings.push(Finding {
			rule_id: "A002".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("潜在类型弱冲突: Province scope 使用 {}", usage.key),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("scope_id={}", usage.scope_id)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.55),
		});
	}
	findings
}

fn check_a003_cross_mod_override(index: &SemanticIndex) -> Vec<Finding> {
	let mut grouped: HashMap<(SymbolKind, String), Vec<_>> = HashMap::new();
	for def in &index.definitions {
		grouped
			.entry((def.kind, def.name.clone()))
			.or_default()
			.push(def);
	}

	let mut findings = Vec::new();
	for ((kind, name), defs) in grouped {
		let mut mods = HashSet::new();
		for def in &defs {
			mods.insert(def.mod_id.as_str());
		}
		if mods.len() < 2 {
			continue;
		}
		let Some(last) = defs.last() else {
			continue;
		};
		let evidence = defs
			.iter()
			.map(|item| item.mod_id.as_str())
			.collect::<Vec<_>>()
			.join(" -> ");
		findings.push(Finding {
			rule_id: "A003".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!(
				"跨 Mod 同名定义可能改变解析目标: {} {}",
				symbol_kind_text(kind),
				name
			),
			mod_id: Some(last.mod_id.clone()),
			path: Some(last.path.clone()),
			evidence: Some(evidence),
			line: Some(last.line),
			column: Some(last.column),
			confidence: Some(0.7),
		});
	}
	findings
}

#[derive(Clone, Copy)]
enum FlagUsageKind {
	Definition,
	Reference,
}

#[derive(Clone, Copy)]
struct FlagOpSpec {
	kind: &'static str,
	usage: FlagUsageKind,
}

#[derive(Clone)]
struct FlagTemplateUsage {
	kind: &'static str,
	op_key: String,
	param_name: String,
	path: std::path::PathBuf,
	line: usize,
	column: usize,
}

fn check_a004_unresolved_flag_symbol(index: &SemanticIndex) -> Vec<Finding> {
	let mut defined_flags: HashSet<(String, String)> = HashSet::new();
	let mut findings = Vec::new();
	let mut seen = HashSet::new();

	for usage in &index.scalar_assignments {
		if let Some(spec) = flag_op_spec(usage.key.as_str())
			&& matches!(spec.usage, FlagUsageKind::Definition)
			&& let Some(flag) = normalized_static_symbol(usage.value.as_str())
		{
			defined_flags.insert((spec.kind.to_string(), flag));
		}
	}

	let mut templated_flag_defs: HashMap<usize, Vec<FlagTemplateUsage>> = HashMap::new();
	let mut templated_flag_refs: HashMap<usize, Vec<FlagTemplateUsage>> = HashMap::new();
	let scripted_effect_scope_map: HashMap<usize, usize> = index
		.definitions
		.iter()
		.enumerate()
		.filter_map(|(idx, definition)| {
			if definition.kind == SymbolKind::ScriptedEffect {
				Some((definition.scope_id, idx))
			} else {
				None
			}
		})
		.collect();

	for usage in &index.scalar_assignments {
		let Some(spec) = flag_op_spec(usage.key.as_str()) else {
			continue;
		};
		let Some(param_name) = extract_param_name(usage.value.as_str()) else {
			continue;
		};
		let Some(def_idx) =
			enclosing_scripted_effect_definition(index, &scripted_effect_scope_map, usage.scope_id)
		else {
			continue;
		};
		let template = FlagTemplateUsage {
			kind: spec.kind,
			op_key: usage.key.clone(),
			param_name: param_name.to_string(),
			path: usage.path.clone(),
			line: usage.line,
			column: usage.column,
		};
		match spec.usage {
			FlagUsageKind::Definition => templated_flag_defs
				.entry(def_idx)
				.or_default()
				.push(template),
			FlagUsageKind::Reference => templated_flag_refs
				.entry(def_idx)
				.or_default()
				.push(template),
		}
	}

	for reference in &index.references {
		if reference.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		let bindings: HashMap<&str, &str> = reference
			.param_bindings
			.iter()
			.map(|binding| (binding.name.as_str(), binding.value.as_str()))
			.collect();
		if bindings.is_empty() {
			continue;
		}
		for target in resolve_scripted_effect_reference_targets(index, reference) {
			let Some(templates) = templated_flag_defs.get(&target) else {
				if let Some(ref_templates) = templated_flag_refs.get(&target) {
					for template in ref_templates {
						let Some(bound_value) = bindings.get(template.param_name.as_str()) else {
							continue;
						};
						let Some(flag) = normalized_static_symbol(bound_value) else {
							continue;
						};
						let dedup_key = format!(
							"{}:{}:{}:{}:{}",
							reference.path.display(),
							reference.line,
							reference.column,
							template.kind,
							flag
						);
						if defined_flags.contains(&(template.kind.to_string(), flag.clone())) {
							continue;
						}
						if !seen.insert(dedup_key) {
							continue;
						}
						let def_name = index
							.definitions
							.get(target)
							.map(|item| item.name.as_str())
							.unwrap_or(reference.name.as_str());
						findings.push(Finding {
							rule_id: "A004".to_string(),
							severity: Severity::Warning,
							channel: FindingChannel::Advisory,
							message: format!(
								"flag 可能未声明: {}({}) 引用 {}",
								template.op_key, template.kind, flag
							),
							mod_id: Some(reference.mod_id.clone()),
							path: Some(reference.path.clone()),
							evidence: Some(format!(
								"调用 {} 绑定 {}={}；模板 {}:{}:{} 中 {} = ${}$；推导值 {}",
								def_name,
								template.param_name,
								bound_value,
								template.path.display(),
								template.line,
								template.column,
								template.op_key,
								template.param_name,
								flag
							)),
							line: Some(reference.line),
							column: Some(reference.column),
							confidence: Some(0.78),
						});
					}
				}
				continue;
			};
			for template in templates {
				let Some(bound_value) = bindings.get(template.param_name.as_str()) else {
					continue;
				};
				let Some(flag) = normalized_static_symbol(bound_value) else {
					continue;
				};
				defined_flags.insert((template.kind.to_string(), flag));
			}
			if let Some(ref_templates) = templated_flag_refs.get(&target) {
				for template in ref_templates {
					let Some(bound_value) = bindings.get(template.param_name.as_str()) else {
						continue;
					};
					let Some(flag) = normalized_static_symbol(bound_value) else {
						continue;
					};
					if defined_flags.contains(&(template.kind.to_string(), flag.clone())) {
						continue;
					}
					let dedup_key = format!(
						"{}:{}:{}:{}:{}",
						reference.path.display(),
						reference.line,
						reference.column,
						template.kind,
						flag
					);
					if !seen.insert(dedup_key) {
						continue;
					}
					let def_name = index
						.definitions
						.get(target)
						.map(|item| item.name.as_str())
						.unwrap_or(reference.name.as_str());
					findings.push(Finding {
						rule_id: "A004".to_string(),
						severity: Severity::Warning,
						channel: FindingChannel::Advisory,
						message: format!(
							"flag 可能未声明: {}({}) 引用 {}",
							template.op_key, template.kind, flag
						),
						mod_id: Some(reference.mod_id.clone()),
						path: Some(reference.path.clone()),
						evidence: Some(format!(
							"调用 {} 绑定 {}={}；模板 {}:{}:{} 中 {} = ${}$；推导值 {}",
							def_name,
							template.param_name,
							bound_value,
							template.path.display(),
							template.line,
							template.column,
							template.op_key,
							template.param_name,
							flag
						)),
						line: Some(reference.line),
						column: Some(reference.column),
						confidence: Some(0.78),
					});
				}
			}
		}
	}

	for usage in &index.scalar_assignments {
		let Some(spec) = flag_op_spec(usage.key.as_str()) else {
			continue;
		};
		if !matches!(spec.usage, FlagUsageKind::Reference) {
			continue;
		}
		let Some(flag) = normalized_static_symbol(usage.value.as_str()) else {
			continue;
		};
		if defined_flags.contains(&(spec.kind.to_string(), flag.clone())) {
			continue;
		}
		let dedup_key = format!(
			"{}:{}:{}:{}",
			usage.path.display(),
			usage.line,
			usage.column,
			flag
		);
		if !seen.insert(dedup_key) {
			continue;
		}
		findings.push(Finding {
			rule_id: "A004".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!(
				"flag 可能未声明: {}({}) 引用 {}",
				usage.key, spec.kind, flag
			),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("直接引用 {} = {}", usage.key, flag)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.7),
		});
	}

	findings
}

fn check_a005_missing_localisation_key(index: &SemanticIndex) -> Vec<Finding> {
	let defined_keys: HashSet<&str> = index
		.localisation_definitions
		.iter()
		.map(|item| item.key.as_str())
		.collect();

	let mut findings = Vec::new();
	let mut seen = HashSet::new();
	for usage in &index.scalar_assignments {
		let Some(key) = normalized_static_symbol(usage.value.as_str()) else {
			continue;
		};
		if !is_localisation_reference_key(usage.key.as_str(), key.as_str()) {
			continue;
		}
		if defined_keys.contains(key.as_str()) {
			continue;
		}
		let dedup_key = format!(
			"{}:{}:{}:{}",
			usage.path.display(),
			usage.line,
			usage.column,
			key
		);
		if !seen.insert(dedup_key) {
			continue;
		}
		findings.push(Finding {
			rule_id: "A005".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("localisation key 未找到: {}", key),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("引用字段 {} = {}", usage.key, key)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.68),
		});
	}
	findings
}

fn flag_op_spec(key: &str) -> Option<FlagOpSpec> {
	let spec = match key {
		"set_global_flag" => FlagOpSpec {
			kind: "global",
			usage: FlagUsageKind::Definition,
		},
		"set_country_flag" => FlagOpSpec {
			kind: "country",
			usage: FlagUsageKind::Definition,
		},
		"set_province_flag" | "set_permanent_province_flag" => FlagOpSpec {
			kind: "province",
			usage: FlagUsageKind::Definition,
		},
		"set_ruler_flag" => FlagOpSpec {
			kind: "ruler",
			usage: FlagUsageKind::Definition,
		},
		"set_heir_flag" => FlagOpSpec {
			kind: "heir",
			usage: FlagUsageKind::Definition,
		},
		"set_consort_flag" => FlagOpSpec {
			kind: "consort",
			usage: FlagUsageKind::Definition,
		},
		"has_global_flag" | "clr_global_flag" | "had_global_flag" => FlagOpSpec {
			kind: "global",
			usage: FlagUsageKind::Reference,
		},
		"has_country_flag" | "clr_country_flag" | "had_country_flag" => FlagOpSpec {
			kind: "country",
			usage: FlagUsageKind::Reference,
		},
		"has_province_flag" | "clr_province_flag" | "had_province_flag" => FlagOpSpec {
			kind: "province",
			usage: FlagUsageKind::Reference,
		},
		"has_ruler_flag" | "clr_ruler_flag" | "had_ruler_flag" => FlagOpSpec {
			kind: "ruler",
			usage: FlagUsageKind::Reference,
		},
		"has_heir_flag" | "clr_heir_flag" | "had_heir_flag" => FlagOpSpec {
			kind: "heir",
			usage: FlagUsageKind::Reference,
		},
		"has_consort_flag" | "clr_consort_flag" | "had_consort_flag" => FlagOpSpec {
			kind: "consort",
			usage: FlagUsageKind::Reference,
		},
		_ => return None,
	};
	Some(spec)
}

fn enclosing_scripted_effect_definition(
	index: &SemanticIndex,
	scope_to_definition: &HashMap<usize, usize>,
	mut scope_id: usize,
) -> Option<usize> {
	loop {
		if let Some(def_idx) = scope_to_definition.get(&scope_id) {
			return Some(*def_idx);
		}
		let parent = index.scopes.get(scope_id).and_then(|scope| scope.parent)?;
		scope_id = parent;
	}
}

fn extract_param_name(value: &str) -> Option<&str> {
	let value = value.trim();
	if !(value.starts_with('$') && value.ends_with('$') && value.len() > 2) {
		return None;
	}
	let param = &value[1..value.len() - 1];
	if param
		.chars()
		.all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
	{
		Some(param)
	} else {
		None
	}
}

fn normalized_static_symbol(value: &str) -> Option<String> {
	let value = value.trim();
	if value.is_empty() || value.contains(char::is_whitespace) {
		return None;
	}
	if value.contains('$') || value.contains('[') || value.contains(']') {
		return None;
	}
	if value
		.chars()
		.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '@' | ':'))
	{
		Some(value.to_string())
	} else {
		None
	}
}

fn is_localisation_reference_key(key: &str, value: &str) -> bool {
	match key {
		"tooltip" | "custom_tooltip" | "localisation_key" | "title" | "desc" => true,
		"name" => looks_like_localisation_name(value),
		_ => false,
	}
}

fn looks_like_localisation_name(value: &str) -> bool {
	value.contains('.')
		|| value.chars().any(|ch| ch.is_ascii_uppercase())
		|| value.ends_with("_title")
		|| value.ends_with("_desc")
		|| value.ends_with("_tt")
		|| value.ends_with("_tooltip")
		|| value.ends_with("_localization")
}

fn is_alias_visible(index: &SemanticIndex, mut scope_id: usize, alias: &str) -> bool {
	loop {
		let Some(scope) = index.scopes.get(scope_id) else {
			return false;
		};
		if scope.aliases.contains_key(alias) {
			return true;
		}
		let Some(parent) = scope.parent else {
			return false;
		};
		scope_id = parent;
	}
}

fn symbol_kind_text(kind: SymbolKind) -> &'static str {
	match kind {
		SymbolKind::ScriptedEffect => "scripted_effect",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}
