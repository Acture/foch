use super::finding::{ValidatorFinding, ValidatorSeverity};
use crate::analyzer::parser::{AstStatement, AstValue};
use crate::cwt::{CwtRule, CwtRuleBody, CwtSchema, CwtValueType};

/// Check scalar assignments whose rule declares an `enum[...]` value type:
/// the value must be one of the schema enum's known values. Unknown enum
/// names are skipped (we cannot validate what the schema does not define).
pub fn check_enum_values(
	schema: &CwtSchema,
	rules: &[CwtRule],
	statements: &[AstStatement],
) -> Vec<ValidatorFinding> {
	let mut findings = Vec::new();
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let AstValue::Scalar {
			value: scalar,
			span,
		} = value
		else {
			continue;
		};
		let Some(rule) = rules.iter().find(|r| &r.key == key) else {
			continue;
		};
		let CwtRuleBody::Leaf(CwtValueType::Enum(enum_name)) = &rule.body else {
			continue;
		};
		let Some(cwt_enum) = schema.enums.iter().find(|e| &e.name == enum_name) else {
			continue;
		};
		let text = scalar.as_text();
		if !cwt_enum.values.iter().any(|v| v == &text) {
			findings.push(ValidatorFinding {
				rule_id: "invalid-enum-value".to_string(),
				severity: ValidatorSeverity::Error,
				message: format!(
					"value `{text}` for `{key}` is not in enum `{enum_name}` (allowed: {})",
					cwt_enum.values.join(", ")
				),
				line: span.start.line,
				column: span.start.column,
			});
		}
	}
	findings
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analyzer::parser::parse_clausewitz_content;
	use crate::cwt::{CwtEnum, CwtRule, CwtRuleBody, CwtSchema, CwtValueType};
	use std::path::PathBuf;

	fn rules() -> Vec<CwtRule> {
		vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Enum("power_categories".to_string())),
			cardinality: None,
			options: Vec::new(),
		}]
	}

	fn schema() -> CwtSchema {
		CwtSchema {
			enums: vec![CwtEnum {
				name: "power_categories".to_string(),
				values: vec!["ADM".to_string(), "DIP".to_string(), "MIL".to_string()],
			}],
			..Default::default()
		}
	}

	fn ast(src: &str) -> Vec<crate::analyzer::parser::AstStatement> {
		parse_clausewitz_content(PathBuf::from("t.txt"), src)
			.ast
			.statements
	}

	#[test]
	fn flags_value_outside_enum() {
		let findings = check_enum_values(&schema(), &rules(), &ast("category = ECO\n"));
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "invalid-enum-value");
		assert_eq!(findings[0].severity, ValidatorSeverity::Error);
	}

	#[test]
	fn accepts_value_in_enum() {
		assert!(check_enum_values(&schema(), &rules(), &ast("category = ADM\n")).is_empty());
	}

	#[test]
	fn ignores_unknown_enum_name() {
		let mut s = schema();
		s.enums.clear();
		assert!(check_enum_values(&s, &rules(), &ast("category = ECO\n")).is_empty());
	}
}
