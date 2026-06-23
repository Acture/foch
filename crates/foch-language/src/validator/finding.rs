/// Severity tier for a validator finding. Ordering is meaningful:
/// `Error > Warning > Info`, so callers can sort/filter by minimum severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ValidatorSeverity {
	Info,
	Warning,
	Error,
}

/// A single schema-conformance finding produced by the validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorFinding {
	pub rule_id: String,
	pub severity: ValidatorSeverity,
	pub message: String,
	pub line: usize,
	pub column: usize,
}

impl ValidatorFinding {
	/// Stable sort key: position first, so findings read top-to-bottom.
	pub fn sort_key(&self) -> (usize, usize, String) {
		(self.line, self.column, self.rule_id.clone())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn finding_orders_by_severity_then_position() {
		let err = ValidatorFinding {
			rule_id: "invalid-enum-value".to_string(),
			severity: ValidatorSeverity::Error,
			message: "x".to_string(),
			line: 5,
			column: 1,
		};
		let warn = ValidatorFinding {
			rule_id: "unknown-key".to_string(),
			severity: ValidatorSeverity::Warning,
			message: "y".to_string(),
			line: 2,
			column: 1,
		};
		assert!(ValidatorSeverity::Error > ValidatorSeverity::Warning);
		assert!(ValidatorSeverity::Warning > ValidatorSeverity::Info);
		// sort_key orders by (line, column, rule_id): warn (line 2) precedes err (line 5)
		assert!(warn.sort_key() < err.sort_key());
	}
}
