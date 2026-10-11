//! Extract scripted GUI features with the context they need to run.
//!
//! One extraction serves every presentation route: a decision/event route
//! and a Foch GUI route both consume the same [`ExtractedFeature`]. The
//! result is static: it says how each object is bound when the feature runs,
//! not which runtime objects a particular game will have.

pub(crate) mod adapt;
mod context;
pub mod decision;
pub mod geography;
pub mod host;
mod province;
pub mod widget;

#[cfg(test)]
mod tests;

use crate::model::GamePathBuf;
use std::collections::BTreeMap;

use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue};
pub use context::{Binding, ScopeUse, ScriptCall, ScriptKind, ScriptSource, Stage};
use context::{ContextWalk, Rewrite, ScriptDefinitions};
pub use decision::{GeneratedDecision, GuiOverrides, RouteContext, SourceMod};
pub use host::{HostRoot, documented_host};
pub use widget::{WidgetNode, WidgetPath, named_widgets};

/// The effective documents an extraction reads. The caller resolves playset
/// order and dependencies; each slice holds the winning documents only, with
/// paths relative to the mod root.
#[derive(Clone, Copy)]
pub struct GuiSources<'a> {
	pub interface: &'a [AstFile],
	pub custom_gui: &'a [AstFile],
	pub scripted_effects: &'a [ScriptSource],
	pub scripted_triggers: &'a [ScriptSource],
}

/// The documented engine window a feature runs under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostBinding {
	pub window: String,
	pub root: HostRoot,
}

/// A gameplay gate in the order the game evaluates it: every scripted
/// ancestor's `potential`, outermost first, then the widget's own
/// `potential` and `trigger`.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureCondition {
	pub widget: String,
	pub stage: Stage,
	pub statements: Vec<AstStatement>,
}

/// One scripted action together with everything it needs to run elsewhere.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedFeature {
	pub widget: WidgetPath,
	pub definition_path: GamePathBuf,
	pub host: HostBinding,
	pub conditions: Vec<FeatureCondition>,
	pub effect: Vec<AstStatement>,
	/// The custom_gui `tooltip` localisation key.
	pub tooltip: Option<String>,
	pub calls: Vec<ScriptCall>,
	pub scope_uses: Vec<ScopeUse>,
	/// Context or dependencies the extraction could not establish. A feature
	/// with any entry here is not safe to move by any route.
	pub unresolved: Vec<String>,
}

impl ExtractedFeature {
	pub fn is_complete(&self) -> bool {
		self.unresolved.is_empty()
	}

	/// Every object the scope reference `token` denotes, in walk order.
	pub fn bindings_of(&self, token: &str) -> Vec<&Binding> {
		self.scope_uses
			.iter()
			.filter(|usage| usage.token.eq_ignore_ascii_case(token))
			.map(|usage| &usage.binding)
			.collect()
	}
}

struct CustomGuiDefinition {
	kind: String,
	path: GamePathBuf,
	items: Vec<AstStatement>,
}

/// Scripted widgets and custom GUI definitions of one effective playset.
pub struct FeatureCatalog {
	widgets: Vec<WidgetPath>,
	definitions: BTreeMap<String, Vec<CustomGuiDefinition>>,
	order: Vec<String>,
	scripts: ScriptDefinitions,
}

impl FeatureCatalog {
	pub fn new(sources: GuiSources<'_>) -> Self {
		let widgets = sources
			.interface
			.iter()
			.flat_map(|file| named_widgets(file.path.as_str(), file))
			.collect();
		let mut definitions: BTreeMap<String, Vec<CustomGuiDefinition>> = BTreeMap::new();
		let mut order = Vec::new();
		for file in sources.custom_gui {
			for statement in &file.statements {
				let AstStatement::Assignment {
					key,
					value: AstValue::Block { items, .. },
					..
				} = statement
				else {
					continue;
				};
				let Some(name) = widget::scalar(items, "name") else {
					continue;
				};
				if !definitions.contains_key(&name) {
					order.push(name.clone());
				}
				definitions
					.entry(name)
					.or_default()
					.push(CustomGuiDefinition {
						kind: key.clone(),
						path: file.path.clone(),
						items: items.clone(),
					});
			}
		}
		Self {
			widgets,
			definitions,
			order,
			scripts: ScriptDefinitions::new(sources.scripted_effects, sources.scripted_triggers),
		}
	}

	/// Every `custom_button` action, in definition order, with its extraction
	/// or the reason it cannot be extracted.
	pub fn actions(&self) -> Vec<(String, Result<ExtractedFeature, String>)> {
		self.order
			.iter()
			.filter(|name| {
				self.definitions[*name]
					.iter()
					.any(|definition| definition.kind == "custom_button")
			})
			.map(|name| (name.clone(), self.extract(name)))
			.collect()
	}

	pub fn extract(&self, name: &str) -> Result<ExtractedFeature, String> {
		let definition = self.single_definition(name)?;
		if definition.kind != "custom_button" {
			return Err(format!(
				"`{name}` is a `{}`; only `custom_button` actions are extracted",
				definition.kind
			));
		}
		let widget = self.single_widget(name)?;
		let (window, root) = widget
			.ancestry
			.iter()
			.rev()
			.find_map(|node| {
				documented_host(&widget.gui_file, &node.name).map(|root| (node.name.clone(), root))
			})
			.ok_or_else(|| {
				format!(
					"`{name}` in `{}` is not under a documented host window",
					widget.gui_file
				)
			})?;
		let mut unresolved = Vec::new();
		let mut conditions = Vec::new();
		for ancestor in &widget.ancestry[..widget.ancestry.len() - 1] {
			if !ancestor.scripted {
				continue;
			}
			match self.single_definition(&ancestor.name) {
				Ok(parent) => {
					if let Some(potential) = block(&parent.items, "potential") {
						conditions.push(FeatureCondition {
							widget: ancestor.name.clone(),
							stage: Stage::Potential,
							statements: potential,
						});
					}
				}
				Err(reason) => unresolved.push(format!("scripted ancestor: {reason}")),
			}
		}
		for (key, stage) in [("potential", Stage::Potential), ("trigger", Stage::Trigger)] {
			if let Some(statements) = block(&definition.items, key) {
				conditions.push(FeatureCondition {
					widget: name.to_owned(),
					stage,
					statements,
				});
			}
		}
		let effect = block(&definition.items, "effect").unwrap_or_default();

		let root_binding = match root {
			HostRoot::Actor => Binding::Actor,
			other => Binding::Target(other),
		};
		let mut walk = ContextWalk::new(&self.scripts, root_binding);
		for condition in &conditions {
			walk.walk(condition.stage, &condition.widget, &condition.statements);
		}
		walk.walk(Stage::Effect, name, &effect);
		unresolved.extend(walk.unresolved);
		Ok(ExtractedFeature {
			widget: widget.clone(),
			definition_path: definition.path.clone(),
			host: HostBinding { window, root },
			conditions,
			effect,
			tooltip: widget::scalar(&definition.items, "tooltip"),
			calls: walk.calls,
			scope_uses: walk.uses,
			unresolved,
		})
	}

	/// Rebinds an action's `ROOT`/`FROM` for a decision or event chain whose
	/// `ROOT` is the clicking country, with a host target saved as `target`.
	fn rewrite<'a>(&'a self, target: Option<&'a str>) -> Rewrite<'a> {
		Rewrite {
			definitions: &self.scripts,
			target,
			texts: None,
			instances: &[],
		}
	}

	/// [`Self::rewrite`] that also rebinds the localisation keys it shows.
	fn rewrite_text<'a>(
		&'a self,
		target: Option<&'a str>,
		instances: &'a [String],
		texts: &'a context::Localise<'a>,
	) -> Rewrite<'a> {
		Rewrite {
			definitions: &self.scripts,
			target,
			texts: Some(texts),
			instances,
		}
	}

	fn single_definition(&self, name: &str) -> Result<&CustomGuiDefinition, String> {
		match self.definitions.get(name).map(Vec::as_slice) {
			Some([definition]) => Ok(definition),
			Some(many) => Err(format!(
				"`{name}` has {} custom_gui definitions: {}",
				many.len(),
				many.iter()
					.map(|definition| definition.path.to_string())
					.collect::<Vec<_>>()
					.join(", ")
			)),
			None => Err(format!("`{name}` has no custom_gui definition")),
		}
	}

	fn single_widget(&self, name: &str) -> Result<&WidgetPath, String> {
		let matches = self
			.widgets
			.iter()
			.filter(|path| path.leaf().scripted && path.leaf().name == name)
			.collect::<Vec<_>>();
		match matches.as_slice() {
			[widget] => Ok(widget),
			[] => Err(format!("`{name}` has no scripted widget")),
			many => Err(format!(
				"`{name}` matches {} scripted widgets: {}",
				many.len(),
				many.iter()
					.map(|path| path.gui_file.as_str())
					.collect::<Vec<_>>()
					.join(", ")
			)),
		}
	}
}

fn block(items: &[AstStatement], key: &str) -> Option<Vec<AstStatement>> {
	items.iter().find_map(|statement| match statement {
		AstStatement::Assignment {
			key: found,
			value: AstValue::Block { items, .. },
			..
		} if found == key => Some(items.clone()),
		_ => None,
	})
}
