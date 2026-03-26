use crate::check::eu4_builtin::builtin_catalog_hash;
use crate::check::param_contracts::registered_param_contracts_hash;
use std::sync::OnceLock;

pub const ANALYSIS_RULES_VERSION: u32 = 9;

static ANALYSIS_RULES_ID: OnceLock<String> = OnceLock::new();

pub fn analysis_rules_version() -> &'static str {
	ANALYSIS_RULES_ID.get_or_init(|| {
		format!(
			"rules-v{}-catalog-{}-contracts-{}",
			ANALYSIS_RULES_VERSION,
			builtin_catalog_hash(),
			registered_param_contracts_hash()
		)
	})
}

#[cfg(test)]
mod tests {
	use super::analysis_rules_version;
	use crate::check::param_contracts::registered_param_contracts_hash;

	#[test]
	fn analysis_rules_version_tracks_param_contract_registry() {
		assert!(
			analysis_rules_version().contains(registered_param_contracts_hash()),
			"analysis rules version should invalidate caches when param contracts change"
		);
	}
}
