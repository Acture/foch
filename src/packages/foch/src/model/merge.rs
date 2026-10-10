use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::analysis::Severity;
use super::{GamePath, GamePathBuf, Isolation, SourceRepair};
use crate::playset::steam::WorkshopInstallIdentity;
use crate::project::AppliedDepOverride;

pub const MERGED_MOD_DESCRIPTOR_PATH: &str = "descriptor.mod";
pub const MERGE_PLAN_ARTIFACT_PATH: &str = ".foch/foch-merge-plan.json";
pub const MERGE_REPORT_ARTIFACT_PATH: &str = ".foch/foch-merge-report.json";
pub const MERGE_PROVENANCE_ARTIFACT_PATH: &str = ".foch/foch-provenance.json";
pub const MERGE_TRACE_ARTIFACT_PATH: &str = ".foch/foch-merge-trace.json";

/// Definition provenance bound to the exact bytes of a generated merge output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeProvenanceArtifact {
	pub version: u32,
	pub files: BTreeMap<GamePathBuf, MergeProvenanceFile>,
	pub mod_names: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeProvenanceFile {
	/// Lowercase hexadecimal BLAKE3 hash of the raw emitted file bytes.
	pub content_hash: String,
	/// Definition key → contributing mod IDs, in DAG-precedence order.
	pub definitions: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePlanStrategy {
	#[default]
	CopyThrough,
	LastWriterOverlay,
	StructuralMerge,
	/// Key-level dedup merge for `localisation/**.yml` files. Each merged
	/// file contains the union of keys from all contributors; on key
	/// collision the highest-precedence contributor wins.
	LocalisationMerge,
	ManualConflict,
	/// A file a reviewed transformation creates. It has no source contributor;
	/// its frozen bytes live in the transformation plan.
	Generated,
}

/// One contributor to a planned unit. `mod_id`, `precedence` and
/// `is_base_game` identify it; `source_path` only renders its physical file for
/// people and must not be compared, parsed or joined.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanContributor {
	pub mod_id: String,
	/// The physical source file, rendered for display.
	pub source_path: String,
	pub precedence: usize,
	pub is_base_game: bool,
}

#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct MergeUnitId {
	pub family_id: String,
	pub module_name: String,
}

/// One namespace a definition module writes.
///
/// An EU4 database can be fed by more than one directory, and each directory
/// keeps its own output file, `replace_path` prefix and content-family
/// semantics: `replace_path` is declared per directory, and the extractors
/// dispatch on the directory a definition was read from. Consolidating a
/// database into a single file would apply one directory's semantics to all of
/// them.
///
/// The namespace is always the directory holding the output, so an output
/// directly under the game root cannot be built, and reading back one whose
/// `namespace_prefix` is not its output's directory fails.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawMergeModuleOutput")]
pub struct MergeModuleOutput {
	output_path: GamePathBuf,
	namespace_prefix: GamePathBuf,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	replace_prefix: Option<GamePathBuf>,
}

/// A [`MergeModuleOutput`] as persisted, before its namespace is checked.
#[derive(Deserialize)]
struct RawMergeModuleOutput {
	output_path: GamePathBuf,
	namespace_prefix: GamePathBuf,
	#[serde(default)]
	replace_prefix: Option<GamePathBuf>,
}

impl TryFrom<RawMergeModuleOutput> for MergeModuleOutput {
	type Error = String;

	fn try_from(raw: RawMergeModuleOutput) -> Result<Self, String> {
		let output = Self::new(raw.output_path, raw.replace_prefix)
			.ok_or("a definition module output lies inside a namespace directory")?;
		if output.namespace_prefix != raw.namespace_prefix {
			return Err(format!(
				"namespace {} is not the directory holding output {}",
				raw.namespace_prefix, output.output_path
			));
		}
		Ok(output)
	}
}

impl MergeModuleOutput {
	/// One output whose namespace is the directory holding `output_path`, or
	/// `None` for a file directly under the root: a definition module is a
	/// directory, and the root is not one a module can own.
	pub fn new(output_path: GamePathBuf, replace_prefix: Option<GamePathBuf>) -> Option<Self> {
		let namespace_prefix = output_path.parent()?.to_owned();
		Some(Self {
			output_path,
			namespace_prefix,
			replace_prefix,
		})
	}

	/// The file this namespace's merged definitions are written to.
	pub fn output_path(&self) -> &GamePath {
		&self.output_path
	}

	/// The directory holding [`Self::output_path`], whose files are the
	/// namespace's inputs.
	pub fn namespace_prefix(&self) -> &GamePath {
		&self.namespace_prefix
	}

	/// The `replace_path` the generated descriptor declares for this
	/// namespace, if the merge replaces it.
	pub fn replace_prefix(&self) -> Option<&GamePath> {
		self.replace_prefix.as_deref()
	}
}

/// The files a definition module writes, one per namespace, ordered by output
/// path. There is always at least one: the first is the unit's primary path,
/// its review path, staging identity and plan ordering. It serializes as the
/// plain list, and reading back an empty list fails.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "Vec<MergeModuleOutput>")]
pub struct MergeModuleOutputs(Vec<MergeModuleOutput>);

impl MergeModuleOutputs {
	/// `None` when there is no output at all.
	pub fn new(outputs: Vec<MergeModuleOutput>) -> Option<Self> {
		(!outputs.is_empty()).then_some(Self(outputs))
	}

	pub fn primary(&self) -> &MergeModuleOutput {
		&self.0[0]
	}
}

impl TryFrom<Vec<MergeModuleOutput>> for MergeModuleOutputs {
	type Error = &'static str;

	fn try_from(outputs: Vec<MergeModuleOutput>) -> Result<Self, Self::Error> {
		Self::new(outputs).ok_or("a definition module writes at least one output")
	}
}

impl std::ops::Deref for MergeModuleOutputs {
	type Target = [MergeModuleOutput];

	fn deref(&self) -> &[MergeModuleOutput] {
		&self.0
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergePlanTarget {
	File {
		path: GamePathBuf,
	},
	Module {
		id: MergeUnitId,
		input_paths: Vec<GamePathBuf>,
		outputs: MergeModuleOutputs,
	},
}

impl MergePlanTarget {
	/// The unit's primary output path.
	pub fn output_path(&self) -> &GamePath {
		match self {
			Self::File { path } => path,
			Self::Module { outputs, .. } => outputs.primary().output_path(),
		}
	}

	/// Every path this unit writes, in plan order.
	pub fn output_paths(&self) -> Vec<&GamePath> {
		match self {
			Self::File { path } => vec![path],
			Self::Module { outputs, .. } => {
				outputs.iter().map(MergeModuleOutput::output_path).collect()
			}
		}
	}

	pub fn module_outputs(&self) -> &[MergeModuleOutput] {
		match self {
			Self::File { .. } => &[],
			Self::Module { outputs, .. } => outputs,
		}
	}

	pub fn module_id(&self) -> Option<&MergeUnitId> {
		match self {
			Self::File { .. } => None,
			Self::Module { id, .. } => Some(id),
		}
	}

	pub fn input_paths(&self) -> &[GamePathBuf] {
		match self {
			Self::File { path } => std::slice::from_ref(path),
			Self::Module { input_paths, .. } => input_paths,
		}
	}

	/// Inputs belonging to one of this unit's output namespaces, in plan order.
	///
	/// Definition modules are flat: EU4 reads the directory itself, not a tree
	/// below it, so `common/static_modifiers/nested/a.txt` is not an input of
	/// the `common/static_modifiers` namespace.
	pub fn namespace_input_paths(&self, namespace_prefix: &GamePath) -> Vec<&GamePath> {
		self.input_paths()
			.iter()
			.filter(|path| path.is_child_of(namespace_prefix))
			.map(GamePathBuf::as_game_path)
			.collect()
	}

	/// The primary output's `replace_path` prefix. Use [`Self::module_outputs`]
	/// when every namespace's prefix matters.
	pub fn replace_prefix(&self) -> Option<&GamePath> {
		match self {
			Self::File { .. } => None,
			Self::Module { outputs, .. } => outputs.primary().replace_prefix(),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergePlanEntry {
	pub target: MergePlanTarget,
	pub strategy: MergePlanStrategy,
	pub contributors: Vec<MergePlanContributor>,
	pub winner: Option<MergePlanContributor>,
	#[serde(default)]
	pub notes: Vec<String>,
}

impl MergePlanEntry {
	pub fn output_path(&self) -> &GamePath {
		self.target.output_path()
	}
}

#[cfg(test)]
mod tests {
	use super::{
		MERGE_EXECUTION_ATTESTATION_SCHEMA, MergeBackendId, MergeExecutionAttestation,
		MergeModuleOutput, MergeModuleOutputs, MergePlanContributor, MergePlanEntry,
		MergePlanResult, MergePlanStrategies, MergePlanStrategy, MergePlanTarget,
		MergeReportBaseSnapshot, MergeReportScope, MergeUnitId, ProductInputManifest,
		ProductInputMod,
	};
	use crate::model::{GamePath, GamePathBuf};
	use crate::playset::steam::{SteamId, WorkshopInstallIdentity};

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("valid game path")
	}

	fn output(output_path: &str, replace_prefix: Option<&str>) -> MergeModuleOutput {
		MergeModuleOutput::new(game_path(output_path), replace_prefix.map(game_path))
			.expect("output inside a namespace directory")
	}

	fn outputs(outputs: Vec<MergeModuleOutput>) -> MergeModuleOutputs {
		MergeModuleOutputs::new(outputs).expect("at least one output")
	}

	#[test]
	fn module_target_serializes_every_required_runtime_field() {
		let entry = MergePlanEntry {
			target: MergePlanTarget::Module {
				id: MergeUnitId {
					family_id: "governments".to_string(),
					module_name: "governments".to_string(),
				},
				input_paths: vec![game_path("common/governments/00_governments.txt")],
				outputs: outputs(vec![output(
					"common/governments/zzz_foch_governments.txt",
					Some("common/governments"),
				)]),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: Vec::new(),
			winner: None,
			notes: Vec::new(),
		};

		let json = serde_json::to_value(&entry).expect("serialize merge plan entry");
		assert_eq!(json["target"]["kind"], "module");
		// A unit writes one file per namespace, so the persisted plan carries a
		// list. Each entry keeps its own `replace_path` prefix.
		let outputs = json["target"]["outputs"].as_array().expect("outputs array");
		assert_eq!(outputs.len(), 1);
		assert_eq!(
			outputs[0]["output_path"],
			"common/governments/zzz_foch_governments.txt"
		);
		assert_eq!(outputs[0]["namespace_prefix"], "common/governments");
		assert_eq!(outputs[0]["replace_prefix"], "common/governments");
	}

	/// Plan paths are typed, but the persisted plan is the same text: every
	/// path serializes as its canonical string, exactly as the text fields it
	/// replaced did.
	#[test]
	fn a_representative_plan_serializes_to_the_same_json_text() {
		let contributor =
			|mod_id: &str, source_path: &str, precedence: usize| MergePlanContributor {
				mod_id: mod_id.to_string(),
				source_path: source_path.to_string(),
				precedence,
				is_base_game: precedence == 0,
			};
		let plan = MergePlanResult {
			game: "eu4".to_string(),
			playset_name: "playset".to_string(),
			generated_at: "1".to_string(),
			include_game_base: true,
			strategies: MergePlanStrategies {
				total_paths: 2,
				last_writer_overlay: 1,
				structural_merge: 1,
				..MergePlanStrategies::default()
			},
			paths: vec![
				MergePlanEntry {
					target: MergePlanTarget::Module {
						id: MergeUnitId {
							family_id: "CStaticModifierDataBase".to_string(),
							module_name: "CStaticModifierDataBase".to_string(),
						},
						input_paths: vec![
							game_path("common/event_modifiers/a.txt"),
							game_path("common/static_modifiers/b.txt"),
						],
						outputs: outputs(vec![
							output("common/event_modifiers/zzz_foch_event_modifiers.txt", None),
							output(
								"common/static_modifiers/zzz_foch_static_modifiers.txt",
								Some("common/static_modifiers"),
							),
						]),
					},
					strategy: MergePlanStrategy::StructuralMerge,
					contributors: vec![contributor(
						"mod-a",
						"/mods/a/common/event_modifiers/a.txt",
						1,
					)],
					winner: Some(contributor(
						"mod-a",
						"/mods/a/common/event_modifiers/a.txt",
						1,
					)),
					notes: Vec::new(),
				},
				MergePlanEntry {
					target: MergePlanTarget::File {
						path: game_path("gfx/interface/Mixed Case.dds"),
					},
					strategy: MergePlanStrategy::LastWriterOverlay,
					contributors: vec![
						contributor("__game__eu4", "/game/gfx/interface/Mixed Case.dds", 0),
						contributor("mod-b", "/mods/b/gfx/interface/Mixed Case.dds", 2),
					],
					winner: Some(contributor(
						"mod-b",
						"/mods/b/gfx/interface/Mixed Case.dds",
						2,
					)),
					notes: vec!["binary overlap resolved by last-writer-overlay".to_string()],
				},
			],
			fatal_errors: Vec::new(),
		};
		let expected = concat!(
			r#"{"game":"eu4","playset_name":"playset","generated_at":"1","include_game_base":true,"#,
			r#""strategies":{"total_paths":2,"copy_through":0,"last_writer_overlay":1,"structural_merge":1,"localisation_merge":0,"manual_conflict":0,"generated":0},"#,
			r#""paths":[{"target":{"kind":"module","id":{"family_id":"CStaticModifierDataBase","module_name":"CStaticModifierDataBase"},"#,
			r#""input_paths":["common/event_modifiers/a.txt","common/static_modifiers/b.txt"],"#,
			r#""outputs":[{"output_path":"common/event_modifiers/zzz_foch_event_modifiers.txt","namespace_prefix":"common/event_modifiers"},"#,
			r#"{"output_path":"common/static_modifiers/zzz_foch_static_modifiers.txt","namespace_prefix":"common/static_modifiers","replace_prefix":"common/static_modifiers"}]},"#,
			r#""strategy":"structural_merge","#,
			r#""contributors":[{"mod_id":"mod-a","source_path":"/mods/a/common/event_modifiers/a.txt","precedence":1,"is_base_game":false}],"#,
			r#""winner":{"mod_id":"mod-a","source_path":"/mods/a/common/event_modifiers/a.txt","precedence":1,"is_base_game":false},"notes":[]},"#,
			r#"{"target":{"kind":"file","path":"gfx/interface/Mixed Case.dds"},"strategy":"last_writer_overlay","#,
			r#""contributors":[{"mod_id":"__game__eu4","source_path":"/game/gfx/interface/Mixed Case.dds","precedence":0,"is_base_game":true},"#,
			r#"{"mod_id":"mod-b","source_path":"/mods/b/gfx/interface/Mixed Case.dds","precedence":2,"is_base_game":false}],"#,
			r#""winner":{"mod_id":"mod-b","source_path":"/mods/b/gfx/interface/Mixed Case.dds","precedence":2,"is_base_game":false},"#,
			r#""notes":["binary overlap resolved by last-writer-overlay"]}]}"#,
		);

		let json = serde_json::to_string(&plan).expect("serialize plan");
		assert_eq!(json, expected);
		let read_back: MergePlanResult = serde_json::from_str(&json).expect("read plan back");
		assert_eq!(
			serde_json::to_string(&read_back).expect("serialize again"),
			expected
		);
	}

	/// Report paths are typed, but the persisted report is the same text: a
	/// path record serializes as the string it held before, and a map keyed
	/// by path keeps its keys and their order.
	#[test]
	fn report_path_fields_serialize_to_the_same_json_text() {
		use super::{
			DeferredUnitReason, HandlerResolutionRecord, MergeReportConflictResolution,
			StaleVanillaTargetDescriptor,
		};
		use std::collections::BTreeMap;

		let record = HandlerResolutionRecord {
			path: game_path("history/countries/FRA - France.txt"),
			action: "kept_existing".to_string(),
			source: None,
			rationale: None,
		};
		assert_eq!(
			serde_json::to_string(&record).expect("serialize record"),
			r#"{"path":"history/countries/FRA - France.txt","action":"kept_existing"}"#
		);
		let resolution = MergeReportConflictResolution {
			path: game_path("common/ideas/00_basic_ideas.txt"),
			reason: "deferred".to_string(),
			deferred_reason: DeferredUnitReason::NeedsUserChoice,
			kind: None,
			leaf_conflicts: Vec::new(),
		};
		assert_eq!(
			serde_json::to_string(&resolution).expect("serialize resolution"),
			r#"{"path":"common/ideas/00_basic_ideas.txt","reason":"deferred","deferred_reason":"needs_user_choice","leaf_conflicts":[]}"#
		);
		let stale = StaleVanillaTargetDescriptor {
			mod_id: "mod-a".to_string(),
			mod_version: "1.0".to_string(),
			file_path: game_path("events/Flavor.txt"),
			patch_kind: "remove".to_string(),
			target_path: vec!["root".to_string()],
			target_key: None,
			note: None,
		};
		assert_eq!(
			serde_json::to_string(&stale).expect("serialize stale target"),
			r#"{"mod_id":"mod-a","mod_version":"1.0","file_path":"events/Flavor.txt","patch_kind":"remove","target_path":["root"],"target_key":null,"note":null}"#
		);
		let provenance: BTreeMap<GamePathBuf, BTreeMap<String, Vec<String>>> = [
			"common/scripted_effects/a-b.txt",
			"common/scripted_effects/a/b.txt",
		]
		.into_iter()
		.map(|path| (game_path(path), BTreeMap::new()))
		.collect();
		assert_eq!(
			serde_json::to_string(&provenance).expect("serialize provenance"),
			r#"{"common/scripted_effects/a-b.txt":{},"common/scripted_effects/a/b.txt":{}}"#
		);
	}

	#[test]
	fn a_persisted_plan_that_names_no_game_path_or_no_output_is_rejected() {
		for (json, reason) in [
			(
				r#"{"kind":"file","path":"common\\x.txt"}"#,
				"invalid game path",
			),
			(r#"{"kind":"file","path":"../x.txt"}"#, "invalid game path"),
			(
				r#"{"kind":"module","id":{"family_id":"f","module_name":"m"},"input_paths":[],"outputs":[]}"#,
				"at least one output",
			),
			(
				r#"{"kind":"module","id":{"family_id":"f","module_name":"m"},"input_paths":["common\\ideas\\a.txt"],"outputs":[{"output_path":"common/ideas/z.txt","namespace_prefix":"common/ideas"}]}"#,
				"invalid game path",
			),
			(
				r#"{"kind":"module","id":{"family_id":"f","module_name":"m"},"input_paths":[],"outputs":[{"output_path":"common/ideas/z.txt","namespace_prefix":"common/ideas","replace_prefix":"../ideas"}]}"#,
				"invalid game path",
			),
			// An output's namespace is the directory holding it: one directly
			// under the root has none, and a persisted namespace naming another
			// directory would select that directory's files as inputs.
			(
				r#"{"kind":"module","id":{"family_id":"f","module_name":"m"},"input_paths":[],"outputs":[{"output_path":"zzz_foch_root.txt","namespace_prefix":"common"}]}"#,
				"inside a namespace directory",
			),
			(
				r#"{"kind":"module","id":{"family_id":"f","module_name":"m"},"input_paths":[],"outputs":[{"output_path":"common/ideas/z.txt","namespace_prefix":"events"}]}"#,
				"namespace events is not the directory holding output common/ideas/z.txt",
			),
		] {
			let error = serde_json::from_str::<MergePlanTarget>(json).expect_err(json);
			assert!(error.to_string().contains(reason), "{json}: {error}");
		}
	}

	#[test]
	fn a_module_output_directly_under_the_root_has_no_namespace() {
		assert!(MergeModuleOutput::new(game_path("zzz_foch_root.txt"), None).is_none());
		let nested = output("common/ideas/zzz_foch_ideas.txt", None);
		assert_eq!(nested.namespace_prefix.as_str(), "common/ideas");
	}

	#[test]
	fn namespace_inputs_are_the_direct_children_of_the_namespace() {
		let target = MergePlanTarget::Module {
			id: MergeUnitId {
				family_id: "ideas".to_string(),
				module_name: "ideas".to_string(),
			},
			input_paths: vec![
				game_path("common/ideas/a.txt"),
				game_path("common/ideas/nested/b.txt"),
				game_path("common/ideas_extra/c.txt"),
			],
			outputs: outputs(vec![output("common/ideas/zzz_foch_ideas.txt", None)]),
		};

		assert_eq!(
			target.namespace_input_paths(GamePath::new("common/ideas").expect("valid game path")),
			[GamePath::new("common/ideas/a.txt").expect("valid game path")]
		);
	}

	#[test]
	fn file_target_exposes_its_output_path() {
		let target = MergePlanTarget::File {
			path: game_path("common/scripted_effects/example.txt"),
		};

		assert_eq!(
			target.output_path().as_str(),
			"common/scripted_effects/example.txt"
		);
		assert!(target.module_id().is_none());
	}

	#[test]
	fn product_input_manifest_digest_binds_mod_order_and_acf_version() {
		let first = ProductInputMod {
			mod_id: "mod-a".to_string(),
			precedence: 1,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_001),
				manifest_id: SteamId::new(2_001),
			},
		};
		let second = ProductInputMod {
			mod_id: "mod-b".to_string(),
			precedence: 2,
			workshop_identity: WorkshopInstallIdentity {
				app_id: 236_850,
				workshop_id: SteamId::new(1_002),
				manifest_id: SteamId::new(2_002),
			},
		};
		let manifest = ProductInputManifest::new(vec![first.clone(), second.clone()]);
		let reordered = ProductInputManifest::new(vec![second, first.clone()]);
		let mut changed = first;
		changed.workshop_identity.manifest_id = SteamId::new(2_003);
		let changed = ProductInputManifest::new(vec![changed]);

		assert_eq!(manifest.digest.len(), 64);
		assert_ne!(manifest.digest, reordered.digest);
		assert_ne!(manifest.digest, changed.digest);
		assert_eq!(manifest.attestation().mod_count, 2);
	}

	#[test]
	fn backend_ids_are_stable_and_keep_maturity_out_of_the_id() {
		let stable = MergeBackendId::GumtreePcsNway;
		let experimental = MergeBackendId::AddressPatch;

		assert_eq!(MergeBackendId::default(), stable);
		assert_eq!(stable.as_str(), "gumtree-pcs-nway");
		assert_eq!(experimental.as_str(), "address-patch");
		assert_eq!(
			serde_json::to_string(&stable).unwrap(),
			"\"gumtree-pcs-nway\""
		);
		assert_eq!(
			serde_json::to_string(&experimental).unwrap(),
			"\"address-patch\""
		);
		assert!(!stable.descriptor().experimental);
		assert!(experimental.descriptor().experimental);
	}

	#[test]
	fn execution_attestation_reports_the_backend_id() {
		let attestation = MergeExecutionAttestation {
			schema: MERGE_EXECUTION_ATTESTATION_SCHEMA.to_string(),
			backend: MergeBackendId::GumtreePcsNway,
			scope: MergeReportScope::FullProductMerge,
			base_snapshot: MergeReportBaseSnapshot::Disabled,
		};
		let json = serde_json::to_value(attestation).unwrap();

		assert_eq!(json["schema"], "2.0.0");
		assert_eq!(json["backend"], "gumtree-pcs-nway");
		assert!(json.get("kernel").is_none());
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanStrategies {
	pub total_paths: usize,
	pub copy_through: usize,
	pub last_writer_overlay: usize,
	pub structural_merge: usize,
	#[serde(default)]
	pub localisation_merge: usize,
	pub manual_conflict: usize,
	#[serde(default)]
	pub generated: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergePlanResult {
	pub game: String,
	pub playset_name: String,
	pub generated_at: String,
	pub include_game_base: bool,
	pub strategies: MergePlanStrategies,
	pub paths: Vec<MergePlanEntry>,
	#[serde(skip_serializing, skip_deserializing)]
	pub fatal_errors: Vec<String>,
}

impl MergePlanResult {
	pub fn has_fatal_errors(&self) -> bool {
		!self.fatal_errors.is_empty()
	}

	pub fn has_manual_conflicts(&self) -> bool {
		self.strategies.manual_conflict > 0
	}

	pub fn push_fatal_error(&mut self, message: impl Into<String>) {
		self.fatal_errors.push(message.into());
	}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportStatus {
	#[default]
	Ready,
	/// Safe units were exported while unresolved units were deferred or forced.
	PartialSuccess,
	/// Commit was stopped by an explicit non-conflict gate.
	Blocked,
	Fatal,
}

/// Wire schema for the execution identity embedded in every product merge
/// report. Merge-quality consumers reject reports without this attestation
/// instead of inferring the implementation from the command they launched.
pub const MERGE_EXECUTION_ATTESTATION_SCHEMA: &str = "2.0.0";

/// Wire schema and hashing profile for ordered Steam Workshop revisions.
///
/// Normal product execution trusts the read-only Workshop ACF as the source
/// version authority. It must never derive this identity by walking or hashing
/// the Workshop content tree.
pub const PRODUCT_INPUT_MANIFEST_SCHEMA: &str = "2.0.0";
pub const PRODUCT_INPUT_PROFILE: &str = "steam-workshop-acf-v1";
pub const PRODUCT_INPUT_DIGEST_ALGORITHM: &str = "blake3";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputMod {
	pub mod_id: String,
	pub precedence: usize,
	pub workshop_identity: WorkshopInstallIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputManifest {
	pub schema: String,
	pub profile: String,
	pub digest_algorithm: String,
	pub digest: String,
	pub mods: Vec<ProductInputMod>,
}

impl ProductInputManifest {
	pub fn new(mods: Vec<ProductInputMod>) -> Self {
		let mut manifest = Self {
			schema: PRODUCT_INPUT_MANIFEST_SCHEMA.to_string(),
			profile: PRODUCT_INPUT_PROFILE.to_string(),
			digest_algorithm: PRODUCT_INPUT_DIGEST_ALGORITHM.to_string(),
			digest: String::new(),
			mods,
		};
		manifest.digest = manifest.compute_digest();
		manifest
	}

	pub fn attestation(&self) -> ProductInputAttestation {
		ProductInputAttestation {
			schema: self.schema.clone(),
			profile: self.profile.clone(),
			digest_algorithm: self.digest_algorithm.clone(),
			digest: self.digest.clone(),
			mod_count: self.mods.len(),
		}
	}

	pub fn digest_is_valid(&self) -> bool {
		self.digest == self.compute_digest()
	}

	fn compute_digest(&self) -> String {
		let mut hasher = blake3::Hasher::new();
		update_digest_field(&mut hasher, &self.schema);
		update_digest_field(&mut hasher, &self.profile);
		update_digest_field(&mut hasher, &self.digest_algorithm);
		hasher.update(&(self.mods.len() as u64).to_le_bytes());
		for mod_input in &self.mods {
			update_digest_field(&mut hasher, &mod_input.mod_id);
			hasher.update(&(mod_input.precedence as u64).to_le_bytes());
			hasher.update(&mod_input.workshop_identity.app_id.to_le_bytes());
			update_digest_field(
				&mut hasher,
				mod_input.workshop_identity.workshop_id.as_str(),
			);
			update_digest_field(
				&mut hasher,
				mod_input.workshop_identity.manifest_id.as_str(),
			);
		}
		hasher.finalize().to_hex().to_string()
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductInputAttestation {
	pub schema: String,
	pub profile: String,
	pub digest_algorithm: String,
	pub digest: String,
	pub mod_count: usize,
}

fn update_digest_field(hasher: &mut blake3::Hasher, value: &str) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value.as_bytes());
}

#[derive(
	Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub enum MergeBackendId {
	/// Stable product backend based on GumTree matching, PCS alignment, and an
	/// N-way semantic join.
	#[default]
	#[serde(rename = "gumtree-pcs-nway")]
	GumtreePcsNway,
	/// Experimental address-patch backend retained for comparative evaluation.
	#[serde(rename = "address-patch")]
	AddressPatch,
}

impl MergeBackendId {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::GumtreePcsNway => "gumtree-pcs-nway",
			Self::AddressPatch => "address-patch",
		}
	}

	pub const fn descriptor(self) -> MergeBackendDescriptor {
		match self {
			Self::GumtreePcsNway => MergeBackendDescriptor {
				id: self,
				display_name: "GumTree + PCS N-way",
				experimental: false,
			},
			Self::AddressPatch => MergeBackendDescriptor {
				id: self,
				display_name: "Address patch",
				experimental: true,
			},
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MergeBackendDescriptor {
	pub id: MergeBackendId,
	pub display_name: &'static str,
	pub experimental: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReportScope {
	FullProductMerge,
	RetainedPathEvaluation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MergeReportBaseSnapshot {
	Disabled,
	Resolved { identity: String },
	Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeExecutionAttestation {
	pub schema: String,
	pub backend: MergeBackendId,
	pub scope: MergeReportScope,
	pub base_snapshot: MergeReportBaseSnapshot,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportValidation {
	pub fatal_errors: usize,
	pub strict_findings: usize,
	pub advisory_findings: usize,
	pub parse_errors: usize,
	pub unresolved_references: usize,
	pub missing_localisation: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportRename {
	pub family_id: String,
	pub original_key: String,
	pub renamed_key: String,
	pub mod_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReportConflictContributor {
	pub mod_id: String,
	pub mod_version: String,
	pub precedence: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
	DeepMergeable,
	SchemaCardinalityViolation,
}

impl ConflictKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::DeepMergeable => "deep_mergeable",
			Self::SchemaCardinalityViolation => "schema_cardinality_violation",
		}
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LeafConflictDetail {
	pub address_path: String,
	pub address_key: String,
	pub conflict_id: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub kind: Option<ConflictKind>,
	#[serde(default)]
	pub contributors: Vec<MergeReportConflictContributor>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredUnitReason {
	#[default]
	NeedsUserChoice,
	UnsupportedInput,
	EngineFailure,
}

impl DeferredUnitReason {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::NeedsUserChoice => "needs_user_choice",
			Self::UnsupportedInput => "unsupported_input",
			Self::EngineFailure => "engine_failure",
		}
	}
}

/// A repair Foch applied to its parsed copy of a source file that a merge unit
/// read. The source file itself is unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeReportSourceRepair {
	/// The stable id of the unit that read the repaired file.
	pub unit: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
	pub repair: SourceRepair,
}

/// A definition left out of a source file a merge unit read, because its
/// syntax error has no trustworthy repair. The merge read it as the mod's
/// parent has it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeReportIsolatedDefinition {
	/// The stable id of the unit that read the file.
	pub unit: String,
	pub mod_id: String,
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
	pub isolation: Isolation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeReportConflictResolution {
	/// The deferred unit's primary output.
	pub path: GamePathBuf,
	pub reason: String,
	#[serde(default)]
	pub deferred_reason: DeferredUnitReason,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub kind: Option<ConflictKind>,
	#[serde(default)]
	pub leaf_conflicts: Vec<LeafConflictDetail>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HandlerResolutionRecord {
	/// The output file the decision applies to. Commit reads a `kept_existing`
	/// record's file back from the prior output, so reading a report validates
	/// it as a game path.
	pub path: GamePathBuf,
	pub action: String,
	/// Where the kept content came from (a mod id, an AST address or an
	/// external file), rendered for people. It is never read back to find a
	/// file.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub rationale: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceContributor {
	pub mod_id: String,
	pub precedence: usize,
	pub dag_level: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeTracePolicy {
	CopyThrough,
	Overlay,
	Union,
	BooleanOr,
	NamedContainer,
	Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeTraceDecision {
	Adopted,
	Overridden,
	Unioned,
	Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceEntry {
	pub contributors: Vec<MergeTraceContributor>,
	pub policy: MergeTracePolicy,
	pub decision: MergeTraceDecision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MergeTraceEdge {
	pub merged_definition: String,
	pub source_mod: String,
	pub policy: MergeTracePolicy,
	pub decision: MergeTraceDecision,
	pub precedence: usize,
	pub dag_level: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DepMisuseEvidence {
	pub semantic_refs_to_dep: u32,
	pub false_remove_count: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DepMisuseFinding {
	pub mod_id: String,
	pub mod_display_name: String,
	pub suspicious_dep_id: String,
	pub suspicious_dep_display_name: String,
	pub evidence: DepMisuseEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionMismatchFinding {
	pub tag: String,
	pub severity: Severity,
	pub mod_id: String,
	pub mod_display_name: String,
	pub supported_version: String,
	pub game_version: String,
	pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StaleVanillaTargetDescriptor {
	pub mod_id: String,
	pub mod_version: String,
	pub file_path: GamePathBuf,
	pub patch_kind: String,
	pub target_path: Vec<String>,
	pub target_key: Option<String>,
	pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeReport {
	pub status: MergeReportStatus,
	/// Stable, product-authored execution identity. Historical reports omit it;
	/// consumers that require an exact kernel/scope contract must reject `None`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub execution: Option<MergeExecutionAttestation>,
	/// Stable attestation for the exact ordered mod inputs observed by the
	/// product. It intentionally contains no local paths or input payloads.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input: Option<ProductInputAttestation>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cache_source: Option<String>,
	/// When `status == Fatal` because input resolution failed, the
	/// underlying cause (e.g. missing/stale installed base data with the
	/// `foch data install` hint), mirroring what `foch check` surfaces.
	/// `None` on success — omitted from the report JSON so a non-fatal
	/// report stays byte-identical.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fatal_reason: Option<String>,
	/// Deferred units with genuine competing outcomes that require a reviewed
	/// user or policy choice.
	pub manual_conflict_count: usize,
	/// Deferred units whose syntax or content shape is not supported yet.
	#[serde(default)]
	pub unsupported_input_count: usize,
	/// Deferred units caused by an internal merge invariant or implementation
	/// failure. These are product bugs, not user conflicts.
	#[serde(default)]
	pub engine_failure_count: usize,
	pub generated_file_count: usize,
	pub copied_file_count: usize,
	pub overlay_file_count: usize,
	#[serde(default)]
	pub definition_module_count: usize,
	#[serde(default)]
	pub definition_module_generated_count: usize,
	#[serde(default)]
	pub definition_module_blocked_count: usize,
	/// Summed analysis and apply time of every definition module. Modules
	/// analyzed on different workers overlap, so the sum can exceed wall time.
	#[serde(default)]
	pub definition_module_elapsed_ms: u64,
	/// Unchanged vanilla base-game CopyThrough files intentionally not written
	/// to the merged mod because the game already ships them.
	#[serde(default)]
	pub base_passthrough_skipped_file_count: usize,
	/// Files whose patch-merge result was AST-equal to the vanilla base
	/// (modulo whitespace and comments) and were therefore skipped: shipping
	/// them would just shadow the game's own copy with byte-for-byte
	/// equivalent content.
	#[serde(default)]
	pub noop_skipped_file_count: usize,
	/// Generated files removed because every merge key already exists with
	/// identical content in a different file in the same opted-in family
	/// namespace. Tracked separately from same-path vanilla NoOp skips so the
	/// pruning reason remains auditable.
	#[serde(default)]
	pub cross_file_noop_skipped_file_count: usize,
	/// Individual generated entries removed because the same file's vanilla base
	/// already defines the key with an identical value in an opted-in family.
	#[serde(default)]
	pub per_entry_noop_skipped_count: usize,
	pub validation: MergeReportValidation,
	#[serde(default)]
	pub renames: Vec<MergeReportRename>,
	#[serde(default)]
	pub conflict_resolutions: Vec<MergeReportConflictResolution>,
	#[serde(default)]
	pub handler_resolutions: Vec<HandlerResolutionRecord>,
	#[serde(default)]
	pub dep_misuse: Vec<DepMisuseFinding>,
	#[serde(default)]
	pub version_mismatch: Vec<VersionMismatchFinding>,
	#[serde(default)]
	pub stale_vanilla_targets: Vec<StaleVanillaTargetDescriptor>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
	/// Source syntax repairs behind the units' analysis, in plan order.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub source_repairs: Vec<MergeReportSourceRepair>,
	/// Definitions left out of source files for review, in plan order.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub isolated_definitions: Vec<MergeReportIsolatedDefinition>,
	// D2 local dependency overrides applied during DAG-based merge.
	#[serde(default)]
	pub dep_overrides_applied: Vec<AppliedDepOverride>,
	/// BLAKE3 fingerprint of the playset state that produced this report: the
	/// ordered enabled-mods list with each mod's version, plus sorted local
	/// foch.toml [[overrides]] and [[resolutions]] entries. It attributes the
	/// analyzed input; it does not authorize reuse of a previous output.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub playset_fingerprint: Option<String>,
	/// Per merged file path → per top-level definition key → the mods whose
	/// content is adopted into the output, in DAG-precedence order. Only
	/// populated when `--provenance` is enabled. Diagnostic metadata only; it
	/// does not affect the emitted game files, so it is omitted from the report
	/// (and thus the report stays byte-identical) when the flag is off.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub definition_provenance: BTreeMap<GamePathBuf, BTreeMap<String, Vec<String>>>,
	/// Display names for mods referenced by surviving `definition_provenance`.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub provenance_mod_names: BTreeMap<String, String>,
	/// Per merged file path → per top-level definition key → merge audit trail.
	/// Populated with `definition_provenance` when `--provenance` is enabled.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub merge_trace: BTreeMap<GamePathBuf, BTreeMap<String, MergeTraceEntry>>,
}

impl MergeReport {
	pub fn deferred_unit_count(&self) -> usize {
		self.manual_conflict_count
			.saturating_add(self.unsupported_input_count)
			.saturating_add(self.engine_failure_count)
	}
}
