use foch_core::model::{SemanticIndex, SymbolDefinition, SymbolKind};
use std::collections::{HashMap, HashSet};

/// Source of vanilla semantic definitions for a workspace-like value.
///
/// `foch-language` cannot depend on the engine crate that owns
/// `ResolvedWorkspace`, so the engine implements this trait for its workspace
/// type and the indexer stays on the analyzer side of the crate boundary.
pub trait VanillaSymbolSource {
	fn visit_vanilla_definitions(&self, visit: &mut dyn FnMut(&SymbolDefinition));
}

impl VanillaSymbolSource for SemanticIndex {
	fn visit_vanilla_definitions(&self, visit: &mut dyn FnMut(&SymbolDefinition)) {
		for definition in &self.definitions {
			visit(definition);
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct VanillaSymbolIndex {
	by_kind: HashMap<SymbolKind, HashSet<String>>,
}

impl VanillaSymbolIndex {
	pub fn build(workspace: &impl VanillaSymbolSource) -> Self {
		let mut index = Self::default();
		workspace.visit_vanilla_definitions(&mut |definition| {
			index.insert_definition(definition);
		});
		index
	}

	pub fn from_semantic_index(index: &SemanticIndex) -> Self {
		Self::build(index)
	}

	pub fn contains(&self, kind: SymbolKind, name: &str) -> bool {
		self.by_kind
			.get(&kind)
			.is_some_and(|symbols| symbols.contains(name))
	}

	pub fn kinds_resolving(&self, name: &str) -> Vec<SymbolKind> {
		let mut kinds = self
			.by_kind
			.iter()
			.filter_map(|(kind, symbols)| symbols.contains(name).then_some(*kind))
			.collect::<Vec<_>>();
		kinds.sort_by_key(|kind| symbol_kind_order(*kind));
		kinds
	}

	fn insert_definition(&mut self, definition: &SymbolDefinition) {
		self.insert(definition.kind, definition.name.as_str());
		if should_index_local_name(definition) {
			self.insert(definition.kind, definition.local_name.as_str());
		}
	}

	fn insert(&mut self, kind: SymbolKind, name: &str) {
		if name.is_empty() {
			return;
		}
		self.by_kind
			.entry(kind)
			.or_default()
			.insert(name.to_string());
	}
}

fn should_index_local_name(definition: &SymbolDefinition) -> bool {
	definition.kind != SymbolKind::Event
		&& !definition.local_name.is_empty()
		&& definition.local_name != definition.name
}

fn symbol_kind_order(kind: SymbolKind) -> u8 {
	match kind {
		SymbolKind::ScriptedEffect => 0,
		SymbolKind::ScriptedTrigger => 1,
		SymbolKind::Event => 2,
		SymbolKind::Decision => 3,
		SymbolKind::DiplomaticAction => 4,
		SymbolKind::TriggeredModifier => 5,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::model::ScopeType;
	use std::path::PathBuf;

	struct TestWorkspace {
		base: Option<SemanticIndex>,
	}

	impl VanillaSymbolSource for TestWorkspace {
		fn visit_vanilla_definitions(&self, visit: &mut dyn FnMut(&SymbolDefinition)) {
			let Some(base) = self.base.as_ref() else {
				return;
			};
			for definition in &base.definitions {
				visit(definition);
			}
		}
	}

	fn definition(kind: SymbolKind, name: &str, local_name: &str) -> SymbolDefinition {
		SymbolDefinition {
			kind,
			name: name.to_string(),
			module: "vanilla".to_string(),
			local_name: local_name.to_string(),
			mod_id: "__game__eu4".to_string(),
			path: PathBuf::from("common/vanilla.txt"),
			line: 1,
			column: 1,
			scope_id: 0,
			declared_this_type: ScopeType::Unknown,
			inferred_this_type: ScopeType::Unknown,
			inferred_this_mask: 0,
			inferred_from_mask: 0,
			inferred_root_mask: 0,
			required_params: Vec::new(),
			optional_params: Vec::new(),
			param_contract: None,
			scope_param_names: Vec::new(),
		}
	}

	fn semantic_index(definitions: Vec<SymbolDefinition>) -> SemanticIndex {
		SemanticIndex {
			definitions,
			..Default::default()
		}
	}

	#[test]
	fn vanilla_symbol_index_groups_by_kind() {
		let workspace = TestWorkspace {
			base: Some(semantic_index(vec![
				definition(SymbolKind::ScriptedEffect, "eu4::effects::shared", "shared"),
				definition(
					SymbolKind::ScriptedTrigger,
					"eu4::triggers::shared",
					"shared",
				),
			])),
		};

		let index = VanillaSymbolIndex::build(&workspace);

		assert!(index.contains(SymbolKind::ScriptedEffect, "shared"));
		assert!(index.contains(SymbolKind::ScriptedTrigger, "shared"));
		assert!(!index.contains(SymbolKind::Event, "shared"));
	}

	#[test]
	fn vanilla_symbol_index_contains_known_vanilla_symbol() {
		let workspace = TestWorkspace {
			base: Some(semantic_index(vec![definition(
				SymbolKind::ScriptedEffect,
				"eu4::common.scripted_effects::vanilla_effect",
				"vanilla_effect",
			)])),
		};

		let index = VanillaSymbolIndex::build(&workspace);

		assert!(index.contains(SymbolKind::ScriptedEffect, "vanilla_effect"));
		assert!(index.contains(
			SymbolKind::ScriptedEffect,
			"eu4::common.scripted_effects::vanilla_effect"
		));
	}

	#[test]
	fn vanilla_symbol_index_returns_empty_when_workspace_has_no_base() {
		let workspace = TestWorkspace { base: None };

		let index = VanillaSymbolIndex::build(&workspace);

		assert!(!index.contains(SymbolKind::ScriptedEffect, "vanilla_effect"));
		assert!(index.kinds_resolving("vanilla_effect").is_empty());
	}

	#[test]
	fn vanilla_symbol_index_kinds_resolving_finds_cross_kind_collisions() {
		let workspace = TestWorkspace {
			base: Some(semantic_index(vec![
				definition(SymbolKind::ScriptedEffect, "eu4::effects::shared", "shared"),
				definition(SymbolKind::Decision, "eu4::decisions::shared", "shared"),
				definition(
					SymbolKind::TriggeredModifier,
					"eu4::modifiers::other",
					"other",
				),
			])),
		};

		let index = VanillaSymbolIndex::build(&workspace);

		assert_eq!(
			index.kinds_resolving("shared"),
			vec![SymbolKind::ScriptedEffect, SymbolKind::Decision]
		);
	}
}
