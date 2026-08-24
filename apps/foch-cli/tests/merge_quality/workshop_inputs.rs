//! Logical product-cohort inputs resolved from a read-only Steam Workshop.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use foch::model::{ProductInputManifest, ProductInputMod};
use foch::playset::steam::SteamId;

use crate::merge_quality::config::{WorkshopCatalog, WorkshopItemVersion};
use crate::merge_quality::corpus::Case;
use crate::merge_quality::dataset::{
	GameInputIdentityV2, InputVersionRecord, WorkshopInputVersionV2,
};

pub const WORKSHOP_CASE_MANIFEST_SCHEMA: &str = "2.0.0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopCaseManifest {
	pub schema: String,
	pub app_id: String,
	pub cases: Vec<WorkshopCaseDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopCaseDefinition {
	pub case_id: String,
	pub compatch_workshop_id: String,
	pub source_workshop_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWorkshopItem {
	pub install: WorkshopItemVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedWorkshopCase {
	pub definition: WorkshopCaseDefinition,
	pub compatch: ResolvedWorkshopItem,
	pub sources: Vec<ResolvedWorkshopItem>,
	pub product_manifest: ProductInputManifest,
}

impl WorkshopCaseManifest {
	pub fn from_path(path: &Path) -> io::Result<Self> {
		let raw = fs::read_to_string(path)?;
		Self::from_json(&raw).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
	}

	pub fn from_json(raw: &str) -> Result<Self, String> {
		let manifest = serde_json::from_str::<Self>(raw)
			.map_err(|error| format!("invalid Workshop case manifest JSON: {error}"))?;
		manifest.validate()?;
		Ok(manifest)
	}

	pub fn validate(&self) -> Result<(), String> {
		if self.schema != WORKSHOP_CASE_MANIFEST_SCHEMA {
			return Err(format!(
				"unsupported Workshop case manifest schema {}; expected {WORKSHOP_CASE_MANIFEST_SCHEMA}",
				self.schema
			));
		}
		validate_decimal_id("app_id", &self.app_id)?;
		if self.cases.is_empty() {
			return Err("Workshop case manifest contains no cases".to_string());
		}

		let mut case_ids = BTreeSet::new();
		for case in &self.cases {
			validate_decimal_id("case_id", &case.case_id)?;
			validate_decimal_id("compatch_workshop_id", &case.compatch_workshop_id)?;
			if case.case_id != case.compatch_workshop_id {
				return Err(format!(
					"case {} must use its compatch Workshop ID as case_id",
					case.case_id
				));
			}
			if !case_ids.insert(case.case_id.as_str()) {
				return Err(format!("duplicate Workshop case {}", case.case_id));
			}
			if case.source_workshop_ids.len() < 2 {
				return Err(format!(
					"Workshop case {} must contain at least two source mods",
					case.case_id
				));
			}
			let mut source_ids = BTreeSet::new();
			for source_id in &case.source_workshop_ids {
				validate_decimal_id("source_workshop_id", source_id)?;
				if source_id == &case.compatch_workshop_id {
					return Err(format!(
						"Workshop case {} lists its compatch as a source",
						case.case_id
					));
				}
				if !source_ids.insert(source_id.as_str()) {
					return Err(format!(
						"Workshop case {} contains duplicate source {source_id}",
						case.case_id
					));
				}
			}
		}
		Ok(())
	}

	pub fn required_item_ids(&self) -> BTreeSet<&str> {
		self.cases
			.iter()
			.flat_map(|case| {
				std::iter::once(case.compatch_workshop_id.as_str())
					.chain(case.source_workshop_ids.iter().map(String::as_str))
			})
			.collect()
	}
}

impl WorkshopCaseDefinition {
	pub fn to_case(&self) -> Case {
		Case {
			compatch_id: self.compatch_workshop_id.clone(),
			title: format!("Workshop compatch {}", self.compatch_workshop_id),
			referenced_mods: self.source_workshop_ids.clone(),
			..Case::default()
		}
	}
}

impl ResolvedWorkshopCase {
	pub fn resolve(
		catalog: &WorkshopCatalog,
		definition: &WorkshopCaseDefinition,
	) -> Result<Self, String> {
		let compatch = resolve_item(catalog, &definition.compatch_workshop_id)?;
		let sources = definition
			.source_workshop_ids
			.iter()
			.map(|workshop_id| resolve_item(catalog, workshop_id))
			.collect::<Result<Vec<_>, _>>()?;
		let product_manifest = source_product_manifest(&sources);
		Ok(Self {
			definition: definition.clone(),
			compatch,
			sources,
			product_manifest,
		})
	}

	/// Re-read the exact ACF files after execution. Content trees are never
	/// traversed: Steam's installed-item record is the version authority.
	pub fn validate_unchanged(&self, catalog: &WorkshopCatalog) -> Result<(), String> {
		validate_workshop_cases_unchanged(catalog, std::slice::from_ref(self), |_, _, _| {})
	}

	fn validate_same_input(&self, after: &Self) -> Result<(), String> {
		if after.compatch.install != self.compatch.install
			|| after
				.sources
				.iter()
				.map(|source| &source.install)
				.ne(self.sources.iter().map(|source| &source.install))
		{
			return Err(format!(
				"Steam Workshop ACF changed while measuring case {}",
				self.definition.case_id
			));
		}
		if after.product_manifest != self.product_manifest {
			return Err(format!(
				"Steam Workshop ACF identity changed while measuring case {}",
				self.definition.case_id
			));
		}
		Ok(())
	}

	pub fn case(&self) -> Case {
		self.definition.to_case()
	}

	pub fn input_version(
		&self,
		game_version: &str,
		steam_build_id: Option<u64>,
	) -> Result<InputVersionRecord, String> {
		fn item(resolved: &ResolvedWorkshopItem) -> WorkshopInputVersionV2 {
			WorkshopInputVersionV2 {
				workshop_id: resolved.install.identity.workshop_id.clone(),
				manifest_id: resolved.install.identity.manifest_id.clone(),
			}
		}

		Ok(InputVersionRecord::new(
			self.definition.case_id.clone(),
			GameInputIdentityV2 {
				app_id: SteamId::new(u64::from(crate::merge_quality::config::EU4_APPID)),
				version: game_version.to_string(),
				steam_build_id: steam_build_id.map(SteamId::new),
			},
			item(&self.compatch),
			self.sources.iter().map(item).collect(),
		))
	}

	pub fn source_dirs(&self) -> Vec<PathBuf> {
		self.sources
			.iter()
			.map(|source| source.install.content_path.clone())
			.collect()
	}
}

pub(crate) fn resolve_workshop_cases(
	catalog: &WorkshopCatalog,
	definitions: &[WorkshopCaseDefinition],
	mut on_item: impl FnMut(usize, usize, &str),
) -> Result<Vec<ResolvedWorkshopCase>, String> {
	resolve_workshop_cases_with(definitions, |position, total, workshop_id| {
		on_item(position, total, workshop_id);
		resolve_item(catalog, workshop_id)
	})
}

pub(crate) fn validate_workshop_cases_unchanged(
	catalog: &WorkshopCatalog,
	expected: &[ResolvedWorkshopCase],
	on_item: impl FnMut(usize, usize, &str),
) -> Result<(), String> {
	let reloaded = catalog.reload()?;
	let definitions = expected
		.iter()
		.map(|case| case.definition.clone())
		.collect::<Vec<_>>();
	let actual = resolve_workshop_cases(&reloaded, &definitions, on_item)?;
	for (expected, actual) in expected.iter().zip(&actual) {
		expected.validate_same_input(actual)?;
	}
	Ok(())
}

fn resolve_workshop_cases_with(
	definitions: &[WorkshopCaseDefinition],
	mut resolve: impl FnMut(usize, usize, &str) -> Result<ResolvedWorkshopItem, String>,
) -> Result<Vec<ResolvedWorkshopCase>, String> {
	let required_item_ids = definitions
		.iter()
		.flat_map(|definition| {
			std::iter::once(definition.compatch_workshop_id.as_str())
				.chain(definition.source_workshop_ids.iter().map(String::as_str))
		})
		.collect::<BTreeSet<_>>();
	let total = required_item_ids.len();
	let mut resolved_items = BTreeMap::new();
	for (index, workshop_id) in required_item_ids.into_iter().enumerate() {
		let resolved = resolve(index + 1, total, workshop_id)
			.map_err(|error| format!("failed to resolve Workshop item {workshop_id}: {error}"))?;
		resolved_items.insert(workshop_id.to_string(), resolved);
	}

	definitions
		.iter()
		.map(|definition| resolved_case_from_items(definition, &resolved_items))
		.collect()
}

fn resolved_case_from_items(
	definition: &WorkshopCaseDefinition,
	resolved_items: &BTreeMap<String, ResolvedWorkshopItem>,
) -> Result<ResolvedWorkshopCase, String> {
	fn resolved_item(
		resolved_items: &BTreeMap<String, ResolvedWorkshopItem>,
		workshop_id: &str,
	) -> Result<ResolvedWorkshopItem, String> {
		resolved_items
			.get(workshop_id)
			.cloned()
			.ok_or_else(|| format!("Workshop cohort cache is missing item {workshop_id}"))
	}

	let compatch = resolved_item(resolved_items, &definition.compatch_workshop_id)?;
	let sources = definition
		.source_workshop_ids
		.iter()
		.map(|workshop_id| resolved_item(resolved_items, workshop_id))
		.collect::<Result<Vec<_>, _>>()?;
	let product_manifest = source_product_manifest(&sources);
	Ok(ResolvedWorkshopCase {
		definition: definition.clone(),
		compatch,
		sources,
		product_manifest,
	})
}

fn resolve_item(
	catalog: &WorkshopCatalog,
	workshop_id: &str,
) -> Result<ResolvedWorkshopItem, String> {
	let install = catalog.require_item(workshop_id)?;
	Ok(ResolvedWorkshopItem { install })
}

fn source_product_manifest(sources: &[ResolvedWorkshopItem]) -> ProductInputManifest {
	ProductInputManifest::new(
		sources
			.iter()
			.enumerate()
			.map(|(index, source)| ProductInputMod {
				mod_id: source.install.identity.workshop_id.to_string(),
				precedence: index + 1,
				workshop_identity: source.install.identity.clone(),
			})
			.collect(),
	)
}

fn validate_decimal_id(field: &str, value: &str) -> Result<(), String> {
	if value.is_empty()
		|| !value.bytes().all(|byte| byte.is_ascii_digit())
		|| (value.len() > 1 && value.starts_with('0'))
	{
		return Err(format!("{field} must be a canonical decimal string"));
	}
	value
		.parse::<u64>()
		.map_err(|_| format!("{field} is outside the unsigned 64-bit range"))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn synthetic_item(workshop_id: &str) -> ResolvedWorkshopItem {
		ResolvedWorkshopItem {
			install: WorkshopItemVersion {
				identity: foch::playset::steam::WorkshopInstallIdentity {
					app_id: crate::merge_quality::config::EU4_APPID,
					workshop_id: workshop_id.parse().unwrap(),
					manifest_id: "1".parse().unwrap(),
				},
				size_bytes: 1,
				time_updated: 1,
				ugc_handle: None,
				content_path: PathBuf::from("workshop").join(workshop_id),
				manifest_path: PathBuf::from("appworkshop_236850.acf"),
			},
		}
	}

	#[test]
	fn fixed_workshop_product_manifest_is_exactly_fourteen_cases() {
		let manifest = WorkshopCaseManifest::from_json(include_str!(
			"fixtures/workshop-product-cases-v2.json"
		))
		.unwrap();
		assert_eq!(manifest.app_id, "236850");
		assert_eq!(manifest.cases.len(), 14);
		assert_eq!(manifest.required_item_ids().len(), 26);
		for retired in [
			"2095475587",
			"788906833",
			"1884376477",
			"2804377099",
			"253263609",
			"1088606279",
			"1449952810",
			"2469419235",
			"276456014",
		] {
			assert!(manifest.cases.iter().all(|case| case.case_id != retired));
		}
	}

	#[test]
	fn resolve_and_revalidate_read_only_acf_not_mod_contents() {
		let temp = tempfile::tempdir().unwrap();
		let library = temp.path().join("Library");
		let content_root = library.join("steamapps/workshop/content/236850");
		let manifest_path = library.join("steamapps/workshop/appworkshop_236850.acf");
		for workshop_id in ["42", "43", "44"] {
			fs::create_dir_all(content_root.join(workshop_id)).unwrap();
		}
		// A directory at descriptor.mod is deliberately invalid as product
		// content. ACF-only discovery must not inspect it.
		fs::create_dir(content_root.join("42/descriptor.mod")).unwrap();
		fs::write(
			&manifest_path,
			r#""AppWorkshop"
{
	"appid" "236850"
	"WorkshopItemsInstalled"
	{
		"42" { "size" "12" "timeupdated" "1780000000" "manifest" "4200" }
		"43" { "size" "13" "timeupdated" "1780000001" "manifest" "4300" }
		"44" { "size" "14" "timeupdated" "1780000002" "manifest" "4400" }
	}
}"#,
		)
		.unwrap();
		let catalog = WorkshopCatalog::from_override(
			crate::merge_quality::config::EU4_APPID,
			&content_root,
			&manifest_path,
		)
		.unwrap();
		let definition = WorkshopCaseDefinition {
			case_id: "42".to_string(),
			compatch_workshop_id: "42".to_string(),
			source_workshop_ids: vec!["43".to_string(), "44".to_string()],
		};

		let resolved = ResolvedWorkshopCase::resolve(&catalog, &definition).unwrap();
		assert_eq!(resolved.product_manifest.mods.len(), 2);
		assert_eq!(resolved.product_manifest.mods[0].mod_id, "43");
		fs::write(
			content_root.join("43/changed-after-resolution.bin"),
			b"changed",
		)
		.unwrap();
		resolved.validate_unchanged(&catalog).unwrap();
	}

	#[test]
	fn cohort_resolution_reads_unique_acf_items_once_and_preserves_case_manifests() {
		let manifest = WorkshopCaseManifest::from_json(include_str!(
			"fixtures/workshop-product-cases-v2.json"
		))
		.unwrap();
		let item_slots = manifest
			.cases
			.iter()
			.map(|case| 1 + case.source_workshop_ids.len())
			.sum::<usize>();
		assert_eq!(item_slots, 48);

		let expected_item_ids = manifest
			.required_item_ids()
			.into_iter()
			.map(str::to_string)
			.collect::<Vec<_>>();
		let mut resolutions = BTreeMap::<String, usize>::new();
		let mut resolution_order = Vec::new();
		let resolved =
			resolve_workshop_cases_with(&manifest.cases, |position, total, workshop_id| {
				assert_eq!(position, resolution_order.len() + 1);
				assert_eq!(total, expected_item_ids.len());
				resolution_order.push(workshop_id.to_string());
				*resolutions.entry(workshop_id.to_string()).or_default() += 1;
				Ok(synthetic_item(workshop_id))
			})
			.unwrap();

		assert_eq!(expected_item_ids.len(), 26);
		assert_eq!(resolution_order, expected_item_ids);
		assert_eq!(resolutions.len(), 26);
		assert!(resolutions.values().all(|count| *count == 1));
		assert_eq!(resolved.len(), manifest.cases.len());
		for (case, definition) in resolved.iter().zip(&manifest.cases) {
			assert_eq!(&case.definition, definition);
			assert_eq!(
				case.compatch,
				synthetic_item(&definition.compatch_workshop_id)
			);
			let expected_sources = definition
				.source_workshop_ids
				.iter()
				.map(|workshop_id| synthetic_item(workshop_id))
				.collect::<Vec<_>>();
			let expected_manifest = source_product_manifest(&expected_sources);
			assert_eq!(case.sources, expected_sources);
			assert_eq!(case.product_manifest, expected_manifest);
			assert!(case.product_manifest.digest_is_valid());
		}
	}

	#[test]
	fn rejects_noncanonical_or_ambiguous_cases() {
		let duplicate = r#"{
			"schema":"2.0.0",
			"app_id":"0236850",
			"cases":[{
				"case_id":"42",
				"compatch_workshop_id":"42",
				"source_workshop_ids":["1","1"]
			}]
		}"#;
		assert!(WorkshopCaseManifest::from_json(duplicate).is_err());
	}
}
