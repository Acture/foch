use crate::check::eu4_builtin::builtin_catalog_hash;
use std::sync::OnceLock;

pub const ANALYSIS_RULES_VERSION: u32 = 6;

static ANALYSIS_RULES_ID: OnceLock<String> = OnceLock::new();

pub fn analysis_rules_version() -> &'static str {
	ANALYSIS_RULES_ID.get_or_init(|| {
		format!(
			"rules-v{}-catalog-{}",
			ANALYSIS_RULES_VERSION,
			builtin_catalog_hash()
		)
	})
}
