use crate::model::{GamePath, GamePathBuf};
use globset::{Candidate, GlobBuilder, GlobMatcher};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

const RULES_1_37_5: &str = include_str!("rules/1.37.5.json");

#[derive(Deserialize)]
struct RuleFile {
	game_version: String,
	/// The executable builds the rules were verified against. Builds of one
	/// game version come from the same engine source, so one version's rules
	/// are a single semantic artifact and this list records the provenance;
	/// a build that ever disagrees is split into its own rule file instead.
	builds: Vec<RuleBuild>,
	/// Each database as written: a bare list of its directories, or an object
	/// that also records how a name two of them share resolves. Reading the
	/// raw value keeps the inner error (such as an invalid game path) instead
	/// of an untagged enum's "matched no variant".
	databases: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct RuleBuild {
	platform: String,
	binary_sha256: String,
}

/// A database whose directories are listed, plus the established handling of
/// a name two of them share.
#[derive(Deserialize)]
struct DetailedDatabaseRule {
	directories: Vec<FileSelection>,
	same_name: SameNameRule,
}

#[derive(Deserialize)]
struct SameNameRule {
	/// How a name two directories of the database both define resolves. The
	/// one recorded resolution: the later-registered directory's entry is the
	/// one lookups return.
	resolution: SameNameResolution,
	/// The directories in the order the executable registers them.
	registration_order: Vec<GamePathBuf>,
	/// What established the resolution — the binary and the functions read —
	/// so a rule never reads as a guess.
	evidence: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SameNameResolution {
	LaterDirectoryWins,
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
	same_name: Option<SameNameRule>,
}

pub(crate) struct DatabaseLoadRules {
	game_version: String,
	databases: Vec<DatabaseSelection>,
}

impl DatabaseLoadRules {
	fn parse(json: &str) -> Result<Self, String> {
		let file: RuleFile = serde_json::from_str(json).map_err(|error| error.to_string())?;
		for build in &file.builds {
			if !matches!(build.platform.as_str(), "macos" | "windows" | "linux")
				|| build.binary_sha256.len() != 64
				|| !build
					.binary_sha256
					.bytes()
					.all(|byte| byte.is_ascii_hexdigit())
			{
				return Err(format!(
					"rule build provenance is malformed: {} {}",
					build.platform, build.binary_sha256
				));
			}
		}
		if file.builds.is_empty() {
			return Err("rule file records no verifying build".to_string());
		}
		let mut databases: Vec<DatabaseSelection> = Vec::new();
		for (name, rule) in file.databases {
			let (selections, same_name) = match rule {
				serde_json::Value::Array(_) => {
					let selections: Vec<FileSelection> =
						serde_json::from_value(rule).map_err(|error| error.to_string())?;
					(selections, None)
				}
				serde_json::Value::Object(_) => {
					let detailed: DetailedDatabaseRule =
						serde_json::from_value(rule).map_err(|error| error.to_string())?;
					(detailed.directories, Some(detailed.same_name))
				}
				_ => {
					return Err(format!(
						"rule for database {name} is neither a directory list nor a detailed rule"
					));
				}
			};
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
			if let Some(same_name) = &same_name {
				// The order must place every directory exactly once: a
				// directory it misses would leave that pair's repeat as
				// unresolvable as a database with no rule at all.
				let directories: BTreeSet<_> = files
					.iter()
					.map(|(directory, _)| directory.as_game_path())
					.collect();
				let ordered: BTreeSet<_> = same_name
					.registration_order
					.iter()
					.map(|directory| directory.as_game_path())
					.collect();
				if directories != ordered || same_name.evidence.trim().is_empty() {
					return Err(format!(
						"same-name rule for {name} is malformed: the registration order must cover the database's directories exactly and cite its evidence"
					));
				}
			}
			databases.push(DatabaseSelection {
				name,
				files,
				same_name,
			});
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

	/// The directories of `database` in the order the executable registers
	/// them, when the rules record how a name two of them share resolves: the
	/// last one holding the name is the one lookups return.
	pub(crate) fn same_name_registration_order(&self, database: &str) -> Option<&[GamePathBuf]> {
		self.databases
			.iter()
			.find(|selection| selection.name == database)
			.and_then(|selection| selection.same_name.as_ref())
			.map(|rule| match rule.resolution {
				SameNameResolution::LaterDirectoryWins => rule.registration_order.as_slice(),
			})
	}
}

/// EU4 reports its own version as the launcher's `rawVersion` (`v1.37.5.0`),
/// while an extracted rule snapshot is keyed by the `major.minor.patch` triple
/// the extractor read from the binary (`1.37.5`). Reduce the former to the
/// latter so rules resolve for a real installation and not only for a
/// hand-written test string.
///
/// The triple is a proxy identity, not the real one: each rule file records
/// the builds (`platform` and `binary_sha256`) its content was extracted and
/// verified against, and a hotfix can ship a different binary under the same
/// triple. Builds of one triple share the engine source, so they verify the
/// same rules in principle; the extractor refuses to merge a build whose
/// rules disagree and splits it into its own file instead.
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
	use super::{
		DatabaseLoadRules, RULES_1_37_5, RuleFile, load_rules_for_version, normalize_game_version,
	};
	use crate::model::{GamePath, GamePathErrorKind};

	#[test]
	fn embedded_rules_record_the_builds_that_verified_them() {
		// One game version is one semantic artifact; `builds` is the
		// provenance of which executable builds verified it.
		let file: RuleFile =
			serde_json::from_str(RULES_1_37_5).expect("valid embedded EU4 loading rules");
		assert_eq!(file.game_version, "1.37.5");
		assert!(!file.builds.is_empty());
		for build in &file.builds {
			assert!(
				matches!(build.platform.as_str(), "macos" | "windows" | "linux"),
				"unknown platform {}",
				build.platform
			);
			assert_eq!(build.binary_sha256.len(), 64);
		}
	}

	#[test]
	fn the_same_name_rule_records_the_registration_order_that_resolves_repeats() {
		let rules: &DatabaseLoadRules = load_rules_for_version("1.37.5").unwrap();
		let order = rules
			.same_name_registration_order("CStaticModifierDataBase")
			.expect("the static modifier database records its registration order");
		assert_eq!(
			order
				.iter()
				.map(|directory| directory.as_str())
				.collect::<Vec<_>>(),
			["common/static_modifiers", "common/event_modifiers"]
		);
		// A single-directory database has no repeat to resolve.
		assert_eq!(rules.same_name_registration_order("CIdeaDataBase"), None);
	}

	#[test]
	fn a_same_name_rule_must_place_every_directory_and_cite_its_evidence() {
		let rules = |order: serde_json::Value, evidence: &str| {
			serde_json::json!({
				"game_version": "test",
				"builds": [{"platform": "macos", "binary_sha256": "a".repeat(64)}],
				"databases": {"DB": {
					"directories": [
						{"directory": "common/one", "files": "*.txt"},
						{"directory": "common/two", "files": "*.txt"}
					],
					"same_name": {
						"resolution": "later_directory_wins",
						"registration_order": order,
						"evidence": evidence
					}
				}}
			})
			.to_string()
		};
		assert!(
			DatabaseLoadRules::parse(&rules(
				serde_json::json!(["common/one", "common/two"]),
				"decompiled registration and lookup"
			))
			.is_ok()
		);
		for malformed in [
			// An order that misses a directory leaves that pair's repeat as
			// unresolvable as a database with no rule at all.
			rules(serde_json::json!(["common/one"]), "decompiled"),
			// An order naming a directory the database does not hold.
			rules(
				serde_json::json!(["common/one", "common/two", "common/three"]),
				"decompiled",
			),
			// A rule without its evidence reads as a guess.
			rules(serde_json::json!(["common/one", "common/two"]), "   "),
		] {
			assert!(DatabaseLoadRules::parse(&malformed).is_err(), "{malformed}");
		}
	}

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
			r#"{"game_version":"test","builds":[{"platform":"macos","binary_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],"databases":{"Ideas":[{"directory":"common/ideas","files":"literal\\*.txt"}]}}"#,
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
				"builds": [{"platform": "macos", "binary_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],
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
			r#"{"game_version":"test","builds":[{"platform":"macos","binary_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],"databases":{
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
