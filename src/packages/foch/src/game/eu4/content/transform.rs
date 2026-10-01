//! Content-family selection for verified identity and reference transformations.

use std::collections::HashMap;
use std::sync::Arc;

use super::{ContentFamilyDescriptor, ContentFamilyPathMatcher, MergePolicies};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::parser::AstFile;
use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::transform::TransformAdapter;
use crate::merge::transform::tree::EntityTransform;

impl ContentFamilyDescriptor {
	pub(crate) fn transform_adapter(&self) -> Option<&'static dyn TransformAdapter> {
		match self.matcher {
			ContentFamilyPathMatcher::Prefix(prefix) if prefix.as_str() == "common/cultures" => {
				Some(&crate::game::eu4::cultures::adapt::CultureAdapter)
			}
			_ => None,
		}
	}

	pub(crate) fn infer_entity_transform(
		&self,
		base: &AstFile,
		revisions: &[&AstFile],
	) -> Result<Option<Arc<dyn EntityTransform>>, String> {
		match self.transform_adapter() {
			Some(adapter) => adapter.infer_entity_transform(base, revisions),
			None => Ok(None),
		}
	}

	pub(crate) fn infer_dag_transform(
		&self,
		file_dag: &FileDag,
		vanilla: Option<&ParsedScriptFile>,
		contributors: &HashMap<ModId, ParsedScriptFile>,
		policies: &MergePolicies,
	) -> Result<Option<Arc<dyn EntityTransform>>, String> {
		match self.transform_adapter() {
			Some(adapter) => adapter.infer_dag_transform(file_dag, vanilla, contributors, policies),
			None => Ok(None),
		}
	}
}
