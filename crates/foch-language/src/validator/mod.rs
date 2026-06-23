mod enum_check;
mod finding;
mod unknown_key_check;

pub use enum_check::check_enum_values;
pub use finding::{ValidatorFinding, ValidatorSeverity};
pub use unknown_key_check::{UnknownKeyOptions, check_unknown_keys};

use crate::analyzer::parser::AstStatement;
use crate::cwt::{CwtRule, CwtSchema};
use std::collections::BTreeSet;

/// Top-level validation options for a block.
#[derive(Clone, Debug, Default)]
pub struct ValidationOptions {
	/// Opt-in for the false-positive-prone unknown-key check.
	pub check_unknown_keys: bool,
	/// Keys the unknown-key check must never flag.
	pub suppressed_keys: BTreeSet<String>,
}

/// Validate the statements of one block against its cwt rule set, returning
/// findings sorted by position then rule id. Runs the enum-value check
/// (always) and the unknown-key check (gated by options).
pub fn validate_block(
	schema: &CwtSchema,
	rules: &[CwtRule],
	statements: &[AstStatement],
	options: &ValidationOptions,
) -> Vec<ValidatorFinding> {
	let mut findings = check_enum_values(schema, rules, statements);
	let unknown_options = UnknownKeyOptions {
		enabled: options.check_unknown_keys,
		suppressed: options.suppressed_keys.clone(),
	};
	findings.extend(check_unknown_keys(rules, statements, &unknown_options));
	findings.sort_by_key(|f| f.sort_key());
	findings
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analyzer::parser::parse_clausewitz_content;
	use crate::cwt::{CwtEnum, CwtRule, CwtRuleBody, CwtSchema, CwtValueType};
	use std::path::PathBuf;

	#[test]
	fn validate_block_runs_both_checks_sorted() {
		let schema = CwtSchema {
			enums: vec![CwtEnum {
				name: "cat".to_string(),
				values: vec!["ADM".to_string()],
			}],
			..Default::default()
		};
		let rules = vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Enum("cat".to_string())),
			cardinality: None,
			options: Vec::new(),
		}];
		let src = "bogus = 1\ncategory = ECO\n";
		let ast = parse_clausewitz_content(PathBuf::from("t.txt"), src)
			.ast
			.statements;
		let options = ValidationOptions {
			check_unknown_keys: true,
			..Default::default()
		};
		let findings = validate_block(&schema, &rules, &ast, &options);
		assert_eq!(findings.len(), 2);
		assert_eq!(findings[0].rule_id, "unknown-key");
		assert_eq!(findings[1].rule_id, "invalid-enum-value");
	}

	#[test]
	fn unknown_keys_off_by_default() {
		let schema = CwtSchema::default();
		let rules = vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Scalar),
			cardinality: None,
			options: Vec::new(),
		}];
		let ast = parse_clausewitz_content(PathBuf::from("t.txt"), "bogus = 1\n")
			.ast
			.statements;
		let findings = validate_block(&schema, &rules, &ast, &ValidationOptions::default());
		assert!(findings.is_empty());
	}
}
