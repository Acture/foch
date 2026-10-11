//! Named widget ancestry inside one `.gui` document.

use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue};

/// One named GUI object, such as a `windowType` or `guiButtonType`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WidgetNode {
	pub kind: String,
	pub name: String,
	pub scripted: bool,
	/// The `buttonText` localisation key, when the widget has one.
	pub text: Option<String>,
}

/// A widget and every named ancestor, outermost first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WidgetPath {
	pub gui_file: String,
	pub ancestry: Vec<WidgetNode>,
}

impl WidgetPath {
	pub fn leaf(&self) -> &WidgetNode {
		self.ancestry
			.last()
			.expect("a widget path holds at least its widget")
	}
}

/// Every named widget in the document. Unnamed blocks, such as `position`,
/// are traversed but are not part of the ancestry.
pub fn named_widgets(gui_file: &str, ast: &AstFile) -> Vec<WidgetPath> {
	let mut found = Vec::new();
	let mut ancestry = Vec::new();
	collect(gui_file, &ast.statements, &mut ancestry, &mut found);
	found
}

fn collect(
	gui_file: &str,
	statements: &[AstStatement],
	ancestry: &mut Vec<WidgetNode>,
	found: &mut Vec<WidgetPath>,
) {
	for statement in statements {
		let AstStatement::Assignment {
			key,
			value: AstValue::Block { items, .. },
			..
		} = statement
		else {
			continue;
		};
		let name = scalar(items, "name");
		if let Some(name) = &name {
			ancestry.push(WidgetNode {
				kind: key.clone(),
				name: name.clone(),
				scripted: scalar(items, "scripted").is_some_and(|value| value == "yes"),
				text: scalar(items, "buttonText").filter(|text| !text.is_empty()),
			});
			found.push(WidgetPath {
				gui_file: gui_file.to_owned(),
				ancestry: ancestry.clone(),
			});
		}
		collect(gui_file, items, ancestry, found);
		if name.is_some() {
			ancestry.pop();
		}
	}
}

pub(super) fn scalar(items: &[AstStatement], key: &str) -> Option<String> {
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key: found,
			value: AstValue::Scalar { value, .. },
			..
		} if found == key => Some(value.as_text()),
		_ => None,
	})
}
