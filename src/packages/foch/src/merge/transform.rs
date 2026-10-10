//! Execute content-family transformations without interpreting their domain semantics.

pub(crate) mod input;
pub(crate) mod tree;

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::game::eu4::content::{DuplicateDefinitionPolicy, eu4};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::emit::EmitOptions;
use crate::game::eu4::script::parser::AstFile;
use crate::input::ResolvedInput;
use crate::merge::dag::IgnoreReplacePath;
use crate::merge::error::{MergeError, MergeErrorSubject};
use crate::model::{GamePath, GamePathBuf, MergePlanEntry, MergePlanResult};
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

	fn prepare(
		&self,
		input: &mut ResolvedInput,
		request: &TransformRequest<'_>,
		reviewed: input::ReviewedInputs,
	) -> Result<PreparedTransform, MergeError>;
}

/// Domain-specific audit of the actual effective output, after structural merging.
pub(crate) trait OutputValidation: std::fmt::Debug + Send + Sync {
	fn validate_emitted(&self, documents: &[(GamePathBuf, AstFile)]) -> Vec<String>;
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
		let policies = adapters
			.iter()
			.map(|adapter| adapter.reviewed_inputs(request.project))
			.collect::<Result<Vec<_>, _>>()?;
		let reviewed = input::ReviewedInputBatch::prepare(input, &policies)?.apply(input);
		let mut plan = Self::default();
		for (adapter, reviewed) in adapters.into_iter().zip(reviewed) {
			let prepared = adapter.prepare(input, request, reviewed)?;
			plan.push(input, prepared)?;
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

pub(crate) struct TransformOutputGroup<'a> {
	pub entries: BTreeSet<usize>,
	transforms: Vec<&'a PreparedTransform>,
}

impl TransformOutputGroup<'_> {
	pub(crate) fn validate_emitted(&self, documents: &[(GamePathBuf, AstFile)]) -> Vec<String> {
		self.transforms
			.iter()
			.filter_map(|group| group.validator.as_ref())
			.flat_map(|validator| validator.validate_emitted(documents))
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
		fn validate_emitted(&self, documents: &[(GamePathBuf, AstFile)]) -> Vec<String> {
			if documents
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
			connected.validate_emitted(&[]),
			["reference output is missing"]
		);
		let independent = groups
			.iter()
			.find(|group| group.entries.contains(&3))
			.unwrap();
		assert_eq!(independent.entries, BTreeSet::from([3]));
		assert!(independent.validate_emitted(&[]).is_empty());
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
}
