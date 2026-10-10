//! Reviewed syntax repairs of mod script files, bound to the exact source.

use super::ConfigError;
use super::transform::{SourceEdit, validate_edits};
use crate::model::GamePath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One reviewed repair: exact edits to a mod's script file, applied to Foch's
/// parsed copy only while the file still has the reviewed SHA256.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepairEntry {
	#[serde(rename = "mod")]
	pub mod_id: String,
	/// The game path of the file in the mod.
	pub file: String,
	pub sha256: String,
	pub edits: Vec<SourceEdit>,
}

pub(super) fn validate_repairs(entries: &[SourceRepairEntry]) -> Result<(), ConfigError> {
	let mut files = BTreeSet::new();
	for entry in entries {
		if entry.mod_id.is_empty() || entry.mod_id.trim() != entry.mod_id {
			return Err(invalid("mod must be a nonempty contributor identity"));
		}
		let path = GamePath::new(&entry.file)
			.map_err(|error| invalid(format!("file `{}`: {error}", entry.file)))?;
		if path
			.as_str()
			.rsplit_once('.')
			.is_none_or(|(_, extension)| extension.eq_ignore_ascii_case("lua"))
		{
			return Err(invalid(format!(
				"file `{}` must be a Clausewitz script, not Lua",
				entry.file
			)));
		}
		if entry.sha256.len() != 64
			|| !entry
				.sha256
				.bytes()
				.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
		{
			return Err(invalid(
				"sha256 must contain 64 lowercase hexadecimal digits",
			));
		}
		if !files.insert((&entry.mod_id, &entry.file)) {
			return Err(invalid(format!(
				"duplicate repair entry for {}:{}",
				entry.mod_id, entry.file
			)));
		}
		validate_edits(&entry.edits).map_err(invalid)?;
	}
	Ok(())
}

/// Content identity of the repairs, independent of entry and edit order.
pub(crate) fn repairs_identity(entries: &[SourceRepairEntry]) -> String {
	let mut canonical = entries.to_vec();
	canonical.sort_by(|left, right| {
		(&left.mod_id, &left.file, &left.sha256).cmp(&(&right.mod_id, &right.file, &right.sha256))
	});
	for entry in &mut canonical {
		entry.edits.sort_by_key(|edit| (edit.start, edit.end));
	}
	let bytes = serde_json::to_vec(&canonical).expect("repairs serialize");
	blake3::hash(&bytes).to_hex().to_string()
}

/// The entry that would apply `proposal` to `bytes`, the mod's file at
/// `file`, bound to their SHA256. `None` when the text does not have what the
/// proposal expects.
pub fn proposed_repair_entry(
	mod_id: &str,
	file: &GamePath,
	bytes: &[u8],
	proposal: crate::model::RepairProposal,
) -> Option<SourceRepairEntry> {
	use sha2::{Digest, Sha256};

	let content = crate::game::eu4::text::decode_paradox_bytes(bytes);
	let edit = crate::game::eu4::script::parser::repair_text_edit(
		&content,
		proposal.edit,
		proposal.offset,
	)?;
	let end = edit.offset + edit.remove;
	Some(SourceRepairEntry {
		mod_id: mod_id.to_owned(),
		file: file.as_str().to_owned(),
		sha256: format!("{:x}", Sha256::digest(bytes)),
		edits: vec![SourceEdit {
			start: edit.offset,
			end,
			expected: content.get(edit.offset..end)?.to_owned(),
			replacement: edit.insert.to_owned(),
		}],
	})
}

/// `entries` as the `[[repairs]]` tables of a `foch.toml`.
pub fn render_repairs_toml(entries: &[SourceRepairEntry]) -> String {
	#[derive(Serialize)]
	struct Repairs<'a> {
		repairs: &'a [SourceRepairEntry],
	}
	toml::to_string(&Repairs { repairs: entries }).expect("repairs serialize as TOML")
}

fn invalid(message: impl Into<String>) -> ConfigError {
	ConfigError::new(format!("invalid repair configuration: {}", message.into()))
}

#[cfg(test)]
mod tests {
	use super::validate_repairs;
	use crate::project::Project;

	fn config(file: &str, sha256: &str) -> String {
		format!(
			"[[repairs]]\nmod = 'b'\nfile = '{file}'\nsha256 = '{sha256}'\nedits = [{{ start = 3, end = 4, expected = '}}', replacement = '' }}]\n"
		)
	}

	#[test]
	fn a_reviewed_repair_binds_any_script_file_but_lua() {
		let sha = "a".repeat(64);
		let project: Project = toml::from_str(&config("events/b.txt", &sha)).unwrap();
		assert_eq!(project.repairs.len(), 1);
		assert!(validate_repairs(&project.repairs).is_ok());
		for (file, sha256) in [
			("common/defines/b.lua", sha.as_str()),
			(r"events\b.txt", sha.as_str()),
			("events/b.txt", "ABC"),
		] {
			assert!(
				toml::from_str::<Project>(&config(file, sha256)).is_err(),
				"{file} {sha256}"
			);
		}
		let twice = format!(
			"{}{}",
			config("events/b.txt", &sha),
			config("events/b.txt", &sha)
		);
		assert!(toml::from_str::<Project>(&twice).is_err());
	}
}
