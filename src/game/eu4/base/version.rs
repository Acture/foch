use super::builtin::builtin_catalog_hash;
use crate::game::eu4::active_cwt_schema_id;
use crate::game::eu4::analysis::param_contracts::registered_param_contracts_hash;
use std::sync::OnceLock;

pub const ANALYSIS_RULES_VERSION: u32 = 25;

static ANALYSIS_RULES_ID: OnceLock<String> = OnceLock::new();

/// Identity of every rule set semantic analysis reads. Scope classification
/// comes from the active CWT schema, so the schema identity is part of it.
pub fn analysis_rules_version() -> &'static str {
	ANALYSIS_RULES_ID.get_or_init(|| {
		format!(
			"rules-v{}-catalog-{}-contracts-{}-cwt-{}",
			ANALYSIS_RULES_VERSION,
			builtin_catalog_hash(),
			registered_param_contracts_hash(),
			&active_cwt_schema_id()[..16]
		)
	})
}

#[cfg(test)]
mod tests {
	use super::analysis_rules_version;
	use crate::game::eu4::active_cwt_schema_id;
	use crate::game::eu4::analysis::param_contracts::registered_param_contracts_hash;

	#[test]
	fn analysis_rules_version_tracks_param_contract_registry() {
		assert!(
			analysis_rules_version().contains(registered_param_contracts_hash()),
			"analysis rules version should invalidate caches when param contracts change"
		);
	}

	#[test]
	fn analysis_rules_version_tracks_the_active_cwt_schema() {
		assert!(
			analysis_rules_version().ends_with(&format!("-cwt-{}", &active_cwt_schema_id()[..16])),
			"analysis rules version should invalidate caches when the CWT schema changes"
		);
	}
}
