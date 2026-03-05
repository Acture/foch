use crate::check::model::{
	AnalysisMode, Finding, FindingChannel, ScopeType, SemanticDiagnostics, SemanticIndex, Severity,
	SymbolKind,
};
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
	let mut defined = HashSet::new();
	for def in &index.definitions {
		if matches!(def.kind, SymbolKind::Event | SymbolKind::ScriptedEffect) {
			defined.insert((def.kind, def.name.clone()));
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
		if defined.contains(&(reference.kind, reference.name.clone())) {
			continue;
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
	let mut defs: HashMap<&str, Vec<_>> = HashMap::new();
	for def in &index.definitions {
		if def.kind == SymbolKind::ScriptedEffect {
			defs.entry(def.name.as_str()).or_default().push(def);
		}
	}

	let mut findings = Vec::new();
	let mut seen = HashSet::new();
	for reference in &index.references {
		if reference.kind != SymbolKind::ScriptedEffect {
			continue;
		}
		let Some(candidates) = defs.get(reference.name.as_str()) else {
			continue;
		};
		let def = candidates
			.iter()
			.copied()
			.find(|item| item.mod_id == reference.mod_id)
			.or_else(|| candidates.first().copied());
		let Some(def) = def else {
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
