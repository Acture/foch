use super::finding::{ValidatorFinding, ValidatorSeverity};
use crate::analyzer::parser::AstStatement;
use crate::cwt::{CwtRule, CwtRuleBody, CwtValueType};
use std::collections::BTreeSet;

/// Options controlling the false-positive-prone unknown-key check.
#[derive(Clone, Debug, Default)]
pub struct UnknownKeyOptions {
	/// Off by default; opt in only where the rule set is trusted.
	pub enabled: bool,
	/// Keys to never flag (dynamic / engine-handled).
	pub suppressed: BTreeSet<String>,
}

/// Flag assignment keys not present in the rule set. Conservative:
/// - disabled unless `options.enabled`;
/// - skipped for suppressed keys;
/// - entirely disabled for blocks whose rules accept open-ended aliased keys
///   (`alias_name[...]` / `alias_match_left[...]`), which cannot be enumerated.
pub fn check_unknown_keys(
	rules: &[CwtRule],
	statements: &[AstStatement],
	options: &UnknownKeyOptions,
) -> Vec<ValidatorFinding> {
	if !options.enabled || block_accepts_open_keys(rules) {
		return Vec::new();
	}
	let known: BTreeSet<&str> = rules.iter().map(|r| r.key.as_str()).collect();
	let mut findings = Vec::new();
	for statement in statements {
		let AstStatement::Assignment { key, key_span, .. } = statement else {
			continue;
		};
		if known.contains(key.as_str()) || options.suppressed.contains(key) {
			continue;
		}
		findings.push(ValidatorFinding {
			rule_id: "unknown-key".to_string(),
			severity: ValidatorSeverity::Warning,
			message: format!("unknown key `{key}` is not declared in the schema for this block"),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}
	findings
}

fn block_accepts_open_keys(rules: &[CwtRule]) -> bool {
	rules.iter().any(|r| {
		// open-ended KEY pattern: <type>, scalar, enum[x], value[x], value_set[x],
		// scope[x], alias_name[x], ... — anything from_token does not classify as a
		// plain literal means the block accepts dynamic keys we cannot enumerate.
		!matches!(CwtValueType::from_token(&r.key), CwtValueType::Literal(_))
			// or a body that accepts open-ended aliased keys
			|| matches!(
				&r.body,
				CwtRuleBody::Leaf(CwtValueType::AliasName(_))
					| CwtRuleBody::Leaf(CwtValueType::AliasMatchLeft(_))
			)
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analyzer::parser::parse_clausewitz_content;
	use crate::cwt::{CwtRule, CwtRuleBody, CwtValueType};
	use std::collections::BTreeSet;
	use std::path::PathBuf;

	fn rule(key: &str, vt: CwtValueType) -> CwtRule {
		CwtRule {
			key: key.to_string(),
			body: CwtRuleBody::Leaf(vt),
			cardinality: None,
			options: Vec::new(),
		}
	}
	fn ast(src: &str) -> Vec<crate::analyzer::parser::AstStatement> {
		parse_clausewitz_content(PathBuf::from("t.txt"), src)
			.ast
			.statements
	}

	#[test]
	fn flags_unknown_key_when_enabled() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions {
			enabled: true,
			suppressed: BTreeSet::new(),
		};
		let findings = check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts);
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unknown-key");
		assert_eq!(findings[0].severity, ValidatorSeverity::Warning);
	}
	#[test]
	fn known_key_is_clean() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions {
			enabled: true,
			suppressed: BTreeSet::new(),
		};
		assert!(check_unknown_keys(&rules, &ast("category = ADM\n"), &opts).is_empty());
	}
	#[test]
	fn disabled_by_default() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions {
			enabled: false,
			suppressed: BTreeSet::new(),
		};
		assert!(check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts).is_empty());
	}
	#[test]
	fn suppressed_key_is_skipped() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let mut suppressed = BTreeSet::new();
		suppressed.insert("bogus".to_string());
		let opts = UnknownKeyOptions {
			enabled: true,
			suppressed,
		};
		assert!(check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts).is_empty());
	}
	#[test]
	fn alias_accepting_block_disables_check() {
		let rules = vec![rule(
			"trigger",
			CwtValueType::AliasName("trigger".to_string()),
		)];
		let opts = UnknownKeyOptions {
			enabled: true,
			suppressed: BTreeSet::new(),
		};
		assert!(check_unknown_keys(&rules, &ast("anything = 1\n"), &opts).is_empty());
	}
	#[test]
	fn open_ended_key_pattern_disables_check() {
		// a block whose rule KEY is a type-ref pattern accepts dynamic keys;
		// unknown-key must not fire even when enabled.
		let rules = vec![rule("<event_target>", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions {
			enabled: true,
			suppressed: BTreeSet::new(),
		};
		assert!(check_unknown_keys(&rules, &ast("some_target = 1\n"), &opts).is_empty());
	}
}
