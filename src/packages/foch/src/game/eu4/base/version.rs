use super::builtin::builtin_catalog_hash;
use crate::DATA_VERSION;
use crate::game::eu4::active_cwt_schema_id;
use std::sync::OnceLock;

static ANALYSIS_RULES_ID: OnceLock<String> = OnceLock::new();

/// Identity of every rule set semantic analysis reads. Scope classification
/// comes from the active CWT schema, so the schema identity is part of it; a
/// change to the analysis itself raises `DATA_VERSION`.
pub fn analysis_rules_version() -> &'static str {
	ANALYSIS_RULES_ID.get_or_init(|| {
		format!(
			"rules-v{}-catalog-{}-cwt-{}",
			DATA_VERSION,
			builtin_catalog_hash(),
			&active_cwt_schema_id()[..16]
		)
	})
}

#[cfg(test)]
mod tests {
	use super::analysis_rules_version;
	use crate::DATA_VERSION;
	use crate::game::eu4::active_cwt_schema_id;

	#[test]
	fn analysis_rules_version_tracks_the_data_version() {
		assert!(
			analysis_rules_version().starts_with(&format!("rules-v{DATA_VERSION}-")),
			"analysis rules version should invalidate caches when the data version changes"
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
