use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_merge_kernel::{ChildOrder, SemanticKey};

pub(crate) trait ClausewitzTreePolicy {
	fn assignment_anchor(&self, key: &str, value: &AstValue) -> Option<SemanticKey>;

	fn assignment_signature(&self, _key: &str, _value: &AstValue) -> Option<String> {
		None
	}

	fn block_child_order(&self, _assignment_key: Option<&str>) -> ChildOrder {
		match _assignment_key {
			Some("OR") => ChildOrder::Commutative,
			_ => ChildOrder::Ordered,
		}
	}
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DefaultClausewitzTreePolicy;

impl ClausewitzTreePolicy for DefaultClausewitzTreePolicy {
	fn assignment_anchor(&self, key: &str, value: &AstValue) -> Option<SemanticKey> {
		match key {
			"country_event" | "province_event" => scalar_field(value, "id").map(|id| {
				SemanticKey::new("clausewitz.assignment.identity", format!("{key}:{id}"))
			}),
			"option" => scalar_field(value, "name").map(|name| {
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("option:{name}"),
				)
			}),
			"desc" | "triggered_desc" => scalar_field(value, "desc").map(|desc| {
				SemanticKey::parent_scoped(
					"clausewitz.assignment.identity",
					format!("{key}:{desc}"),
				)
			}),
			"if" | "else_if" | "else" => None,
			_ => Some(SemanticKey::parent_scoped("clausewitz.assignment.key", key)),
		}
	}

	fn assignment_signature(&self, key: &str, value: &AstValue) -> Option<String> {
		match key {
			"option" => scalar_field(value, "name").map(|name| format!("option:{name}")),
			"modifier" => descendant_scalar_field(value, "tooltip")
				.map(|tooltip| format!("modifier:tooltip:{tooltip}")),
			_ => None,
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

fn descendant_scalar_field(value: &AstValue, field: &str) -> Option<String> {
	let AstValue::Block { items, .. } = value else {
		return None;
	};
	items.iter().find_map(|statement| {
		let (key, value) = match statement {
			AstStatement::Assignment { key, value, .. } => (Some(key.as_str()), value),
			AstStatement::Item { value, .. } => (None, value),
			AstStatement::Comment { .. } => return None,
		};
		if key == Some(field)
			&& let AstValue::Scalar { value, .. } = value
		{
			return Some(value.as_text());
		}
		descendant_scalar_field(value, field)
	})
}
