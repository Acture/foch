use super::content_family::GameProfile;
use super::eu4_profile::eu4_profile;
use super::param_contracts::evaluate_param_contract;
use super::semantic_index::{
	build_inferred_callable_scope_map, collect_inferred_callable_masks,
	effective_alias_scope_mask_with_overrides, effective_scope_mask_with_overrides,
	resolve_cross_kind_reference_targets, resolve_event_reference_targets,
	resolve_scripted_effect_reference_targets, resolve_scripted_trigger_reference_targets,
};
use super::visibility::{should_flag_duplicates, should_flag_unresolved};
use foch_core::model::{
	AnalysisMode, Finding, FindingChannel, ScopeType, SemanticDiagnostics, SemanticIndex, Severity,
	SymbolKind,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct AnalyzeOptions {
	pub mode: AnalysisMode,
}

pub fn analyze_visibility(index: &SemanticIndex, _options: &AnalyzeOptions) -> SemanticDiagnostics {
	let mut diagnostics = SemanticDiagnostics::default();
	diagnostics
		.strict
		.extend(check_duplicate_definitions(index));
	diagnostics
		.strict
		.extend(check_unresolved_call_targets(index));
	let (invisible_alias_strict, invisible_alias_advisory) = check_invisible_scope_aliases(index);
	diagnostics.strict.extend(invisible_alias_strict);
	diagnostics.advisory.extend(invisible_alias_advisory);
	diagnostics
		.strict
		.extend(check_missing_effect_parameters(index));
	diagnostics.advisory.extend(check_unknown_scope_type(index));
	diagnostics
		.advisory
		.extend(check_scope_type_mismatch(index));
	diagnostics
		.advisory
		.extend(check_cross_mod_overlap_advisories(index));
	diagnostics
		.advisory
		.extend(check_unresolved_flag_references(index));
	diagnostics
		.advisory
		.extend(check_missing_localisation_keys(index));
	diagnostics
		.advisory
		.extend(check_duplicate_localisation_keys(index));
	diagnostics
}

fn check_duplicate_definitions(index: &SemanticIndex) -> Vec<Finding> {
	let mut grouped: HashMap<(SymbolKind, String), Vec<_>> = HashMap::new();
	for def in &index.definitions {
		if !should_flag_duplicates(def.kind) {
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
			rule_id: "cross-mod-overshadow".to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!("duplicate definition: {} {}", symbol_kind_text(kind), name),
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

fn check_unresolved_call_targets(index: &SemanticIndex) -> Vec<Finding> {
	let mut seen = HashSet::new();
	let mut findings = Vec::new();
	// Some EU4 trigger keys are not scripted_triggers but the *names* of
	// content definitions whose presence implicitly creates a trigger key
	// (e.g. an advisor type name `idea_var_advisor_5 = 2` tests "country
	// has hired an advisor of that type with skill ≥ 2"). These are routed
	// to the analyzer as ScriptedTrigger references because they sit on
	// the LHS of a scalar assignment in trigger context, but they will
	// never resolve to a scripted_trigger definition. Build a set of such
	// "implicit trigger key" names from the resource-reference index so
	// the resolver can tolerate them. Names are stored lowercased to
	// match the case-insensitive lookup convention used elsewhere.
	let mut implicit_trigger_names: HashSet<String> = HashSet::new();
	for resource in &index.resource_references {
		if resource.key == "advisor_type_definition" {
			implicit_trigger_names.insert(resource.value.to_ascii_lowercase());
		}
	}
	for reference in &index.references {
		if !should_flag_unresolved(reference.kind) {
			continue;
		}
		// Skip template parameters — they resolve at runtime
		if reference.name.contains('$') || reference.name.contains('[') {
			continue;
		}
		match reference.kind {
			SymbolKind::Event => {
				if !resolve_event_reference_targets(index, reference).is_empty() {
					continue;
				}
			}
			SymbolKind::ScriptedEffect => {
				if !resolve_scripted_effect_reference_targets(index, reference).is_empty() {
					continue;
				}
				// Cross-kind: effect reference might resolve to a trigger def
				if !resolve_cross_kind_reference_targets(
					index,
					reference,
					SymbolKind::ScriptedTrigger,
				)
				.is_empty()
				{
					continue;
				}
			}
			SymbolKind::ScriptedTrigger => {
				if !resolve_scripted_trigger_reference_targets(index, reference).is_empty() {
					continue;
				}
				// Cross-kind: trigger reference might resolve to an effect def
				if !resolve_cross_kind_reference_targets(
					index,
					reference,
					SymbolKind::ScriptedEffect,
				)
				.is_empty()
				{
					continue;
				}
				// Implicit trigger keys: advisor-type names act as trigger
				// keys at runtime ("country has advisor of type X with
				// skill ≥ N"). They never appear as scripted_trigger
				// definitions.
				if implicit_trigger_names.contains(&reference.name.to_ascii_lowercase()) {
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
			rule_id: "unresolved-call-target".to_string(),
			severity: Severity::Error,
			channel: FindingChannel::Strict,
			message: format!(
				"unresolved call: {} {}",
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

fn check_invisible_scope_aliases(index: &SemanticIndex) -> (Vec<Finding>, Vec<Finding>) {
	let mut seen = HashSet::new();
	let mut strict = Vec::new();
	let mut advisory = Vec::new();
	let profile = eu4_profile();
	for usage in &index.alias_usages {
		if is_alias_visible(index, usage.scope_id, usage.alias.as_str()) {
			continue;
		}
		// Files belonging to a dynamic-scope content family (callables,
		// scripted_effects/triggers/functions, on_actions, custom_gui,
		// customizable_localization, UI) have no statically-known root or
		// caller scope. THIS/ROOT/FROM/PREV are all by-design absent from
		// the alias map there, so flagging them — even as advisory — is pure
		// noise. The same skip is applied by `unknown-scope-type` and
		// `scope-type-mismatch`.
		if profile
			.classify_content_family(usage.path.as_path())
			.is_some_and(|descriptor| descriptor.scope_policy.dynamic_scope)
		{
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
		// FROM and PREV are runtime-bound scope aliases. They are injected by
		// callers (events, on_actions, scripted_effects/triggers invoked with an
		// explicit scope, decision potentials/effects, mission triggers,
		// custom_gui callbacks, ...). Static analysis cannot reliably determine
		// their visibility without context-sensitive flow analysis, so flagging
		// them as strict errors produces high-volume false positives. Demote
		// such usages to advisory with low confidence; reserve strict
		// `invisible-scope-alias` for THIS/ROOT, which are populated at file
		// scope unconditionally and so only become invisible due to genuine
		// indexing bugs.
		let is_runtime_bound = matches!(usage.alias.as_str(), "FROM" | "PREV");
		let (channel, severity, confidence) = if is_runtime_bound {
			(FindingChannel::Advisory, Severity::Info, 0.3)
		} else {
			(FindingChannel::Strict, Severity::Error, 0.9)
		};
		let finding = Finding {
			rule_id: "invisible-scope-alias".to_string(),
			severity,
			channel,
			message: format!("invisible alias reference: {}", usage.alias),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("scope_id={}", usage.scope_id)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(confidence),
		};
		if is_runtime_bound {
			advisory.push(finding);
		} else {
			strict.push(finding);
		}
	}
	(strict, advisory)
}

fn check_missing_effect_parameters(index: &SemanticIndex) -> Vec<Finding> {
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
		let optional: HashSet<&str> = def.optional_params.iter().map(String::as_str).collect();
		let missing_messages = if let Some(contract) = def.param_contract.as_ref() {
			evaluate_param_contract(contract, &reference.name, &provided)
		} else {
			def.required_params
				.iter()
				.filter(|required| {
					!provided.contains(required.as_str()) && !optional.contains(required.as_str())
				})
				.map(|required| {
					format!("unbound parameter: {} missing {}", reference.name, required)
				})
				.collect()
		};
		for message in missing_messages {
			let dedup_key = format!(
				"{}:{}:{}:{}",
				reference.path.display(),
				reference.line,
				reference.column,
				message
			);
			if !seen.insert(dedup_key) {
				continue;
			}
			findings.push(Finding {
				rule_id: "missing-effect-parameter".to_string(),
				severity: Severity::Error,
				channel: FindingChannel::Strict,
				message,
				mod_id: Some(reference.mod_id.clone()),
				path: Some(reference.path.clone()),
				evidence: Some(format!(
					"definition location {}:{}",
					def.path.display(),
					def.line
				)),
				line: Some(reference.line),
				column: Some(reference.column),
				confidence: Some(0.88),
			});
		}
	}
	findings
}

fn check_unknown_scope_type(index: &SemanticIndex) -> Vec<Finding> {
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

	let callable_scope_map = build_inferred_callable_scope_map(index);
	let inferred_masks = collect_inferred_callable_masks(index);
	let profile = eu4_profile();
	let mut findings = Vec::new();
	for usage in &index.key_usages {
		if !type_sensitive_keys.contains(usage.key.as_str()) {
			continue;
		}
		// Skip files whose content family has no statically determinable
		// implicit scope (callables, UI, customizable_localization,
		// on_actions, scripted_functions). Unknown is by-design there.
		if profile
			.classify_content_family(usage.path.as_path())
			.is_some_and(|descriptor| descriptor.scope_policy.dynamic_scope)
		{
			continue;
		}
		let scope_mask = match usage.key.as_str() {
			"THIS" | "ROOT" | "FROM" | "PREV" => effective_alias_scope_mask_with_overrides(
				index,
				&callable_scope_map,
				&inferred_masks,
				usage.scope_id,
				usage.key.as_str(),
			),
			_ => effective_scope_mask_with_overrides(
				index,
				&callable_scope_map,
				&inferred_masks,
				usage.scope_id,
			),
		};
		if scope_mask != 0 {
			continue;
		}
		findings.push(Finding {
			rule_id: "unknown-scope-type".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("unknown-scope path: key={} in Unknown scope", usage.key),
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

fn check_scope_type_mismatch(index: &SemanticIndex) -> Vec<Finding> {
	let country_only_keys: HashSet<&str> = [
		"country_event",
		"set_country_flag",
		"has_country_flag",
		"add_prestige",
		"add_stability",
	]
	.into_iter()
	.collect();

	let callable_scope_map = build_inferred_callable_scope_map(index);
	let inferred_masks = collect_inferred_callable_masks(index);
	let profile = eu4_profile();
	let mut findings = Vec::new();
	for usage in &index.key_usages {
		if !country_only_keys.contains(usage.key.as_str()) {
			continue;
		}
		// Skip files whose content family has no statically determinable
		// implicit scope (callables, UI, customizable_localization,
		// on_actions, scripted_functions). The runtime caller's scope is
		// unknown there, so flagging Province usage of country effects is
		// noise — same skip applied by unknown-scope-type.
		if profile
			.classify_content_family(usage.path.as_path())
			.is_some_and(|descriptor| descriptor.scope_policy.dynamic_scope)
		{
			continue;
		}
		if effective_scope_mask_with_overrides(
			index,
			&callable_scope_map,
			&inferred_masks,
			usage.scope_id,
		) != scope_type_mask(ScopeType::Province)
		{
			continue;
		}
		findings.push(Finding {
			rule_id: "scope-type-mismatch".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!(
				"potential weak type conflict: Province scope uses {}",
				usage.key
			),
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

fn check_cross_mod_overlap_advisories(index: &SemanticIndex) -> Vec<Finding> {
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
			rule_id: "mergeable-overlap".to_string(),
			severity: Severity::Info,
			channel: FindingChannel::Advisory,
			message: format!(
				"cross-mod same-name definition may change resolution target: {} {}",
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
	prefix: String,
	suffix: String,
	path: std::path::PathBuf,
	line: usize,
	column: usize,
}

/// Flags that are set or maintained by the EU4 engine itself (not via script
/// `set_*_flag = ...`). Reading these via `has_*_flag` is legitimate even when
/// no script setter exists, so we pre-seed them as defined to suppress
/// `unresolved-flag-reference` false positives.
const ENGINE_SET_FLAGS: &[(&str, &str)] = &[
	("country", "vanilla_achievements_enabled"),
	("country", "have_diploannexed"),
	("country", "conquered_province"),
	("country", "has_won_war"),
	("country", "religious_league_war_on_winning_side"),
	("country", "force_converted"),
	("country", "gives_enlightenment_to_neighbors"),
];

/// Pattern derived from a `set_*_flag = prefix_$param$_suffix` setter inside
/// some scripted_effect. Any read whose value matches `<prefix><alphanum_or_._->+<suffix>`
/// is treated as possibly defined: we cannot always pin down every binding
/// site (deeply-nested call chains, optional parameters, base-game callers
/// the indexer does not capture), so suppressing the read is safer than
/// emitting noise.
#[derive(Clone)]
struct TemplatedFlagPattern {
	kind: &'static str,
	prefix: String,
	suffix: String,
}

impl TemplatedFlagPattern {
	fn matches(&self, kind: &str, flag: &str) -> bool {
		if self.kind != kind {
			return false;
		}
		let Some(rest) = flag.strip_prefix(self.prefix.as_str()) else {
			return false;
		};
		let Some(middle) = rest.strip_suffix(self.suffix.as_str()) else {
			return false;
		};
		!middle.is_empty()
			&& middle
				.chars()
				.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | ':'))
	}
}

/// A templated setter is allowlist-eligible only if the surrounding literal
/// fragments are specific enough that the resulting pattern is unlikely to
/// match unrelated flags. Without this guard a setter like
/// `set_country_flag = $param$` would silently allow every read.
fn templated_pattern_is_specific(prefix: &str, suffix: &str) -> bool {
	let literal = prefix.len() + suffix.len();
	if literal < 4 {
		return false;
	}
	let alpha = prefix.chars().filter(|ch| ch.is_ascii_alphabetic()).count()
		+ suffix.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
	alpha >= 3
}

fn check_unresolved_flag_references(index: &SemanticIndex) -> Vec<Finding> {
	let mut defined_flags: HashSet<(String, String)> = HashSet::new();
	for (kind, flag) in ENGINE_SET_FLAGS {
		defined_flags.insert(((*kind).to_string(), (*flag).to_string()));
	}
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
		let Some((param_name, prefix, suffix)) = extract_param_template(usage.value.as_str())
		else {
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
			prefix,
			suffix,
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

	// Build a pattern allowlist from every templated setter found inside a
	// scripted_effect body. We do not require the call site to be present in
	// the index — base-game effects are sometimes invoked through optional
	// parameters or nested scopes the indexer does not cross, so the binding
	// expansion below misses those callers. The pattern check keeps reads of
	// flags shaped like `<prefix><value><suffix>` from being flagged when a
	// matching setter exists somewhere in the workspace.
	let mut templated_flag_patterns: Vec<TemplatedFlagPattern> = Vec::new();
	let mut seen_patterns: HashSet<(&'static str, String, String)> = HashSet::new();
	for templates in templated_flag_defs.values() {
		for template in templates {
			if !templated_pattern_is_specific(&template.prefix, &template.suffix) {
				continue;
			}
			let key = (
				template.kind,
				template.prefix.clone(),
				template.suffix.clone(),
			);
			if !seen_patterns.insert(key) {
				continue;
			}
			templated_flag_patterns.push(TemplatedFlagPattern {
				kind: template.kind,
				prefix: template.prefix.clone(),
				suffix: template.suffix.clone(),
			});
		}
	}
	let flag_is_allowlisted = |kind: &str, flag: &str| -> bool {
		templated_flag_patterns
			.iter()
			.any(|pattern| pattern.matches(kind, flag))
	};

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
						let Some(flag) = apply_flag_template(template, bound_value) else {
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
						if flag_is_allowlisted(template.kind, flag.as_str()) {
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
							rule_id: "unresolved-flag-reference".to_string(),
							severity: Severity::Warning,
							channel: FindingChannel::Advisory,
							message: format!(
								"flag may be undeclared: {}({}) references {}",
								template.op_key, template.kind, flag
							),
							mod_id: Some(reference.mod_id.clone()),
							path: Some(reference.path.clone()),
							evidence: Some(format!(
								"call {} binds {}={}; template {}:{}:{} has {} = {}${}${}; inferred value {}",
								def_name,
								template.param_name,
								bound_value,
								template.path.display(),
								template.line,
								template.column,
								template.op_key,
								template.prefix,
								template.param_name,
								template.suffix,
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
				let Some(flag) = apply_flag_template(template, bound_value) else {
					continue;
				};
				defined_flags.insert((template.kind.to_string(), flag));
			}
			if let Some(ref_templates) = templated_flag_refs.get(&target) {
				for template in ref_templates {
					let Some(bound_value) = bindings.get(template.param_name.as_str()) else {
						continue;
					};
					let Some(flag) = apply_flag_template(template, bound_value) else {
						continue;
					};
					if defined_flags.contains(&(template.kind.to_string(), flag.clone())) {
						continue;
					}
					if flag_is_allowlisted(template.kind, flag.as_str()) {
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
						rule_id: "unresolved-flag-reference".to_string(),
						severity: Severity::Warning,
						channel: FindingChannel::Advisory,
						message: format!(
							"flag may be undeclared: {}({}) references {}",
							template.op_key, template.kind, flag
						),
						mod_id: Some(reference.mod_id.clone()),
						path: Some(reference.path.clone()),
						evidence: Some(format!(
							"call {} binds {}={}; template {}:{}:{} has {} = {}${}${}; inferred value {}",
							def_name,
							template.param_name,
							bound_value,
							template.path.display(),
							template.line,
							template.column,
							template.op_key,
							template.prefix,
							template.param_name,
							template.suffix,
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

	// NOTE: We deliberately do NOT scan `index.scalar_assignments` for
	// literal `has_*_flag` / `had_*_flag` / `clr_*_flag` reads of unknown
	// flags. By Clausewitz syntax these reads are only legal in trigger
	// position (or, for `clr_*_flag`, in effect position), and an unresolved
	// literal read is benign engine semantics: the gate stays closed, or the
	// clear is a no-op. The canonical examples are cross-mod compat gates
	// (`has_global_flag = extended_timeline_mod`) and dead-code branches.
	// Real bugs surface through the templated-derivation paths above (sites
	// 1 and 2), where a parameterized setter is expected by construction but
	// missing.

	findings
}

fn check_missing_localisation_keys(index: &SemanticIndex) -> Vec<Finding> {
	let defined_keys: HashSet<&str> = index
		.localisation_definitions
		.iter()
		.map(|item| item.key.as_str())
		.collect();

	let mut findings = Vec::new();
	let mut seen = HashSet::new();
	for usage in &index.scalar_assignments {
		if path_disables_localisation_reference_check(usage.path.as_path()) {
			continue;
		}
		let Some(key) = normalized_static_symbol(usage.value.as_str()) else {
			continue;
		};
		if !is_localisation_reference_key(usage.key.as_str(), key.as_str()) {
			continue;
		}
		if is_non_localisation_literal(key.as_str()) {
			continue;
		}
		if usage.key == "name" && !is_localisation_name_scope(index, usage.scope_id) {
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
			rule_id: "missing-localisation".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("localisation key not found: {}", key),
			mod_id: Some(usage.mod_id.clone()),
			path: Some(usage.path.clone()),
			evidence: Some(format!("reference field {} = {}", usage.key, key)),
			line: Some(usage.line),
			column: Some(usage.column),
			confidence: Some(0.68),
		});
	}
	findings
}

fn check_duplicate_localisation_keys(index: &SemanticIndex) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut seen = HashSet::new();
	for duplicate in &index.localisation_duplicates {
		let dedup_key = format!(
			"{}:{}:{}",
			duplicate.path.display(),
			duplicate.key,
			duplicate.duplicate_line
		);
		if !seen.insert(dedup_key) {
			continue;
		}
		findings.push(Finding {
			rule_id: "duplicate-localisation".to_string(),
			severity: Severity::Warning,
			channel: FindingChannel::Advisory,
			message: format!("duplicate localisation key: {}", duplicate.key),
			mod_id: Some(duplicate.mod_id.clone()),
			path: Some(duplicate.path.clone()),
			evidence: Some(format!(
				"first defined at line {}, duplicate at line {}",
				duplicate.first_line, duplicate.duplicate_line
			)),
			line: Some(duplicate.duplicate_line),
			column: Some(1),
			confidence: Some(0.84),
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

fn scope_type_mask(scope_type: ScopeType) -> u8 {
	match scope_type {
		ScopeType::Country => 0b01,
		ScopeType::Province => 0b10,
		ScopeType::Unknown => 0,
	}
}

fn extract_param_template(value: &str) -> Option<(&str, String, String)> {
	let value = value.trim();
	let bytes = value.as_bytes();
	let mut idx = 0;
	let mut found: Option<(usize, usize, &str)> = None;
	while idx < bytes.len() {
		if bytes[idx] == b'$' {
			let start = idx;
			let name_start = idx + 1;
			let mut end = name_start;
			while end < bytes.len() {
				let ch = bytes[end];
				if ch.is_ascii_alphanumeric() || ch == b'_' {
					end += 1;
				} else {
					break;
				}
			}
			if end < bytes.len() && bytes[end] == b'$' && end > name_start {
				if found.is_some() {
					return None;
				}
				let name = &value[name_start..end];
				found = Some((start, end + 1, name));
				idx = end + 1;
				continue;
			}
			return None;
		}
		idx += 1;
	}
	let (start, after_end, name) = found?;
	let prefix = &value[..start];
	let suffix = &value[after_end..];
	if !is_valid_flag_literal(prefix) || !is_valid_flag_literal(suffix) {
		return None;
	}
	Some((name, prefix.to_string(), suffix.to_string()))
}

fn is_valid_flag_literal(part: &str) -> bool {
	part.chars()
		.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | ':'))
}

fn apply_flag_template(template: &FlagTemplateUsage, bound_value: &str) -> Option<String> {
	let bound = normalized_static_symbol(bound_value)?;
	let candidate = format!("{}{}{}", template.prefix, bound, template.suffix);
	normalized_static_symbol(&candidate)
}

fn normalized_static_symbol(value: &str) -> Option<String> {
	let value = value.trim();
	if value.is_empty() || value.contains(char::is_whitespace) {
		return None;
	}
	if value.contains('$') || value.contains('[') || value.contains(']') {
		return None;
	}
	if value.contains('@') || value.contains("event_target:") || value.contains("trigger_value:") {
		return None;
	}
	if value
		.chars()
		.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | ':'))
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

/// Files whose `name`/`tooltip`/`title`/`desc` fields are structural identifiers
/// (GUI element names, sprite names, customizable_localization macro IDs, map names),
/// not Clausewitz localisation key references.
fn path_disables_localisation_reference_check(path: &Path) -> bool {
	if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
		let lower = ext.to_ascii_lowercase();
		if matches!(lower.as_str(), "gui" | "gfx") {
			return true;
		}
	}
	let normalized = path.to_string_lossy().replace('\\', "/");
	let top = normalized.split('/').next().unwrap_or("");
	if matches!(
		top,
		"interface" | "gfx" | "map" | "tweakergui_assets" | "customizable_localization"
	) {
		return true;
	}
	normalized.starts_with("common/custom_gui/")
}

/// Values that pass `is_localisation_reference_key` but are not loc keys:
/// numeric template indices (`localisation_key = 0`, `custom_tooltip = 4`)
/// and boolean tooltip toggles (`tooltip = yes`).
fn is_non_localisation_literal(value: &str) -> bool {
	if matches!(value, "yes" | "no") {
		return true;
	}
	!value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

/// `name = X` is only a localisation key reference inside event option blocks.
/// Elsewhere (`monarch`, `heir`, `leader`, `define_advisor`, decision/modifier
/// `name` fields, mission slot names, etc.) it is a character first name or
/// a script identifier.
fn is_localisation_name_scope(index: &SemanticIndex, scope_id: usize) -> bool {
	index
		.scopes
		.get(scope_id)
		.is_some_and(|scope| scope.key == "option")
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
		SymbolKind::ScriptedTrigger => "scripted_trigger",
		SymbolKind::Event => "event",
		SymbolKind::Decision => "decision",
		SymbolKind::DiplomaticAction => "diplomatic_action",
		SymbolKind::TriggeredModifier => "triggered_modifier",
	}
}
