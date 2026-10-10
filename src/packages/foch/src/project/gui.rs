//! Reviewed overrides for GUI action migration.
//!
//! Overrides are written as the source mod would have written them, in the
//! button's original context: under a province window `ROOT` is the clicked
//! province and `FROM` the clicking country. Foch rebinds them for the new
//! entry point like the mod's own script and text.

use serde::{Deserialize, Serialize};

use super::ConfigError;
use crate::game::eu4::script::parser::parse_clausewitz_content;

/// How migrated GUI actions are presented, for the whole playset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiMode {
	/// Every supported action through decisions and events.
	Decisions,
	/// Every supported action through Foch's own GUI. Not implemented yet.
	FochGui,
	/// Whichever verified representation preserves most. Not implemented yet.
	BestEffort,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiConfig {
	/// Migrate mods' GUI actions; without a mode the merge leaves them alone.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mode: Option<GuiMode>,
	/// Replacement text for localisation keys the migrated actions show.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub text: Vec<GuiTextOverride>,
	/// Replacement visibility conditions for province buttons, used where
	/// the button's own condition cannot be tested by the main decision.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub visibility: Vec<GuiVisibilityOverride>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiTextOverride {
	pub key: String,
	/// The language, such as `english`; every language when omitted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub language: Option<String>,
	pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiVisibilityOverride {
	#[serde(rename = "mod")]
	pub mod_id: String,
	pub button: String,
	/// Trigger statements in the button's original context, where `ROOT` is
	/// the clicked province and `FROM` the clicking country, such as
	/// `controller = { alliance_with = FROM }`. Writing the rebound form
	/// instead (`alliance_with = ROOT`) would name the province, not the
	/// player, without any error.
	pub trigger: String,
}

impl GuiConfig {
	pub fn is_empty(&self) -> bool {
		self.mode.is_none() && self.text.is_empty() && self.visibility.is_empty()
	}

	pub fn validate(&self) -> Result<(), ConfigError> {
		if let Some(mode @ (GuiMode::FochGui | GuiMode::BestEffort)) = self.mode {
			return Err(ConfigError::new(format!(
				"gui mode `{}` is not implemented yet; use `decisions`",
				match mode {
					GuiMode::FochGui => "foch_gui",
					_ => "best_effort",
				}
			)));
		}
		let mut texts = std::collections::BTreeSet::new();
		for entry in &self.text {
			if !is_identifier(&entry.key) {
				return Err(ConfigError::new(format!(
					"gui text override key `{}` is not a localisation key",
					entry.key
				)));
			}
			if let Some(language) = &entry.language
				&& !is_identifier(language)
			{
				return Err(ConfigError::new(format!(
					"gui text override for `{}` names invalid language `{language}`",
					entry.key
				)));
			}
			if !texts.insert((&entry.key, &entry.language)) {
				return Err(ConfigError::new(format!(
					"gui text override for `{}` is given twice",
					entry.key
				)));
			}
		}
		let mut buttons = std::collections::BTreeSet::new();
		for entry in &self.visibility {
			if entry.mod_id.is_empty() || !is_identifier(&entry.button) {
				return Err(ConfigError::new(format!(
					"gui visibility override needs a mod and a button name, got `{}` / `{}`",
					entry.mod_id, entry.button
				)));
			}
			if !buttons.insert((&entry.mod_id, &entry.button)) {
				return Err(ConfigError::new(format!(
					"gui visibility override for `{}` in mod `{}` is given twice",
					entry.button, entry.mod_id
				)));
			}
			let parsed = parse_clausewitz_content(
				crate::model::GamePath::new("gui_visibility.txt").expect("synthetic game path"),
				&entry.trigger,
			);
			if !parsed.diagnostics.is_empty() || parsed.ast.statements.is_empty() {
				return Err(ConfigError::new(format!(
					"gui visibility override for `{}` is not trigger script",
					entry.button
				)));
			}
		}
		Ok(())
	}
}

fn is_identifier(text: &str) -> bool {
	!text.is_empty()
		&& text
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
	use crate::project::Project;

	#[test]
	fn manifest_reads_text_and_visibility_overrides() {
		let project: Project = toml::from_str(
			r#"
[[gui.text]]
key = "transfer_occupation_tt"
text = "Take control of [Root.GetName] from an ally of [From.GetName]"

[[gui.text]]
key = "transfer_occupation_tt"
language = "german"
text = "Kontrolle über [Root.GetName] übernehmen"

[[gui.visibility]]
mod = "2164202838"
button = "transfer_occupation_from_ally_button"
trigger = "controller = { alliance_with = FROM }"
"#,
		)
		.unwrap();
		assert_eq!(project.gui.text.len(), 2);
		assert_eq!(project.gui.text[1].language.as_deref(), Some("german"));
		assert_eq!(
			project.gui.visibility[0].trigger,
			"controller = { alliance_with = FROM }"
		);
		let encoded = toml::to_string(&project).unwrap();
		assert_eq!(toml::from_str::<Project>(&encoded).unwrap(), project);
	}

	#[test]
	fn manifest_rejects_ambiguous_or_malformed_overrides() {
		let error = |text: &str| toml::from_str::<Project>(text).unwrap_err().to_string();
		assert!(
			error(
				"[[gui.text]]\nkey = \"a\"\ntext = \"x\"\n[[gui.text]]\nkey = \"a\"\ntext = \"y\"\n"
			)
			.contains("given twice")
		);
		assert!(
			error("[[gui.visibility]]\nmod = \"1\"\nbutton = \"b\"\ntrigger = \"a = {\"\n")
				.contains("not trigger script")
		);
		assert!(error("[gui]\ncolour = \"blue\"\n").contains("unknown field"));
		assert!(error("[gui]\nmode = \"foch_gui\"\n").contains("not implemented yet"));
		let project: Project = toml::from_str("[gui]\nmode = \"decisions\"\n").unwrap();
		assert_eq!(project.gui.mode, Some(super::GuiMode::Decisions));
	}
}
