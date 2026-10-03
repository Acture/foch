use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;
use crate::game::eu4::script::emit::emit_clausewitz_statements;
use crate::game::eu4::script::parser::{AstFile, parse_clausewitz_content};
use crate::merge::model::{
	SemanticMergeSource, SemanticOrigin, SemanticPartitionId, SemanticPartitionLineage,
};
use crate::merge::structured::{ClausewitzFileAdapter, TreePartitionAdapter};

fn parsed(source: &str) -> AstFile {
	let parsed = parse_clausewitz_content(PathBuf::from("interface/test.gui"), source);
	assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
	parsed.ast
}

fn source(id: &str, precedence: usize) -> SemanticMergeSource {
	SemanticMergeSource {
		source_id: id.to_string(),
		precedence,
	}
}

fn semantic(file: &AstFile, vanilla: bool) -> SemanticMergeComputation {
	let tree = ClausewitzFileAdapter
		.prepare(file)
		.normalize(&SemanticPartitionId::File, &MergePolicies::default())
		.expect("normalize GUI fixture");
	let origins = tree
		.nodes()
		.map(|(id, _)| {
			let origin = if vanilla {
				SemanticOrigin::Vanilla
			} else {
				SemanticOrigin::Mod(source("creator", 0))
			};
			(id, BTreeSet::from([origin]))
		})
		.collect();
	let sources = tree
		.nodes()
		.filter_map(|(id, node)| {
			let adopted = match node.value.as_deref() {
				Some("from_a") => source("mod_a", 10),
				Some("from_b") => source("mod_b", 20),
				_ => return None,
			};
			Some((id, BTreeSet::from([adopted])))
		})
		.collect();
	SemanticMergeComputation {
		statements: file.statements.clone(),
		source_deltas: Vec::new(),
		merge_facts: Vec::new(),
		partition_lineage: BTreeMap::from([(
			SemanticPartitionId::File,
			SemanticPartitionLineage {
				tree,
				sources,
				origins,
			},
		)]),
		unresolved_conflicts: Vec::new(),
		handler_resolutions: Vec::new(),
		resolved_conflict_ids: Vec::new(),
		conflict_resolutions: Vec::new(),
		output_directives: Vec::new(),
	}
}

fn render(file: &AstFile, lineage: &SemanticMergeComputation) -> ProvenanceTooltipOutput {
	materialize_gui_provenance_tooltips(
		true,
		VanillaBaseMode::Required,
		"interface/test.gui",
		file.statements.clone(),
		lineage,
		&MergePolicies::default(),
		&HashMap::from([
			("mod_a".to_string(), "A".to_string()),
			("mod_b".to_string(), "B".to_string()),
		]),
	)
	.expect("materialize GUI provenance")
}

#[test]
fn appends_to_authored_static_tooltips_and_preserves_delayed_channels() {
	let file = parsed(
		r#"guiTypes = { windowType = { name = window
			# Keep attached comments while mapping nested widgets.
			iconType = { name = icon pdx_tooltip = ORIGINAL pdx_tooltip_delayed = DELAYED marker = from_a }
			guiButtonType = { name = button tooltipText = "LEGACY" delayedTooltipText = "LATER" marker = from_b }
		} }"#,
	);
	let output = render(&file, &semantic(&file, true));
	assert_eq!(output.localisation.len(), 2);
	assert!(
		output
			.localisation
			.values()
			.any(|value| value == "$ORIGINAL$\\n\\nMerged from A")
	);
	assert!(
		output
			.localisation
			.values()
			.any(|value| value == "$LEGACY$\\n\\nMerged from B")
	);
	let text = emit_clausewitz_statements(&output.statements).unwrap();
	assert!(text.contains("pdx_tooltip_delayed = DELAYED"), "{text}");
	assert!(text.contains("delayedTooltipText = \"LATER\""), "{text}");
	assert!(text.contains("# Keep attached comments"), "{text}");
}

#[test]
fn only_mod_created_widgets_get_a_previously_absent_tooltip() {
	let file = parsed("guiTypes = { buttonType = { name = new_button marker = from_a } }");
	let output = render(&file, &semantic(&file, false));
	assert_eq!(
		output.localisation.values().collect::<Vec<_>>(),
		vec!["Merged from A"]
	);
	let text = emit_clausewitz_statements(&output.statements).unwrap();
	assert!(
		text.contains("pdx_tooltip = FOCH_PROVENANCE_GUI_"),
		"{text}"
	);
	let vanilla = render(&file, &semantic(&file, true));
	assert_eq!(vanilla.statements, file.statements);
	assert!(vanilla.localisation.is_empty());
}

#[test]
fn repeated_names_and_nested_siblings_keep_independent_sources() {
	let file = parsed(
		r#"guiTypes = { windowType = { name = outer
			iconType = { name = repeated pdx_tooltip = SAME marker = from_b }
			windowType = { name = inner
				iconType = { name = repeated pdx_tooltip = SAME marker = from_a }
				iconType = { name = repeated pdx_tooltip = SAME marker = from_a other_marker = from_b }
			}
		} }"#,
	);
	let output = render(&file, &semantic(&file, true));
	assert_eq!(output.localisation.len(), 3);
	let values = output.localisation.values().collect::<BTreeSet<_>>();
	assert!(values.contains(&"$SAME$\\n\\nMerged from A".to_string()));
	assert!(values.contains(&"$SAME$\\n\\nMerged from B".to_string()));
	assert!(values.contains(&"$SAME$\\n\\nMerged from A, B".to_string()));
	assert_eq!(output, render(&file, &semantic(&file, true)));
}

#[test]
fn unsupported_ambiguous_and_dynamic_channels_remain_unchanged() {
	for widget in [
		"containerWindowType = { name = unsupported marker = from_a }",
		"iconType = { name = unsupported tooltipText = LEGACY marker = from_a }",
		"iconType = { name = generic tooltip = DYNAMIC marker = from_a }",
		"iconType = { name = dynamic pdx_tooltip = \"[Scope.GetText]\" marker = from_a }",
		"iconType = { name = padded pdx_tooltip = \" KEY \" marker = from_a }",
		"iconType = { name = duplicate pdx_tooltip = ONE pdx_tooltip = TWO marker = from_a }",
		"guiButtonType = { name = competing pdx_tooltip = ONE tooltipText = TWO marker = from_a }",
		"iconType = { name = delayed pdx_tooltip_delayed = LATER marker = from_a }",
		"iconType = { name = wrapped pdx_tooltip = FOCH_PROVENANCE_existing marker = from_a }",
		"iconType = { pdx_tooltip = ORIGINAL marker = from_a }",
	] {
		let file = parsed(&format!("guiTypes = {{ {widget} }}"));
		let output = render(&file, &semantic(&file, false));
		assert_eq!(output.statements, file.statements, "{widget}");
		assert!(output.localisation.is_empty(), "{widget}");
	}
}

#[test]
fn transformed_ast_disabled_mode_and_unrelated_paths_are_not_annotated() {
	let file =
		parsed("guiTypes = { iconType = { name = icon pdx_tooltip = ORIGINAL marker = from_a } }");
	let lineage = semantic(&file, true);
	let changed = parsed(
		"guiTypes = { iconType = { name = renamed pdx_tooltip = ORIGINAL marker = from_a } }",
	);
	let output = render(&changed, &lineage);
	assert_eq!(output.statements, changed.statements);
	assert!(output.localisation.is_empty());
	for (enabled, path) in [
		(false, "interface/test.gui"),
		(true, "gfx/test.gui"),
		(true, "interface/test.gfx"),
	] {
		let output = materialize_gui_provenance_tooltips(
			enabled,
			VanillaBaseMode::Required,
			path,
			file.statements.clone(),
			&lineage,
			&MergePolicies::default(),
			&HashMap::new(),
		)
		.unwrap();
		assert_eq!(output.statements, file.statements);
		assert!(output.localisation.is_empty());
	}
}

#[test]
fn blank_fields_are_filled_once_but_incomplete_ancestry_is_not_invented() {
	let file = parsed(
		"guiTypes = { instantTextBoxType = { name = added pdx_tooltip = \"\" tooltipText = \"\" marker = from_a } }",
	);
	let mut lineage = semantic(&file, false);
	let output = render(&file, &lineage);
	assert_eq!(output.localisation.len(), 1);
	let text = emit_clausewitz_statements(&output.statements).unwrap();
	assert_eq!(text.matches("pdx_tooltip =").count(), 1, "{text}");
	assert!(text.contains("tooltipText = \"\""), "{text}");
	lineage
		.partition_lineage
		.get_mut(&SemanticPartitionId::File)
		.unwrap()
		.origins
		.clear();
	let output = render(&file, &lineage);
	assert_eq!(output.statements, file.statements);
	assert!(output.localisation.is_empty());
}

#[test]
fn historical_origins_are_not_credited_as_adopted_sources() {
	let file =
		parsed("guiTypes = { iconType = { name = icon pdx_tooltip = ORIGINAL marker = from_a } }");
	let mut lineage = semantic(&file, false);
	for origins in lineage
		.partition_lineage
		.get_mut(&SemanticPartitionId::File)
		.unwrap()
		.origins
		.values_mut()
	{
		origins.insert(SemanticOrigin::Mod(source("overridden", 5)));
	}
	let output = render(&file, &lineage);
	assert_eq!(
		output.localisation.values().collect::<Vec<_>>(),
		vec!["$ORIGINAL$\\n\\nMerged from A"]
	);
	lineage
		.partition_lineage
		.get_mut(&SemanticPartitionId::File)
		.unwrap()
		.sources
		.clear();
	let output = render(&file, &lineage);
	assert_eq!(output.statements, file.statements);
	assert!(output.localisation.is_empty());
}

#[test]
fn generated_wrappers_are_idempotent_and_bound_to_the_output_path() {
	let file = parsed(
		"guiTypes = { guiButtonType = { name = button pdx_tooltip = ORIGINAL marker = from_a } }",
	);
	let lineage = semantic(&file, true);
	let output = render(&file, &lineage);
	let other_path = materialize_gui_provenance_tooltips(
		true,
		VanillaBaseMode::Required,
		"common/interface/other.gui",
		file.statements.clone(),
		&lineage,
		&MergePolicies::default(),
		&HashMap::new(),
	)
	.unwrap();
	assert_eq!(other_path.localisation.len(), 1);
	assert_ne!(
		output.localisation.keys().collect::<Vec<_>>(),
		other_path.localisation.keys().collect::<Vec<_>>()
	);
	let emitted = AstFile {
		path: file.path,
		statements: output.statements,
	};
	let repeated = render(&emitted, &semantic(&emitted, true));
	assert_eq!(repeated.statements, emitted.statements);
	assert!(repeated.localisation.is_empty());
}

#[test]
fn missing_tooltip_requires_a_verified_ancestor_even_with_mod_only_origins() {
	let file = parsed("guiTypes = { buttonType = { name = added marker = from_a } }");
	let lineage = semantic(&file, false);
	for (mode, expected) in [
		(VanillaBaseMode::Required, 1),
		(VanillaBaseMode::KnownAbsent, 1),
		(VanillaBaseMode::ExplicitlyDisabled, 0),
	] {
		let output = materialize_gui_provenance_tooltips(
			true,
			mode,
			"interface/test.gui",
			file.statements.clone(),
			&lineage,
			&MergePolicies::default(),
			&HashMap::new(),
		)
		.unwrap();
		assert_eq!(output.localisation.len(), expected, "{mode:?}");
	}
	let authored = parsed(
		"guiTypes = { buttonType = { name = added pdx_tooltip = ORIGINAL marker = from_a } }",
	);
	let output = materialize_gui_provenance_tooltips(
		true,
		VanillaBaseMode::ExplicitlyDisabled,
		"interface/test.gui",
		authored.statements.clone(),
		&semantic(&authored, false),
		&MergePolicies::default(),
		&HashMap::new(),
	)
	.unwrap();
	assert_eq!(output.localisation.len(), 1);
}
