//! Reviewed culture identities and exact edits bound to original source bytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ConfigError, SourceEdit};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CultureConfig {
	#[serde(default)]
	pub renames: Vec<CultureRenameEntry>,
	#[serde(default)]
	pub repairs: Vec<CultureRepairEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CultureRenameEntry {
	pub from: String,
	pub to: String,
	#[serde(rename = "mod")]
	pub mod_id: String,
	pub file: String,
	pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CultureRepairEntry {
	#[serde(rename = "mod")]
	pub mod_id: String,
	pub file: String,
	pub sha256: String,
	pub edits: Vec<SourceEdit>,
}

impl CultureConfig {
	pub fn is_empty(&self) -> bool {
		self.renames.is_empty() && self.repairs.is_empty()
	}

	pub fn validate(&self) -> Result<(), ConfigError> {
		let mut sources = BTreeMap::new();
		let mut mappings = BTreeMap::new();
		let mut targets = BTreeMap::new();
		let mut reviewed = BTreeMap::new();
		for entry in &self.renames {
			validate_source(&entry.mod_id, &entry.file, &entry.sha256, &mut sources)?;
			for id in [&entry.from, &entry.to] {
				if id.is_empty()
					|| !id
						.bytes()
						.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
				{
					return Err(invalid(format!("invalid culture identity `{id}`")));
				}
			}
			if entry.from == entry.to {
				return Err(invalid("a culture rename must change its identity"));
			}
			if reviewed
				.insert((&entry.mod_id, &entry.from), &entry.to)
				.is_some()
			{
				return Err(invalid(format!(
					"duplicate or conflicting rename for {}:{}",
					entry.mod_id, entry.from
				)));
			}
			if mappings
				.insert(&entry.from, &entry.to)
				.is_some_and(|previous| previous != &entry.to)
			{
				return Err(invalid(format!(
					"conflicting targets for culture `{}`",
					entry.from
				)));
			}
			if targets
				.insert(&entry.to, &entry.from)
				.is_some_and(|previous| previous != &entry.from)
			{
				return Err(invalid(format!("multiple cultures map to `{}`", entry.to)));
			}
		}
		let mut repaired = BTreeMap::new();
		for entry in &self.repairs {
			validate_source(&entry.mod_id, &entry.file, &entry.sha256, &mut sources)?;
			if repaired.insert((&entry.mod_id, &entry.file), ()).is_some() {
				return Err(invalid(format!(
					"duplicate repair entry for {}:{}",
					entry.mod_id, entry.file
				)));
			}
			super::transform::validate_edits(&entry.edits).map_err(invalid)?;
		}
		Ok(())
	}

	/// Content identity for the decisions, independent of TOML entry order.
	pub fn identity(&self) -> String {
		let mut canonical = self.clone();
		canonical.renames.sort_by(|left, right| {
			(&left.mod_id, &left.file, &left.from, &left.to, &left.sha256).cmp(&(
				&right.mod_id,
				&right.file,
				&right.from,
				&right.to,
				&right.sha256,
			))
		});
		canonical.repairs.sort_by(|left, right| {
			(&left.mod_id, &left.file, &left.sha256).cmp(&(
				&right.mod_id,
				&right.file,
				&right.sha256,
			))
		});
		for repair in &mut canonical.repairs {
			repair.edits.sort_by_key(|edit| (edit.start, edit.end));
		}
		let bytes = serde_json::to_vec(&canonical).expect("culture decisions serialize");
		blake3::hash(&bytes).to_hex().to_string()
	}
}

fn validate_source<'a>(
	mod_id: &'a str,
	file: &'a str,
	sha256: &'a str,
	sources: &mut BTreeMap<(&'a str, &'a str), &'a str>,
) -> Result<(), ConfigError> {
	if mod_id.is_empty() || mod_id.trim() != mod_id {
		return Err(invalid("mod must be a nonempty contributor identity"));
	}
	let filename = file.strip_prefix("common/cultures/");
	if !filename.is_some_and(|name| {
		name.ends_with(".txt")
			&& name.len() > 4
			&& !name
				.chars()
				.any(|ch| ch.is_control() || "/\\:*?\"<>|".contains(ch))
			&& name.trim() == name
	}) {
		return Err(invalid(
			"file must be a literal common/cultures/<filename>.txt path",
		));
	}
	if sha256.len() != 64
		|| !sha256
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
	{
		return Err(invalid(
			"sha256 must contain 64 lowercase hexadecimal digits",
		));
	}
	if sources
		.insert((mod_id, file), sha256)
		.is_some_and(|previous| previous != sha256)
	{
		return Err(invalid(format!(
			"conflicting source SHA256 bindings for {mod_id}:{file}"
		)));
	}
	Ok(())
}

fn invalid(message: impl Into<String>) -> ConfigError {
	ConfigError::new(format!("invalid culture configuration: {}", message.into()))
}
