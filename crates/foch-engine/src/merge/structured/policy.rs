use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_merge_kernel::{ChildOrder, SemanticKey};

pub(crate) trait ClausewitzTreePolicy {
	fn assignment_anchor(&self, key: &str, value: &AstValue) -> Option<SemanticKey>;

	fn block_child_order(&self, _assignment_key: Option<&str>) -> ChildOrder {
		ChildOrder::Ordered
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EventTreePolicy;

impl ClausewitzTreePolicy for EventTreePolicy {
	fn assignment_anchor(&self, key: &str, value: &AstValue) -> Option<SemanticKey> {
		match key {
			"country_event" | "province_event" => scalar_field(value, "id").map(|id| {
				SemanticKey::new("clausewitz.assignment.identity", format!("{key}:{id}"))
			}),
			"option" => scalar_field(value, "name").map(|name| {
				SemanticKey::new("clausewitz.assignment.identity", format!("option:{name}"))
			}),
			"if" | "else_if" | "else" | "after" | "desc" | "triggered_desc" => None,
			_ => Some(SemanticKey::new("clausewitz.assignment.key", key)),
		}
	}
}

fn scalar_field(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| {
		let AstStatement::Assignment { key, value, .. } = statement else {
			return None;
		};
		if key != field {
			return None;
		}
		let AstValue::Scalar { value, .. } = value else {
			return None;
		};
		Some(value.as_text())
	})
}
