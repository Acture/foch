//! Shared product expectations for the synthetic P-553 playsets.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use foch::game::eu4::script::parser::{AstStatement, AstValue};
use foch::game::eu4::script::{ParsedScriptFile, build_semantic_index, parse_script_file};
use foch::model::{
	DeferredUnitReason, LeafConflictDetail, MergeReport, MergeReportConflictResolution,
	MergeReportStatus, SemanticIndex,
};
use foch::playset::descriptor::{ModDescriptor, load_descriptor};

pub const SOURCE: &str = "common/static_modifiers/00_static_modifiers.txt";
pub const OUTPUT: &str = "common/static_modifiers/zzz_foch_static_modifiers.txt";
pub const LOCALISATION: &str = "localisation/p553_l_english.yml";

pub struct Case {
	pub name: &'static str,
	pub mods: &'static [&'static str],
	pub expected: Option<(&'static str, &'static str)>,
	pub adopted: &'static [&'static str],
}

pub const CASES: &[Case] = &[
	Case {
		name: "delete_modify",
		mods: &["tax", "deleted", "vanilla"],
		expected: None,
		adopted: &[],
	},
	Case {
		name: "equivalent_conflicting_candidates",
		mods: &["tax", "equal", "up", "vanilla"],
		expected: None,
		adopted: &[],
	},
	Case {
		name: "independent",
		mods: &["tax", "morale", "vanilla"],
		expected: Some(("0.15", "0.2")),
		adopted: &["tax", "morale"],
	},
	Case {
		name: "reversed_independent",
		mods: &["vanilla", "morale", "tax"],
		expected: Some(("0.15", "0.2")),
		adopted: &["morale", "tax"],
	},
	Case {
		name: "unchanged_vanilla",
		mods: &["tax", "vanilla"],
		expected: Some(("0.15", "0.1")),
		adopted: &["tax"],
	},
	Case {
		name: "equivalent",
		mods: &["tax", "equal", "vanilla"],
		expected: Some(("0.15", "0.1")),
		adopted: &["tax", "equal"],
	},
	Case {
		name: "numeric_divergence",
		mods: &["tax", "up", "vanilla"],
		expected: None,
		adopted: &[],
	},
	Case {
		name: "opposite_changes",
		mods: &["tax", "down", "vanilla"],
		expected: None,
		adopted: &[],
	},
	Case {
		name: "explicit_adapter",
		mods: &["tax", "up", "adapter", "vanilla"],
		expected: Some(("0.25", "0.1")),
		adopted: &["adapter"],
	},
	Case {
		name: "adapter_and_independent",
		mods: &["tax", "morale", "tax_adapter", "vanilla"],
		expected: Some(("0.25", "0.2")),
		adopted: &["morale", "tax_adapter"],
	},
	Case {
		name: "ordinary_last",
		mods: &["tax", "up", "last", "vanilla"],
		expected: None,
		adopted: &[],
	},
];

pub fn write_manifest(fixture: &Path, scratch: &Path, case: &Case) -> PathBuf {
	fs::create_dir_all(scratch).expect("create case scratch directory");
	let mut manifest: String = String::from("[project]\ngame = \"eu4\"\n");
	for id in case.mods {
		let path: PathBuf = fixture.join("mods").join(id);
		// JSON strings are valid TOML basic strings, including Windows paths.
		let encoded: String = serde_json::to_string(&path).expect("encode source path");
		manifest.push_str(&format!(
			"\n[[project.mods]]\nid = \"{id}\"\npath = {encoded}\n"
		));
	}
	let path: PathBuf = scratch.join("foch.toml");
	fs::write(&path, manifest).expect("write ordered playset manifest");
	path
}

pub fn source_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
	walkdir::WalkDir::new(root)
		.into_iter()
		.map(|entry| entry.expect("read fixture tree"))
		.filter(|entry| entry.file_type().is_file())
		.map(|entry| {
			let path: PathBuf = entry.path().strip_prefix(root).unwrap().to_path_buf();
			(path, fs::read(entry.path()).expect("read fixture bytes"))
		})
		.collect()
}

pub fn assert_output(case: &Case, out: &Path, report: &MergeReport) {
	let descriptor: ModDescriptor =
		load_descriptor(&out.join("descriptor.mod")).expect("parse generated descriptor");
	assert!(
		descriptor.replace_path.is_empty(),
		"static modifiers use the existing overlay policy"
	);
	assert!(
		out.join(LOCALISATION).is_file(),
		"safe copy must survive: {}",
		case.name
	);
	assert!(!out.join(SOURCE).exists(), "module input must be consumed");
	if let Some((tax, morale)) = case.expected {
		assert_eq!(
			report.status,
			MergeReportStatus::Ready,
			"{}: {report:#?}",
			case.name
		);
		let parsed: ParsedScriptFile =
			parse_script_file("generated", out, &out.join(OUTPUT)).expect("parse generated module");
		assert!(parsed.parse_issues.is_empty(), "{:?}", parsed.parse_issues);
		let mut actual: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
		for statement in &parsed.ast.statements {
			let AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				assert!(matches!(statement, AstStatement::Comment { .. }));
				continue;
			};
			let mut values: BTreeMap<String, String> = BTreeMap::new();
			for item in items {
				let AstStatement::Assignment {
					key,
					value: AstValue::Scalar { value, .. },
					..
				} = item
				else {
					assert!(matches!(item, AstStatement::Comment { .. }));
					continue;
				};
				let value: f64 = value.as_text().parse().expect("numeric fixture modifier");
				assert!(
					values.insert(key.clone(), value.to_string()).is_none(),
					"duplicate modifier field"
				);
			}
			assert!(
				actual.insert(key.clone(), values).is_none(),
				"duplicate definition"
			);
		}
		assert_eq!(
			actual,
			BTreeMap::from([
				(
					"shared".into(),
					BTreeMap::from([
						("global_tax_modifier".into(), tax.into()),
						("land_morale".into(), morale.into()),
						("discipline".into(), "0.05".into()),
					])
				),
				(
					"base_only".into(),
					BTreeMap::from([("global_manpower_modifier".into(), "0.2".into())])
				),
			]),
			"{}",
			case.name
		);
		let index: SemanticIndex = build_semantic_index(&[parsed]);
		assert!(index.parse_issues.is_empty());
		// Numeric static modifiers have no symbolic references in this extractor.
		assert!(index.references.is_empty());
		let references: Vec<(&str, &str)> = index
			.resource_references
			.iter()
			.map(|reference| (reference.key.as_str(), reference.value.as_str()))
			.collect();
		assert_eq!(
			references,
			[
				("static_modifiers_definition", "base_only"),
				("static_modifiers_definition", "shared"),
			]
		);
		let adopted: &[String] = &report.definition_provenance[OUTPUT]["shared"];
		assert_eq!(
			adopted, case.adopted,
			"{}: adopted contributions",
			case.name
		);
	} else {
		assert_eq!(
			report.status,
			MergeReportStatus::PartialSuccess,
			"{}: {report:#?}",
			case.name
		);
		assert!(!out.join(OUTPUT).exists(), "unsafe module must be withheld");
		assert_eq!(report.manual_conflict_count, 1, "{report:#?}");
		assert_eq!(report.unsupported_input_count, 0);
		assert_eq!(report.engine_failure_count, 0);
		assert_eq!(report.conflict_resolutions.len(), 1, "{report:#?}");
		let conflict: &MergeReportConflictResolution = &report.conflict_resolutions[0];
		assert_eq!(conflict.path, OUTPUT);
		assert_eq!(
			conflict.deferred_reason,
			DeferredUnitReason::NeedsUserChoice
		);
		assert_eq!(conflict.leaf_conflicts.len(), 1, "{conflict:#?}");
		let leaf: &LeafConflictDetail = &conflict.leaf_conflicts[0];
		assert!(!leaf.conflict_id.is_empty());
		assert!(leaf.address_path.contains("shared"), "{leaf:#?}");
		assert_eq!(leaf.address_key, "global_tax_modifier", "{leaf:#?}");
		let contributors: Vec<&str> = leaf
			.contributors
			.iter()
			.map(|source| source.mod_id.as_str())
			.collect();
		let expected: Vec<&str> = case
			.mods
			.iter()
			.copied()
			.filter(|id| *id != "vanilla")
			.collect();
		assert_eq!(
			contributors, expected,
			"only changed final candidates belong in conflict choices"
		);
		assert!(
			leaf.contributors
				.windows(2)
				.all(|pair| pair[0].precedence < pair[1].precedence)
		);
	}
}
