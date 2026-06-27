#[path = "../src/analyzer/syntax_bridge.rs"]
mod syntax_bridge;

use std::path::{Path, PathBuf};

use foch_core::model::{MaybeScope, ScopeSet};
use foch_language::analyzer::parser::AstFile;
use foch_language::analyzer::semantic_index::{
	ParsedScriptFile, build_semantic_index, parse_script_file,
};
use foch_syntax::ParadoxTree;

fn corpus_root(mod_name: &str) -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("..")
		.join("..")
		.join("tests")
		.join("corpus")
		.join(mod_name)
}

fn bridged_file(mod_name: &str, mod_id: &str, relative: &str) -> ParsedScriptFile {
	let root = corpus_root(mod_name);
	let path = root.join(relative);
	let hand = parse_script_file(mod_id, &root, &path).expect("parse corpus file with hand parser");
	let tree =
		ParadoxTree::parse(hand.source.as_bytes()).expect("parse corpus file with tree-sitter");
	assert!(
		!tree.has_error(),
		"tree-sitter reported parse errors for {relative}"
	);
	let nodes = tree
		.nodes()
		.expect("project tree-sitter parse into ParadoxNode");
	let statements = nodes
		.iter()
		.flat_map(|node| syntax_bridge::paradox_node_to_ast_statement(node, &hand.source))
		.collect();
	ParsedScriptFile {
		ast: AstFile {
			path: hand.ast.path.clone(),
			statements,
		},
		parse_issues: Vec::new(),
		parse_cache_hit: false,
		..hand
	}
}

fn assert_semantic_equivalence(mod_name: &str, mod_id: &str, relatives: &[&str]) {
	let root = corpus_root(mod_name);
	let hand_files: Vec<_> = relatives
		.iter()
		.map(|relative| {
			parse_script_file(mod_id, &root, &root.join(relative)).expect("parse corpus file")
		})
		.collect();
	let tree_files: Vec<_> = relatives
		.iter()
		.map(|relative| bridged_file(mod_name, mod_id, relative))
		.collect();

	let hand_index = build_semantic_index(&hand_files);
	let tree_index = build_semantic_index(&tree_files);

	assert_eq!(
		normalize_definitions(&hand_index),
		normalize_definitions(&tree_index)
	);
	assert_eq!(
		normalize_references(&hand_index),
		normalize_references(&tree_index)
	);
	assert_eq!(
		normalize_scope_aliases(&hand_index),
		normalize_scope_aliases(&tree_index)
	);
}

#[test]
fn tree_sitter_bridge_matches_hand_parser_semantics() {
	assert_semantic_equivalence(
		"control_military_access",
		"ctrlma",
		&[
			"events/CTRLMA_config_events.txt",
			"common/scripted_effects/CTRLMA_scripted_effects.txt",
		],
	);
	assert_semantic_equivalence(
		"defines",
		"defines",
		&["common/scripted_effects/player.txt"],
	);
	assert_semantic_equivalence("defines", "defines", &["decisions/01_player_decision.txt"]);
}

fn normalize_definitions(index: &foch_core::model::SemanticIndex) -> Vec<String> {
	let mut values: Vec<_> = index
		.definitions
		.iter()
		.map(|definition| {
			format!(
				"{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
				definition.kind.as_str(),
				definition.name,
				definition.module,
				definition.local_name,
				definition.mod_id,
				normalize_path(&definition.path),
				scope_type(definition.declared_this_type),
				scope_type(definition.inferred_this_type),
				scope_set(definition.inferred_this_mask),
				scope_set(definition.inferred_from_mask),
				scope_set(definition.inferred_root_mask),
				definition.required_params.join(","),
				definition.optional_params.join(","),
				scope_signature(index, definition.scope_id),
			)
		})
		.collect();
	values.sort();
	values
}

fn normalize_references(index: &foch_core::model::SemanticIndex) -> Vec<String> {
	let mut values: Vec<_> = index
		.references
		.iter()
		.map(|reference| {
			let mut provided = reference.provided_params.clone();
			provided.sort();
			let mut bindings: Vec<_> = reference
				.param_bindings
				.iter()
				.map(|binding| format!("{}={}", binding.name, binding.value))
				.collect();
			bindings.sort();
			format!(
				"{}|{}|{}|{}|{}|{}|{}|{}",
				reference.kind.as_str(),
				reference.name,
				reference.module,
				reference.mod_id,
				normalize_path(&reference.path),
				provided.join(","),
				bindings.join(","),
				scope_signature(index, reference.scope_id),
			)
		})
		.collect();
	values.sort();
	values
}

fn normalize_scope_aliases(index: &foch_core::model::SemanticIndex) -> Vec<String> {
	let mut values: Vec<_> = index
		.scopes
		.iter()
		.map(|scope| {
			let mut aliases: Vec<_> = scope
				.aliases
				.iter()
				.map(|(alias, alias_scope_type)| {
					format!("{}:{}", alias, scope_type(*alias_scope_type))
				})
				.collect();
			aliases.sort();
			let parent_kind = scope
				.parent
				.map(|parent| format!("{:?}", index.scopes[parent].kind))
				.unwrap_or_default();
			let parent_key = scope
				.parent
				.map(|parent| index.scopes[parent].key.clone())
				.unwrap_or_default();
			format!(
				"{:?}|{}|{}|{}|{}|{}|{}",
				scope.kind,
				normalize_path(&scope.path),
				scope.key,
				scope_type(scope.this_type),
				parent_kind,
				parent_key,
				aliases.join(","),
			)
		})
		.collect();
	values.sort();
	values
}

fn normalize_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn scope_signature(index: &foch_core::model::SemanticIndex, mut scope_id: usize) -> String {
	let mut parts = Vec::new();
	loop {
		let scope = &index.scopes[scope_id];
		let mut aliases: Vec<_> = scope
			.aliases
			.iter()
			.map(|(alias, alias_scope_type)| format!("{}:{}", alias, scope_type(*alias_scope_type)))
			.collect();
		aliases.sort();
		parts.push(format!(
			"{:?}:{}:{}:{}",
			scope.kind,
			normalize_path(&scope.path),
			scope.key,
			aliases.join(","),
		));
		let Some(parent) = scope.parent else {
			break;
		};
		scope_id = parent;
	}
	parts.reverse();
	parts.join(" -> ")
}

fn scope_type(scope_type: MaybeScope) -> &'static str {
	match scope_type {
		MaybeScope::Known(scope_type) => scope_type.name(),
		MaybeScope::Unknown => "unknown",
	}
}

fn scope_set(scope_set: ScopeSet) -> String {
	format!("{scope_set:?}")
}
