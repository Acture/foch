use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

const RULES_1_37_5: &str = include_str!("rules/1.37.5.json");

#[derive(Deserialize)]
struct RuleFile {
	game_version: String,
	databases: BTreeMap<String, Vec<FileSelection>>,
}

#[derive(Deserialize)]
struct FileSelection {
	directory: String,
	files: String,
}

struct DatabaseSelection {
	name: String,
	files: Vec<(String, GlobMatcher)>,
}

pub(crate) struct DatabaseLoadRules {
	game_version: String,
	databases: Vec<DatabaseSelection>,
}

impl DatabaseLoadRules {
	fn parse(json: &str) -> Result<Self, String> {
		let file: RuleFile = serde_json::from_str(json).map_err(|error| error.to_string())?;
		let mut databases: Vec<DatabaseSelection> = Vec::new();
		for (name, selections) in file.databases {
			let mut files: Vec<(String, GlobMatcher)> = Vec::new();
			for selection in selections {
				let matcher: GlobMatcher = GlobBuilder::new(&selection.files)
					.literal_separator(true)
					.build()
					.map_err(|error| error.to_string())?
					.compile_matcher();
				files.push((selection.directory, matcher));
			}
			databases.push(DatabaseSelection { name, files });
		}
		Ok(Self {
			game_version: file.game_version,
			databases,
		})
	}

	pub(crate) fn database_for(&self, relative_path: &str) -> Result<Option<&str>, String> {
		let normalized: String = relative_path.replace('\\', "/");
		let path: &Path = Path::new(&normalized);
		let Some(filename) = path.file_name() else {
			return Ok(None);
		};
		let mut matched: Option<&str> = None;
		for database in &self.databases {
			if !database.files.iter().any(|(directory, matcher)| {
				path.parent() == Some(Path::new(directory)) && matcher.is_match(Path::new(filename))
			}) {
				continue;
			}
			if let Some(previous) = matched {
				return Err(format!(
					"EU4 {} loading rules assign {relative_path} to both {previous} and {}",
					self.game_version, database.name
				));
			}
			matched = Some(&database.name);
		}
		Ok(matched)
	}
}

/// EU4 reports its own version as the launcher's `rawVersion` (`v1.37.5.0`),
/// while an extracted rule snapshot is keyed by the `major.minor.patch` triple
/// the extractor read from the binary (`1.37.5`). Reduce the former to the
/// latter so rules resolve for a real installation and not only for a
/// hand-written test string.
///
/// The triple is a proxy identity, not the real one: the rule file also records
/// `binary_sha256`, and a hotfix can ship a different binary under the same
/// triple.
fn normalize_game_version(version: &str) -> Option<String> {
	let version: &str = version.trim();
	let version: &str = version.strip_prefix(['v', 'V']).unwrap_or(version);
	let mut components = version.splitn(4, '.');
	let major: &str = components.next()?;
	let minor: &str = components.next()?;
	let patch: &str = components.next()?;
	[major, minor, patch]
		.iter()
		.all(|component| {
			!component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
		})
		.then(|| format!("{major}.{minor}.{patch}"))
}

pub(crate) fn load_rules_for_version(version: &str) -> Option<&'static DatabaseLoadRules> {
	static RULES: OnceLock<DatabaseLoadRules> = OnceLock::new();
	match normalize_game_version(version)?.as_str() {
		"1.37.5" => Some(RULES.get_or_init(|| {
			let rules: DatabaseLoadRules =
				DatabaseLoadRules::parse(RULES_1_37_5).expect("valid embedded EU4 loading rules");
			assert_eq!(rules.game_version, "1.37.5");
			rules
		})),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::{DatabaseLoadRules, load_rules_for_version, normalize_game_version};

	#[test]
	fn rules_resolve_for_the_version_string_a_real_installation_reports() {
		// `detect_game_version` returns launcher-settings.json's `rawVersion`
		// verbatim, so production never supplies a bare triple.
		for version in [
			"v1.37.5.0",
			"1.37.5.0",
			"1.37.5",
			"V1.37.5.0",
			" v1.37.5.0 ",
		] {
			assert_eq!(
				normalize_game_version(version).as_deref(),
				Some("1.37.5"),
				"{version}"
			);
			assert!(load_rules_for_version(version).is_some(), "{version}");
		}
		for version in ["1.37", "v1.37", "", "v", "1.37.x", "Inca"] {
			assert_eq!(normalize_game_version(version), None, "{version}");
			assert!(load_rules_for_version(version).is_none(), "{version}");
		}
		// A different patch level must not borrow another snapshot's rules.
		assert_eq!(
			normalize_game_version("v1.37.4.0").as_deref(),
			Some("1.37.4")
		);
		assert!(load_rules_for_version("v1.37.4.0").is_none());
	}

	#[test]
	fn rules_match_database_directories_and_filename_filters() {
		let rules: &DatabaseLoadRules = load_rules_for_version("1.37.5").unwrap();
		for path in [
			"common/static_modifiers/mod_a.txt",
			"common/event_modifiers/mod_b.txt",
			"common\\event_modifiers\\mod_b.txt",
		] {
			assert_eq!(
				rules.database_for(path).unwrap(),
				Some("CStaticModifierDataBase")
			);
		}
		for path in [
			"common/static_modifiers/mod_a.gui",
			"common/static_modifiers_extra/mod_a.txt",
			"common/static_modifiers/nested/mod_a.txt",
			"gfx/example.dds",
		] {
			assert_eq!(rules.database_for(path).unwrap(), None);
		}
		assert!(load_rules_for_version("1.37.4").is_none());
	}

	#[test]
	fn overlapping_database_rules_do_not_choose_an_arbitrary_owner() {
		let rules: DatabaseLoadRules = DatabaseLoadRules::parse(
			r#"{"game_version":"test","databases":{
				"First":[{"directory":"common/test","files":"*.txt"}],
				"Second":[{"directory":"common/test","files":"special.*"}]
			}}"#,
		)
		.unwrap();
		assert!(rules.database_for("common/test/special.txt").is_err());
		assert_eq!(
			rules.database_for("common/test/other.txt").unwrap(),
			Some("First")
		);
	}
}
