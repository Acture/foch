//! Entity identity supplied by a content adapter to the shared tree merge.

use crate::model::GamePath;
use std::fmt::Debug;

use crate::game::eu4::script::parser::{AstFile, AstValue};
use crate::merge::kernel::SemanticKey;

/// A verified correspondence preserves authored labels while matching entities
/// across renames and moves. Content-specific hierarchy and collision rules stay
/// with the adapter that establishes the correspondence.
pub(crate) trait EntityTransform: Debug + Send + Sync {
	fn applies_to(&self, path: &GamePath) -> bool;

	fn entity_identity(
		&self,
		ancestors: &[String],
		key: &str,
		value: &AstValue,
	) -> Option<SemanticKey>;

	fn validate(&self, file: &AstFile) -> Result<(), String>;
}
