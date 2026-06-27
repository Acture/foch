use foch_core::model::{
	AnalysisMode, MaybeScope, ScopeSet, SemanticIndex, SymbolDefinition, SymbolKind,
	SymbolReference, test_support,
};
use foch_language::analyzer::analysis::{AnalyzeOptions, analyze_visibility_with_vanilla_index};
use foch_language::analyzer::vanilla_index::VanillaSymbolIndex;
use std::path::PathBuf;

fn reference(kind: SymbolKind, name: &str) -> SymbolReference {
	SymbolReference {
		kind,
		name: name.to_string(),
		module: "mod_module".to_string(),
		mod_id: "test_mod".to_string(),
		path: PathBuf::from("common/scripted_effects/test.txt"),
		line: 3,
		column: 2,
		scope_id: 0,
		provided_params: Vec::new(),
		param_bindings: Vec::new(),
	}
}

fn definition(kind: SymbolKind, name: &str, local_name: &str) -> SymbolDefinition {
	test_support::install_defaults();
	SymbolDefinition {
		kind,
		name: name.to_string(),
		module: "vanilla".to_string(),
		local_name: local_name.to_string(),
		mod_id: "__game__eu4".to_string(),
		path: PathBuf::from("common/scripted_effects/vanilla.txt"),
		line: 1,
		column: 1,
		scope_id: 0,
		declared_this_type: MaybeScope::Unknown,
		inferred_this_type: MaybeScope::Unknown,
		inferred_this_mask: ScopeSet::EMPTY,
		inferred_from_mask: ScopeSet::EMPTY,
		inferred_root_mask: ScopeSet::EMPTY,
		required_params: Vec::new(),
		optional_params: Vec::new(),
		param_contract: None,
		scope_param_names: Vec::new(),
	}
}

fn analyze(index: &SemanticIndex, vanilla_index: &VanillaSymbolIndex) -> Vec<String> {
	analyze_visibility_with_vanilla_index(
		index,
		&AnalyzeOptions {
			mode: AnalysisMode::Semantic,
		},
		Some(vanilla_index),
	)
	.strict
	.into_iter()
	.map(|finding| finding.rule_id)
	.collect()
}

#[test]
fn s002_resolves_via_vanilla_emits_stale_fallback_not_unresolved() {
	let mod_index = SemanticIndex {
		references: vec![reference(SymbolKind::ScriptedEffect, "vanilla_effect")],
		..Default::default()
	};
	let vanilla_index = VanillaSymbolIndex::from_semantic_index(&SemanticIndex {
		definitions: vec![definition(
			SymbolKind::ScriptedEffect,
			"eu4::common.scripted_effects::vanilla_effect",
			"vanilla_effect",
		)],
		..Default::default()
	});

	let rule_ids = analyze(&mod_index, &vanilla_index);

	assert!(rule_ids.iter().any(|rule| rule == "stale-vanilla-fallback"));
	assert!(!rule_ids.iter().any(|rule| rule == "unresolved-call-target"));
}

#[test]
fn s002_no_vanilla_match_still_emits_unresolved() {
	let mod_index = SemanticIndex {
		references: vec![reference(SymbolKind::ScriptedEffect, "missing_effect")],
		..Default::default()
	};
	let vanilla_index = VanillaSymbolIndex::from_semantic_index(&SemanticIndex::default());

	let rule_ids = analyze(&mod_index, &vanilla_index);

	assert!(rule_ids.iter().any(|rule| rule == "unresolved-call-target"));
	assert!(!rule_ids.iter().any(|rule| rule == "stale-vanilla-fallback"));
}
