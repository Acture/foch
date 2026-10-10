//! Execute content-family transformations without interpreting their domain semantics.

pub(crate) mod input;
pub(crate) mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::game::eu4::content::{DuplicateDefinitionPolicy, eu4};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::emit::EmitOptions;
use crate::game::eu4::script::localisation::{ParsedLocalisationFile, parse_localisation_bytes};
use crate::game::eu4::script::parser::{AstFile, parse_clausewitz_content};
use crate::game::eu4::text::decode_paradox_bytes;
use crate::input::{ResolvedInput, ResolvedInputContributor};
use crate::merge::dag::IgnoreReplacePath;
use crate::merge::error::{MergeError, MergeErrorSubject};
use crate::model::{
	GamePath, GamePathBuf, MergePlanEntry, MergePlanResult, MergePlanStrategy, MergePlanTarget,
};
use crate::project::{DepOverride, Project};
use input::SourceGuard;
use tree::EntityTransform;

pub(crate) struct TransformRequest<'a> {
	pub project: &'a Project,
	pub dep_overrides: &'a [DepOverride],
	pub ignore_replace_path: &'a IgnoreReplacePath,
	pub emit_options: &'a EmitOptions,
	pub duplicate_definitions: Option<DuplicateDefinitionPolicy>,
	pub reference_inventory_complete: bool,
}

pub(crate) trait TransformAdapter: Sync {
	fn reviewed_inputs<'a>(
		&self,
		_project: &'a Project,
	) -> Result<input::ReviewedInputPolicy<'a>, MergeError> {
		Ok(input::ReviewedInputPolicy::default())
	}

	fn infer_entity_transform(
		&self,
		_base: &AstFile,
		_revisions: &[&AstFile],
	) -> Result<Option<Arc<dyn EntityTransform>>, String> {
		Ok(None)
	}

	fn infer_dag_transform(
		&self,
		_file_dag: &crate::merge::dag::FileDag,
		_vanilla: Option<&ParsedScriptFile>,
		_contributors: &std::collections::HashMap<crate::merge::dag::ModId, ParsedScriptFile>,
		_policies: &crate::game::eu4::content::MergePolicies,
	) -> Result<Option<Arc<dyn EntityTransform>>, String> {
		Ok(None)
	}

	/// One group per independently committable transformation. A failure in
	/// one group must not withhold another group's outputs.
	fn prepare(
		&self,
		input: &mut ResolvedInput,
		request: &TransformRequest<'_>,
		reviewed: input::ReviewedInputs,
	) -> Result<Vec<PreparedTransform>, MergeError>;
}

/// Domain-specific audit of the actual effective output, after structural merging.
pub(crate) trait OutputValidation: std::fmt::Debug + Send + Sync {
	fn validate_emitted(&self, output: &EmittedOutput) -> Vec<String>;
}

/// The effective files of one connected output group, each parsed the way the
/// game loads it. Localisation and binary resources are never read as
/// Clausewitz script.
#[derive(Debug, Default)]
pub(crate) struct EmittedOutput {
	pub scripts: Vec<(GamePathBuf, AstFile)>,
	pub localisation: Vec<(GamePathBuf, ParsedLocalisationFile)>,
	pub resources: Vec<(GamePathBuf, EmittedResource)>,
}

/// Bounded metadata for a file whose contents a validator does not interpret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EmittedResource {
	pub len: usize,
	pub sha256: String,
}

impl EmittedOutput {
	/// Classify and parse one file. Returns its parse problems; the caller
	/// blocks on them only for files Foch wrote, not for source fallbacks.
	pub(crate) fn push(&mut self, path: &GamePath, bytes: &[u8]) -> Vec<String> {
		let relative = path.to_owned();
		let lower = path.as_str().to_ascii_lowercase();
		if lower.starts_with("localisation/") && lower.ends_with(".yml") {
			let parsed = parse_localisation_bytes("foch_output", &relative, bytes);
			let issues = parsed
				.parse_issues
				.iter()
				.map(|issue| format!("{path}:{}: {}", issue.line, issue.message))
				.collect();
			self.localisation.push((relative, parsed));
			issues
		} else if [".txt", ".gui", ".gfx"]
			.iter()
			.any(|extension| lower.ends_with(extension))
		{
			let source = decode_paradox_bytes(bytes);
			let parsed = parse_clausewitz_content(&relative, &source);
			let issues = if parsed.diagnostics.is_empty() {
				Vec::new()
			} else {
				vec![format!("{path}: emitted transformation has parse errors")]
			};
			self.scripts.push((relative, parsed.ast));
			issues
		} else {
			self.resources.push((
				relative,
				EmittedResource {
					len: bytes.len(),
					sha256: input::sha256(bytes),
				},
			));
			Vec::new()
		}
	}
}

/// A new output file a transformation creates. Its bytes are frozen during
/// preparation; the path is relative to the merged mod root.
#[derive(Clone, Debug)]
pub(crate) struct GeneratedArtifact {
	pub path: GamePathBuf,
	pub bytes: Arc<[u8]>,
}

/// One semantic transformation and its complete set of output dependencies.
#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedTransform {
	pub id: String,
	pub identity: String,
	pub paths: BTreeSet<GamePathBuf>,
	pub findings: Vec<String>,
	pub evidence: Vec<String>,
	pub entity_transform: Option<Arc<dyn EntityTransform>>,
	pub validator: Option<Arc<dyn OutputValidation>>,
	pub source_guard: SourceGuard,
	pub overlays: Vec<ScriptTransform>,
	pub generated: Vec<GeneratedArtifact>,
}

/// A semantic edit is bound to the exact frozen document it was computed from.
/// Later adapters must read the current overlay before adding their changes.
#[derive(Clone, Debug)]
pub(crate) struct ScriptTransform {
	before: Arc<ParsedScriptFile>,
	after: ParsedScriptFile,
	bytes: Vec<u8>,
}

impl ScriptTransform {
	pub(crate) fn new(before: Arc<ParsedScriptFile>, ast: AstFile, bytes: Vec<u8>) -> Self {
		let mut after = before.as_ref().clone();
		after.ast = ast;
		Self {
			before,
			after,
			bytes,
		}
	}
}

impl PreparedTransform {
	pub(crate) fn affects(&self, entry: &MergePlanEntry) -> bool {
		if entry.strategy == MergePlanStrategy::Generated {
			return self
				.generated
				.iter()
				.any(|artifact| artifact.path == entry.output_path());
		}
		entry
			.target
			.input_paths()
			.iter()
			.any(|path| self.paths.contains(path))
	}
}

/// Frozen plan shared by analysis workers and carried through commit.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransformPlan {
	groups: Vec<PreparedTransform>,
	pub source_guard: SourceGuard,
}

impl TransformPlan {
	pub(crate) fn prepare(
		input: &mut ResolvedInput,
		request: &TransformRequest<'_>,
	) -> Result<Self, MergeError> {
		let adapters = eu4()
			.content_families()
			.iter()
			.filter_map(|descriptor| descriptor.transform_adapter())
			.collect::<Vec<_>>();
		let mut policies = adapters
			.iter()
			.map(|adapter| adapter.reviewed_inputs(request.project))
			.collect::<Result<Vec<_>, _>>()?;
		// Reviewed syntax repairs belong to no content family; they come last
		// and compose with the families' own edits to the same file.
		let repairs = &request.project.repairs;
		policies.push(input::ReviewedInputPolicy {
			label: "syntax",
			sources: Vec::new(),
			repairs: repairs
				.iter()
				.map(|entry| input::SourceRepair {
					source: input::SourceBinding {
						mod_id: &entry.mod_id,
						file: &entry.file,
						sha256: &entry.sha256,
					},
					edits: &entry.edits,
				})
				.collect(),
			validate: None,
		});
		let mut reviewed = input::ReviewedInputBatch::prepare(input, &policies)?.apply(input);
		let syntax = reviewed.pop().expect("one reviewed input per policy");
		let mut plan = Self::default();
		for (adapter, reviewed) in adapters.into_iter().zip(reviewed) {
			for prepared in adapter.prepare(input, request, reviewed)? {
				plan.push(input, prepared)?;
			}
		}
		if !repairs.is_empty() {
			plan.push(
				input,
				PreparedTransform {
					id: "syntax-repairs".into(),
					identity: crate::project::repairs_identity(repairs),
					paths: repairs
						.iter()
						.filter_map(|entry| GamePathBuf::parse(&entry.file).ok())
						.collect(),
					evidence: syntax.evidence,
					source_guard: syntax.source_guard,
					..PreparedTransform::default()
				},
			)?;
		}
		Ok(plan)
	}

	pub(crate) fn push(
		&mut self,
		input: &mut ResolvedInput,
		mut prepared: PreparedTransform,
	) -> Result<(), MergeError> {
		self.source_guard.extend(&prepared.source_guard)?;
		if let Some(transform) = &prepared.entity_transform {
			for path in input.file_inventory.keys() {
				if transform.applies_to(path) && self.entity_transform(path).is_some() {
					return Err(MergeError::Validation {
						subject: Some(MergeErrorSubject::Game(path.clone())),
						message:
							"multiple entity transformations claim the same content-family scope"
								.into(),
					});
				}
			}
		}
		if prepared.findings.is_empty() {
			let mut changed = BTreeSet::new();
			for overlay in &prepared.overlays {
				let prior = &overlay.before;
				let current = input
					.script_cache
					.get(&prior.mod_id, &prior.relative_path)
					.map_err(|message| MergeError::Validation {
						subject: Some(MergeErrorSubject::Game(prior.relative_path.clone())),
						message,
					})?;
				if !changed.insert((&prior.mod_id, &prior.relative_path))
					|| !current.is_some_and(|current| Arc::ptr_eq(&current, prior))
				{
					return Err(MergeError::Validation {
						subject: Some(MergeErrorSubject::Game(prior.relative_path.clone())),
						message: "semantic transformation was not derived from the current frozen document; recompute after the preceding transformation".into(),
					});
				}
			}
			for overlay in prepared.overlays.drain(..) {
				input
					.script_cache
					.insert_overlay(overlay.after, overlay.bytes);
			}
		}
		// Overlays live in the frozen input cache; only their identity and audit remain here.
		prepared.overlays.clear();
		self.groups.push(prepared);
		Ok(())
	}

	/// Register each group's generated files as plan entries, after path
	/// planning and before the outcome ledger is built. A path equal to an
	/// input, a planned output or another generated file under the loader's
	/// separator and case folding is a collision: the group gains a finding
	/// naming the conflict and none of its files are registered. Foch never
	/// picks a replacement name. Windows trailing-dot/space aliases are not
	/// folded yet.
	pub(crate) fn register_generated(
		&mut self,
		inventory: &BTreeMap<GamePathBuf, Vec<ResolvedInputContributor>>,
		plan: &mut MergePlanResult,
	) {
		let mut occupied = inventory
			.keys()
			.map(GamePathBuf::as_game_path)
			.chain(
				plan.paths
					.iter()
					.flat_map(|entry| entry.target.output_paths()),
			)
			.map(|path| (loader_path_key(path), path.to_owned()))
			.collect::<BTreeMap<_, _>>();
		for group in &mut self.groups {
			let mut claimed = BTreeMap::new();
			let mut collisions = Vec::new();
			for artifact in &group.generated {
				let key = loader_path_key(&artifact.path);
				match occupied.get(&key).or_else(|| claimed.get(&key)) {
					Some(existing) => collisions.push(format!(
						"generated output `{}` collides with `{existing}`",
						artifact.path
					)),
					None => {
						claimed.insert(key, artifact.path.clone());
					}
				}
			}
			if !collisions.is_empty() {
				group.findings.extend(collisions);
				continue;
			}
			for artifact in &group.generated {
				plan.paths.push(MergePlanEntry {
					target: MergePlanTarget::File {
						path: artifact.path.clone(),
					},
					strategy: MergePlanStrategy::Generated,
					contributors: Vec::new(),
					winner: None,
					notes: Vec::new(),
				});
				plan.strategies.total_paths += 1;
				plan.strategies.generated += 1;
			}
			occupied.extend(claimed);
		}
	}

	pub(crate) fn generated_bytes(&self, path: &GamePath) -> Option<&[u8]> {
		self.groups
			.iter()
			.flat_map(|group| &group.generated)
			.find(|artifact| artifact.path == path)
			.map(|artifact| artifact.bytes.as_ref())
	}

	/// Findings of groups that own no plan entry, such as a migration whose
	/// only outputs collided. They have no review unit to carry them.
	pub(crate) fn unplaced_findings(&self, plan: &MergePlanResult) -> Vec<String> {
		self.groups
			.iter()
			.filter(|group| !plan.paths.iter().any(|entry| group.affects(entry)))
			.flat_map(|group| group.findings.iter().cloned())
			.collect()
	}

	pub(crate) fn entity_transform(&self, path: &GamePath) -> Option<&dyn EntityTransform> {
		self.groups
			.iter()
			.filter_map(|group| group.entity_transform.as_deref())
			.find(|transform| transform.applies_to(path))
	}

	pub(crate) fn findings(&self, entry: &MergePlanEntry) -> Vec<String> {
		self.groups
			.iter()
			.filter(|group| group.affects(entry))
			.flat_map(|group| group.findings.iter().cloned())
			.collect()
	}

	pub(crate) fn annotate(&self, plan: &mut MergePlanResult) {
		for entry in &mut plan.paths {
			for group in &self.groups {
				if group.affects(entry) {
					entry.notes.extend(group.findings.iter().cloned());
					entry.notes.extend(group.evidence.iter().cloned());
				}
			}
		}
	}

	pub(crate) fn cache_identity(&self, previous: &str) -> String {
		let identities = self
			.groups
			.iter()
			.filter(|group| !group.identity.is_empty())
			.map(|group| (&group.id, &group.identity))
			.collect::<Vec<_>>();
		if identities.is_empty() {
			return previous.to_owned();
		}
		let salt =
			blake3::hash(&serde_json::to_vec(&identities).expect("transform identities serialize"));
		format!("{previous}-transform-{}", salt.to_hex())
	}

	pub(crate) fn fingerprint(&self, prior: Option<&str>) -> Option<String> {
		prior.map(|prior| {
			let identity = self.cache_identity(prior);
			if identity == prior {
				identity
			} else {
				blake3::hash(identity.as_bytes()).to_hex().to_string()
			}
		})
	}

	/// Components are joined by actual merge units, including different input files
	/// which the loader combines into the same definition module.
	pub(crate) fn output_groups<'a>(
		&'a self,
		plan: &'a MergePlanResult,
	) -> Vec<TransformOutputGroup<'a>> {
		let mut groups: Vec<TransformOutputGroup<'a>> = Vec::new();
		for transform in &self.groups {
			let entries = plan
				.paths
				.iter()
				.enumerate()
				.filter_map(|(index, entry)| transform.affects(entry).then_some(index))
				.collect::<BTreeSet<_>>();
			if entries.is_empty() {
				continue;
			}
			let mut group = TransformOutputGroup {
				entries,
				transforms: vec![transform],
			};
			let mut index = 0;
			while index < groups.len() {
				if !groups[index].entries.is_disjoint(&group.entries) {
					let connected = groups.remove(index);
					group.entries.extend(connected.entries);
					group.transforms.extend(connected.transforms);
					// An earlier component can now be connected through the new members.
					index = 0;
				} else {
					index += 1;
				}
			}
			groups.push(group);
		}
		groups
	}
}

fn loader_path_key(path: &GamePath) -> String {
	path.as_str().to_lowercase()
}

pub(crate) struct TransformOutputGroup<'a> {
	pub entries: BTreeSet<usize>,
	transforms: Vec<&'a PreparedTransform>,
}

impl TransformOutputGroup<'_> {
	pub(crate) fn validate_emitted(&self, output: &EmittedOutput) -> Vec<String> {
		self.transforms
			.iter()
			.filter_map(|group| group.validator.as_ref())
			.flat_map(|validator| validator.validate_emitted(output))
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::{
		MergeModuleOutput, MergeModuleOutputs, MergePlanStrategy, MergePlanTarget, MergeUnitId,
	};

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("test game path")
	}

	fn entry(path: &str) -> MergePlanEntry {
		MergePlanEntry {
			target: MergePlanTarget::File {
				path: game_path(path),
			},
			strategy: MergePlanStrategy::StructuralMerge,
			contributors: vec![],
			winner: None,
			notes: vec![],
		}
	}

	#[derive(Debug)]
	struct MissingReference;
	impl OutputValidation for MissingReference {
		fn validate_emitted(&self, output: &EmittedOutput) -> Vec<String> {
			if output
				.scripts
				.iter()
				.any(|(path, _)| path.as_str() == "references.txt")
			{
				vec![]
			} else {
				vec!["reference output is missing".into()]
			}
		}
	}

	fn group(id: &str, paths: &[&str]) -> PreparedTransform {
		PreparedTransform {
			id: id.into(),
			identity: "reviewed-v1".into(),
			paths: paths.iter().map(|path| game_path(path)).collect(),
			..Default::default()
		}
	}

	#[test]
	fn output_dependencies_connect_transitively_without_blocking_independent_transforms() {
		let plan = MergePlanResult {
			paths: vec![entry("a"), entry("b"), entry("c"), entry("independent")],
			..Default::default()
		};
		let mut transforms = TransformPlan {
			groups: vec![
				group("first", &["a"]),
				group("last", &["c"]),
				group("separate", &["independent"]),
				group("ab", &["a", "b"]),
				group("bc", &["b", "c"]),
			],
			..Default::default()
		};
		transforms.groups[0].validator = Some(Arc::new(MissingReference));
		let groups = transforms.output_groups(&plan);
		assert_eq!(groups.len(), 2);
		let connected = groups
			.iter()
			.find(|group| group.entries.contains(&0))
			.unwrap();
		assert_eq!(connected.entries, BTreeSet::from([0, 1, 2]));
		assert_eq!(
			connected.validate_emitted(&EmittedOutput::default()),
			["reference output is missing"]
		);
		let independent = groups
			.iter()
			.find(|group| group.entries.contains(&3))
			.unwrap();
		assert_eq!(independent.entries, BTreeSet::from([3]));
		assert!(
			independent
				.validate_emitted(&EmittedOutput::default())
				.is_empty()
		);
	}

	#[test]
	fn different_input_paths_in_one_loader_module_share_output_dependencies() {
		let mut module = entry("merged.txt");
		module.target = MergePlanTarget::Module {
			id: MergeUnitId {
				family_id: "test".into(),
				module_name: "test".into(),
			},
			input_paths: vec![game_path("left.txt"), game_path("right.txt")],
			outputs: MergeModuleOutputs::new(vec![
				MergeModuleOutput::new(game_path("common/test/merged.txt"), None)
					.expect("module output"),
			])
			.expect("module outputs"),
		};
		let plan = MergePlanResult {
			paths: vec![module, entry("references.txt"), entry("other.txt")],
			..Default::default()
		};
		let transforms = TransformPlan {
			groups: vec![
				group("left", &["left.txt"]),
				group("right", &["right.txt", "references.txt"]),
				group("other", &["other.txt"]),
			],
			..Default::default()
		};
		let groups = transforms.output_groups(&plan);
		assert_eq!(groups.len(), 2);
		assert_eq!(
			groups
				.iter()
				.find(|group| group.entries.contains(&0))
				.unwrap()
				.entries,
			BTreeSet::from([0, 1])
		);
	}

	#[test]
	fn transform_fingerprints_bind_domain_decisions_and_keep_absent_identity_absent() {
		let empty = TransformPlan::default();
		assert_eq!(empty.cache_identity("base"), "base");
		assert_eq!(empty.fingerprint(Some("base")), Some("base".into()));
		let mut plan = TransformPlan {
			groups: vec![group("test.entity", &["a"])],
			..Default::default()
		};
		assert!(plan.fingerprint(None).is_none());
		let original = plan.cache_identity("base");
		plan.groups[0].identity = "reviewed-v2".into();
		assert_ne!(plan.cache_identity("base"), original);
		plan.groups[0].identity = "reviewed-v1".into();
		plan.groups[0].id = "another.entity".into();
		assert_ne!(plan.cache_identity("base"), original);
	}

	#[test]
	fn groups_prepared_together_stay_independent_for_findings_and_outputs() {
		let mut plan = MergePlanResult {
			paths: vec![entry("unsafe.txt"), entry("safe.txt")],
			..Default::default()
		};
		let mut unsafe_group = group("migration.unsafe", &["unsafe.txt"]);
		unsafe_group.findings.push("unknown host context".into());
		let transforms = TransformPlan {
			groups: vec![unsafe_group, group("migration.safe", &["safe.txt"])],
			..Default::default()
		};
		assert_eq!(
			transforms.findings(&plan.paths[0]),
			["unknown host context"]
		);
		assert!(transforms.findings(&plan.paths[1]).is_empty());
		transforms.annotate(&mut plan);
		assert!(plan.paths[1].notes.is_empty());
		assert_eq!(transforms.output_groups(&plan).len(), 2);
	}

	fn artifact(path: &str) -> GeneratedArtifact {
		GeneratedArtifact {
			path: game_path(path),
			bytes: Arc::from(b"generated = yes\n".as_slice()),
		}
	}

	#[test]
	fn generated_artifacts_become_plan_entries_in_their_output_group() {
		let mut plan = MergePlanResult {
			paths: vec![entry("source.txt"), entry("other.txt")],
			..Default::default()
		};
		let mut migration = group("migration", &["source.txt"]);
		migration.generated = vec![artifact("decisions/foch_generated.txt")];
		let mut transforms = TransformPlan {
			groups: vec![migration, group("other", &["other.txt"])],
			..Default::default()
		};
		transforms.register_generated(&BTreeMap::new(), &mut plan);

		let generated = &plan.paths[2];
		assert_eq!(
			generated.output_path().as_str(),
			"decisions/foch_generated.txt"
		);
		assert_eq!(generated.strategy, MergePlanStrategy::Generated);
		assert!(generated.contributors.is_empty() && generated.winner.is_none());
		assert_eq!(plan.strategies.generated, 1);
		assert_eq!(
			transforms.generated_bytes(&game_path("decisions/foch_generated.txt")),
			Some(b"generated = yes\n".as_slice())
		);
		let groups = transforms.output_groups(&plan);
		let connected = groups
			.iter()
			.find(|group| group.entries.contains(&0))
			.unwrap();
		assert_eq!(connected.entries, BTreeSet::from([0, 2]));
	}

	#[test]
	fn generated_path_collisions_defer_the_group_and_name_the_conflict() {
		let mut plan = MergePlanResult {
			paths: vec![entry("source.txt"), entry("Decisions/Taken.txt")],
			..Default::default()
		};
		let inventory = BTreeMap::from([(game_path("events/Existing.txt"), Vec::new())]);
		let mut planned = group("planned", &["source.txt"]);
		planned.generated = vec![artifact("decisions/taken.txt")];
		let mut input = group("input", &[]);
		input.generated = vec![artifact("events/existing.txt")];
		let mut twins = group("twins", &[]);
		twins.generated = vec![artifact("common/a.txt"), artifact("common/A.txt")];
		let mut transforms = TransformPlan {
			groups: vec![planned, input, twins],
			..Default::default()
		};
		transforms.register_generated(&inventory, &mut plan);

		assert_eq!(
			plan.paths.len(),
			2,
			"colliding artifacts are not registered"
		);
		assert_eq!(plan.strategies.generated, 0);
		assert_eq!(
			transforms.findings(&plan.paths[0]),
			["generated output `decisions/taken.txt` collides with `Decisions/Taken.txt`"]
		);
		assert_eq!(
			transforms.unplaced_findings(&plan),
			[
				"generated output `events/existing.txt` collides with `events/Existing.txt`",
				"generated output `common/A.txt` collides with `common/a.txt`",
			]
		);
	}

	#[test]
	fn generated_artifacts_of_a_group_with_findings_are_registered_for_review() {
		let mut plan = MergePlanResult::default();
		let mut unsupported = group("unsupported", &[]);
		unsupported.findings.push("unresolved FROM binding".into());
		unsupported.generated = vec![artifact("decisions/foch_generated.txt")];
		let mut transforms = TransformPlan {
			groups: vec![unsupported],
			..Default::default()
		};
		transforms.register_generated(&BTreeMap::new(), &mut plan);
		assert_eq!(plan.paths.len(), 1);
		assert_eq!(
			transforms.findings(&plan.paths[0]),
			["unresolved FROM binding"]
		);
		assert!(transforms.unplaced_findings(&plan).is_empty());
	}

	#[test]
	fn emitted_output_parses_each_file_by_how_the_game_loads_it() {
		let mut output = EmittedOutput::default();
		assert!(
			output
				.push(&game_path("decisions/foch.txt"), b"foch_decision = { }\n")
				.is_empty()
		);
		assert!(
			output
				.push(
					&game_path("localisation/foch_l_english.yml"),
					"\u{feff}l_english:\n foch_decision_title:0 \"Title\"\n".as_bytes(),
				)
				.is_empty()
		);
		assert!(
			output
				.push(&game_path("gfx/interface/foch.dds"), b"DDS \x00\x01")
				.is_empty()
		);

		assert_eq!(output.scripts[0].0.as_str(), "decisions/foch.txt");
		assert_eq!(
			output.localisation[0].1.entries[0].definition.key,
			"foch_decision_title"
		);
		assert_eq!(output.resources[0].1.len, 6);
		assert_eq!(output.resources[0].1.sha256.len(), 64);

		assert_eq!(
			output.push(&game_path("decisions/broken.txt"), b"foch = {\n"),
			["decisions/broken.txt: emitted transformation has parse errors"]
		);
		assert_eq!(
			output.push(
				&game_path("localisation/broken_l_english.yml"),
				b"key:0 \"no header\"\n"
			),
			["localisation/broken_l_english.yml:1: missing or invalid localisation header"]
		);
	}
}
