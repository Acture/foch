use crate::model::{GamePath, GamePathBuf};
use globset::{Candidate, GlobBuilder, GlobMatcher};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const RULES_1_37_5: &str = include_str!("rules/1.37.5.json");

#[derive(Deserialize)]
struct RuleFile {
	game_version: String,
	databases: BTreeMap<String, Vec<FileSelection>>,
}

#[derive(Deserialize)]
struct FileSelection {
	/// The game directory whose direct children the rule selects; reading the
	/// rule file validates it as a game path.
	directory: GamePathBuf,
	/// A glob over the file name.
	files: String,
}

struct DatabaseSelection {
	name: String,
	files: Vec<(GamePathBuf, GlobMatcher)>,
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
			let mut files: Vec<(GamePathBuf, GlobMatcher)> = Vec::new();
			for selection in selections {
				let matcher: GlobMatcher = GlobBuilder::new(&selection.files)
					.literal_separator(true)
					.backslash_escape(true)
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

	/// The database that loads `relative_path`: a rule matches a direct child
	/// of its directory whose file name matches its glob. Names compare as
	/// spelled, including case.
	pub(crate) fn database_for(&self, relative_path: &GamePath) -> Result<Option<&str>, String> {
		let Some(parent) = relative_path.parent() else {
			return Ok(None);
		};
		let file_name: &str = relative_path.file_name();
		let candidate: Candidate<'_> = Candidate::from_bytes(file_name);
		let mut matched: Option<&str> = None;
		for database in &self.databases {
			if !database.files.iter().any(|(directory, matcher)| {
				parent == directory.as_game_path() && matcher.is_match_candidate(&candidate)
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
	use crate::model::{GamePath, GamePathErrorKind};

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

	fn game_path(text: &str) -> &GamePath {
		GamePath::new(text).expect("valid game path")
	}

	#[test]
	fn rules_match_database_directories_and_filename_filters() {
		let rules: &DatabaseLoadRules = load_rules_for_version("1.37.5").unwrap();
		for path in [
			"common/static_modifiers/mod_a.txt",
			"common/event_modifiers/mod_b.txt",
		] {
			assert_eq!(
				rules.database_for(game_path(path)).unwrap(),
				Some("CStaticModifierDataBase")
			);
		}
		for path in [
			"common/static_modifiers/mod_a.gui",
			"common/static_modifiers_extra/mod_a.txt",
			"common/static_modifiers/nested/mod_a.txt",
			"common/Static_Modifiers/mod_a.txt",
			"common/static_modifiers",
			"gfx/example.dds",
			"trigger_profile.txt",
		] {
			assert_eq!(rules.database_for(game_path(path)).unwrap(), None, "{path}");
		}
		for (path, database) in [
			("common/ideas/x.txt", Some("CIdeaDataBase")),
			("common/ideas/sub/x.txt", None),
			(
				"common/scripted_effects/x.txt",
				Some("CScriptedEffectTemplateDatabase"),
			),
			(
				"common/governments/00_governments.txt",
				Some("CGovernmentDataBase"),
			),
			("common/triggered_modifiers/deep/a.txt", None),
			("interface/x.gui", None),
			("events/x.txt", None),
		] {
			assert_eq!(
				rules.database_for(game_path(path)).unwrap(),
				database,
				"{path}"
			);
		}
		assert!(load_rules_for_version("1.37.4").is_none());
	}

	#[test]
	fn filename_globs_use_portable_escapes_on_every_host() {
		let rules: DatabaseLoadRules = DatabaseLoadRules::parse(
			r#"{"game_version":"test","databases":{"Ideas":[{"directory":"common/ideas","files":"literal\\*.txt"}]}}"#,
		)
		.expect("valid rules");
		assert_eq!(
			rules.database_for(game_path("common/ideas/literal*.txt")),
			Ok(Some("Ideas"))
		);
		assert_eq!(
			rules.database_for(game_path("common/ideas/literal_other.txt")),
			Ok(None)
		);
	}

	#[test]
	fn windows_spelled_rule_input_is_rejected_at_the_game_path_boundary_not_folded() {
		// `database_for` used to rewrite `\` into `/` before matching, so a
		// Unix file named `common\event_modifiers\mod_b.txt` (one name, at the
		// root) was given the database of the nested file. Rule lookups now
		// take a game path: the nested file still matches, and the file whose
		// name holds backslashes gets no game path, so it never reaches the
		// rules.
		let rules: &DatabaseLoadRules = load_rules_for_version("1.37.5").unwrap();
		assert_eq!(
			rules.database_for(game_path("common/event_modifiers/mod_b.txt")),
			Ok(Some("CStaticModifierDataBase"))
		);
		let spelled = r"common\event_modifiers\mod_b.txt";
		let reserved = |kind: &GamePathErrorKind| {
			matches!(
				kind,
				GamePathErrorKind::ReservedCharacter {
					character: '\\',
					..
				}
			)
		};
		assert!(reserved(&GamePath::new(spelled).expect_err(spelled).kind));
		#[cfg(unix)]
		assert!(reserved(
			&crate::model::GamePathBuf::from_native_relative(std::path::Path::new(spelled))
				.expect_err(spelled)
				.kind
		));
	}

	#[test]
	fn a_rule_directory_that_is_not_a_game_path_fails_to_load() {
		for directory in ["common/test/", r"common\test", "../common", ""] {
			let json = serde_json::json!({
				"game_version": "test",
				"databases": {"First": [{"directory": directory, "files": "*.txt"}]},
			})
			.to_string();
			let error = DatabaseLoadRules::parse(&json)
				.err()
				.unwrap_or_else(|| panic!("{directory:?} must not load"));
			assert!(
				error.contains("invalid game path"),
				"{directory:?}: {error}"
			);
		}
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
		assert!(
			rules
				.database_for(game_path("common/test/special.txt"))
				.is_err()
		);
		assert_eq!(
			rules
				.database_for(game_path("common/test/other.txt"))
				.unwrap(),
			Some("First")
		);
	}
}
