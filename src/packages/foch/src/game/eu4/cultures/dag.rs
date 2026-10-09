//! Collect culture identity evidence from the same dependency parents used by merging.

use std::collections::{BTreeMap, HashMap};

use crate::game::eu4::content::MergePolicies;
use crate::game::eu4::cultures::correspondence::{CultureCorrespondence, CultureRevision};
use crate::game::eu4::cultures::{CultureFinding, CultureIndex};
use crate::game::eu4::script::ParsedScriptFile;
use crate::game::eu4::script::parser::{AstFile, AstStatement};
use crate::merge::structured::{TransformModuleAdapter, TreeJoinProtocol};

use crate::merge::planning::dag::{FileDag, ModId};
use crate::merge::planning::dag_join::DagJoinScope;
use crate::merge::planning::dag_pipeline::{
	DagJoinProtocol, DagJoinRequest, EffectiveNodeProtocol, EffectiveNodeRequest,
	execute_dag_pipeline,
};
use crate::merge::planning::module_view::CrossFileModuleViews;

#[derive(Clone, Debug, Default)]
pub(crate) struct ReviewedCultureMappings {
	by_mod: BTreeMap<ModId, BTreeMap<String, String>>,
}

impl ReviewedCultureMappings {
	/// The caller must first verify every entry's SHA256 against original source bytes.
	pub(crate) fn from_verified_entries(
		entries: &[crate::project::CultureRenameEntry],
		views: &CrossFileModuleViews,
	) -> Result<Self, Vec<CultureFinding>> {
		crate::project::CultureConfig {
			renames: entries.to_vec(),
			repairs: vec![],
		}
		.validate()
		.map_err(|error| vec![reviewed_finding(error.to_string())])?;
		let mut result = Self::default();
		for entry in entries {
			let mod_id = ModId(entry.mod_id.clone());
			let view = views.contributors.get(&mod_id).ok_or_else(|| {
				vec![reviewed_finding(format!(
					"reviewed culture contributor `{}` has no active module view",
					entry.mod_id
				))]
			})?;
			let index = catalog(&view.ast).map_err(|message| vec![reviewed_finding(message)])?;
			let target = index.definitions.get(&entry.to).ok_or_else(|| {
				vec![reviewed_finding(format!(
					"reviewed target `{}` does not survive module loading for {}:{}",
					entry.to, entry.mod_id, entry.file
				))]
			})?;
			let source = views
				.definition_sources
				.get(&mod_id)
				.and_then(|sources| sources.get(&target.group));
			if !source.is_some_and(|winner| {
				winner.mod_id == entry.mod_id && winner.source.path.as_str() == entry.file
			}) {
				return Err(vec![reviewed_finding(format!(
					"reviewed culture target `{}` is not owned by the winning definition from {}:{}",
					entry.to, entry.mod_id, entry.file
				))]);
			}
			result
				.by_mod
				.entry(mod_id)
				.or_default()
				.insert(entry.from.clone(), entry.to.clone());
		}
		Ok(result)
	}
}

pub(crate) fn culture_correspondence_for_dag(
	file_dag: &FileDag,
	vanilla: Option<&ParsedScriptFile>,
	contributors: &HashMap<ModId, ParsedScriptFile>,
	policies: &MergePolicies,
	reviewed: &ReviewedCultureMappings,
) -> Result<Option<CultureCorrespondence>, Vec<CultureFinding>> {
	let path = file_dag.file_path().to_owned();
	if !CultureCorrespondence::applies_to(&path) {
		return Ok(None);
	}
	let base = AstFile {
		path,
		statements: vanilla.map_or_else(Vec::new, |file| file.ast.statements.clone()),
	};
	let mut protocol = CultureObservation {
		base: catalog(&base).map_err(|message| vec![finding(message)])?,
		observations: Vec::new(),
		policies,
		reviewed,
	};
	execute_dag_pipeline(file_dag, contributors, base, &mut protocol)
		.map_err(|message| vec![finding(message)])?;
	protocol.correspondence().map(Some)
}

struct CultureObservation<'a> {
	base: CultureIndex,
	observations: Vec<(CultureIndex, CultureIndex, BTreeMap<String, String>)>,
	policies: &'a MergePolicies,
	reviewed: &'a ReviewedCultureMappings,
}

impl CultureObservation<'_> {
	fn correspondence(&self) -> Result<CultureCorrespondence, Vec<CultureFinding>> {
		CultureCorrespondence::infer_observations(
			&self.base,
			&self
				.observations
				.iter()
				.map(|(parent, revision, reviewed)| CultureRevision {
					parent,
					revision,
					reviewed,
				})
				.collect::<Vec<_>>(),
		)
	}
}

impl EffectiveNodeProtocol<AstFile> for CultureObservation<'_> {
	fn effective_node(
		&mut self,
		request: EffectiveNodeRequest<'_, AstFile>,
	) -> Result<AstFile, String> {
		let revision = AstFile {
			path: request.parent.path.clone(),
			statements: request.source.ast.statements.clone(),
		};
		let mut parent = catalog(request.parent)?;
		let after = catalog(&revision)?;
		let reviewed = self
			.reviewed
			.by_mod
			.get(request.mod_id)
			.cloned()
			.unwrap_or_default();
		if request.resets_base {
			// Omitting an entire loader group does not prove a rename in a sparse reset layer.
			parent.definitions.retain(|id, definition| {
				after.groups.contains_key(&definition.group) || reviewed.contains_key(id)
			});
		}
		self.observations.push((parent, after, reviewed));
		Ok(revision)
	}
}

impl DagJoinProtocol<AstFile> for CultureObservation<'_> {
	fn join(&mut self, request: DagJoinRequest<'_, AstFile>) -> Result<AstFile, String> {
		let correspondence = self
			.correspondence()
			.map_err(|findings| format!("{findings:?}"))?;
		let mut revisions = request
			.revisions
			.iter()
			.map(|revision| revision.state.clone())
			.collect::<Vec<_>>();
		let retained = revisions
			.iter()
			.flat_map(|revision| &revision.statements)
			.filter_map(|statement| match statement {
				AstStatement::Assignment { key, .. } => Some(key.clone()),
				_ => None,
			})
			.collect::<std::collections::BTreeSet<_>>();
		for (source, revision) in request.revisions.iter().zip(&mut revisions) {
			if !request.file_dag.replaces_path(source.mod_id) {
				continue;
			}
			let present = revision
				.statements
				.iter()
				.filter_map(|statement| match statement {
					AstStatement::Assignment { key, .. } => Some(key.clone()),
					_ => None,
				})
				.collect::<std::collections::BTreeSet<_>>();
			revision.statements.extend(
				request
					.base
					.statements
					.iter()
					.filter(|statement| {
						let AstStatement::Assignment { key, .. } = statement else {
							return false;
						};
						retained.contains(key) && !present.contains(key)
					})
					.cloned(),
			);
			correspondence
				.validate_catalog(&catalog(revision)?)
				.map_err(|findings| format!("{findings:?}"))?;
		}
		// Validate final padding, but leave final merge resolution to the product pass.
		if request.plan.scope() == DagJoinScope::Final {
			return Ok(request.base.clone());
		}
		let result = TransformModuleAdapter {
			transform: &correspondence,
		}
		.merge_n_way(
			request.base,
			&revisions.iter().collect::<Vec<_>>(),
			self.policies,
			&[],
		)?;
		if !result.conflicts.is_empty() {
			return Err("culture dependency parent has unresolved conflicts; identity correspondence requires review".into());
		}
		Ok(AstFile {
			path: request.base.path.clone(),
			statements: result.statements,
		})
	}
}

fn catalog(file: &AstFile) -> Result<CultureIndex, String> {
	CultureIndex::from_documents(&[(file.path.clone(), file.clone())])
		.map_err(|findings| format!("invalid culture parent view: {findings:?}"))
}

fn finding(message: String) -> CultureFinding {
	CultureFinding {
		code: "unresolved_culture_parent_view",
		message,
		location: None,
	}
}

fn reviewed_finding(message: String) -> CultureFinding {
	CultureFinding {
		code: "invalid_reviewed_culture_mapping",
		message,
		location: None,
	}
}
