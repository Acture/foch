use foch_core::model::SymbolKind;

/// Declares that definitions of `producer` kind are globally visible
/// and can be referenced from any content family.
pub struct GlobalVisibilityRule {
	pub kind: SymbolKind,
	/// Whether duplicates across mods should be flagged as errors
	pub flag_duplicates: bool,
	/// Whether unresolved references should be flagged as errors
	pub flag_unresolved: bool,
}

/// The visibility registry for EU4.
/// This is the single source of truth for which symbols are visible where.
pub fn eu4_global_visibility_rules() -> &'static [GlobalVisibilityRule] {
	static RULES: &[GlobalVisibilityRule] = &[
		GlobalVisibilityRule {
			kind: SymbolKind::ScriptedEffect,
			flag_duplicates: true,
			flag_unresolved: true,
		},
		GlobalVisibilityRule {
			kind: SymbolKind::ScriptedTrigger,
			flag_duplicates: true,
			flag_unresolved: true,
		},
		GlobalVisibilityRule {
			kind: SymbolKind::Event,
			flag_duplicates: true,
			flag_unresolved: true,
		},
		GlobalVisibilityRule {
			kind: SymbolKind::Decision,
			flag_duplicates: false,
			flag_unresolved: false,
		},
		GlobalVisibilityRule {
			kind: SymbolKind::DiplomaticAction,
			flag_duplicates: false,
			flag_unresolved: false,
		},
		GlobalVisibilityRule {
			kind: SymbolKind::TriggeredModifier,
			flag_duplicates: false,
			flag_unresolved: false,
		},
	];
	RULES
}

/// Check if a symbol kind should have its duplicates flagged.
pub fn should_flag_duplicates(kind: SymbolKind) -> bool {
	eu4_global_visibility_rules()
		.iter()
		.any(|r| r.kind == kind && r.flag_duplicates)
}

/// Check if a symbol kind should have its unresolved references flagged.
pub fn should_flag_unresolved(kind: SymbolKind) -> bool {
	eu4_global_visibility_rules()
		.iter()
		.any(|r| r.kind == kind && r.flag_unresolved)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn events_and_scripted_effects_and_triggers_flag_duplicates() {
		assert!(should_flag_duplicates(SymbolKind::Event));
		assert!(should_flag_duplicates(SymbolKind::ScriptedEffect));
		assert!(should_flag_duplicates(SymbolKind::ScriptedTrigger));
	}

	#[test]
	fn decisions_do_not_flag_duplicates() {
		assert!(!should_flag_duplicates(SymbolKind::Decision));
	}

	#[test]
	fn events_and_scripted_effects_and_triggers_flag_unresolved() {
		assert!(should_flag_unresolved(SymbolKind::Event));
		assert!(should_flag_unresolved(SymbolKind::ScriptedEffect));
		assert!(should_flag_unresolved(SymbolKind::ScriptedTrigger));
	}
}
