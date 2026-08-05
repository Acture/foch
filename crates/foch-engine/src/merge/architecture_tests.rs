const SEMANTIC_PIPELINE_SOURCES: &[(&str, &str)] = &[
	("model.rs", include_str!("model.rs")),
	("planning/dag.rs", include_str!("planning/dag.rs")),
	(
		"planning/dag_input.rs",
		include_str!("planning/dag_input.rs"),
	),
	("planning/dag_join.rs", include_str!("planning/dag_join.rs")),
	(
		"planning/dag_pipeline.rs",
		include_str!("planning/dag_pipeline.rs"),
	),
	(
		"planning/definition_trace.rs",
		include_str!("planning/definition_trace.rs"),
	),
	(
		"planning/module_view.rs",
		include_str!("planning/module_view.rs"),
	),
	(
		"structured/ast_adapter.rs",
		include_str!("structured/ast_adapter.rs"),
	),
	(
		"structured/control_flow.rs",
		include_str!("structured/control_flow.rs"),
	),
	(
		"structured/definition_module.rs",
		include_str!("structured/definition_module.rs"),
	),
	("structured/merge.rs", include_str!("structured/merge.rs")),
	("structured/policy.rs", include_str!("structured/policy.rs")),
	(
		"structured/tree_kernel.rs",
		include_str!("structured/tree_kernel.rs"),
	),
	(
		"structured/observer.rs",
		include_str!("structured/observer.rs"),
	),
	("structured/trivia.rs", include_str!("structured/trivia.rs")),
	(
		"resolution/conflict_handler.rs",
		include_str!("resolution/conflict_handler.rs"),
	),
	(
		"resolution/conflict_view.rs",
		include_str!("resolution/conflict_view.rs"),
	),
	(
		"resolution/handler_registry.rs",
		include_str!("resolution/handler_registry.rs"),
	),
	(
		"output/materialize/structural.rs",
		include_str!("output/materialize/structural.rs"),
	),
	(
		"output/materialize/per_entry_noop.rs",
		include_str!("output/materialize/per_entry_noop.rs"),
	),
];

const ADDRESS_PATCH_DEPENDENCIES: &[&str] = &[
	"ClausewitzPatch",
	"PatchAddress",
	"PatchConflict",
	"PatchResolution",
	"PatchMergeResult",
	"PatchMergeStats",
	"PatchBaselineDagProtocol",
	"ModDiffCache",
	"DagBaseCache",
	"patch_engine::",
];

const DAG_MERGE_FACADE_DEPENDENCIES: &[&str] = &[
	"DagMergeEvaluation",
	"MergeKernelMode",
	"ReferenceDag",
	"compute_reference",
];

#[test]
fn semantic_pipeline_does_not_depend_on_address_patch_types() {
	for (path, source) in SEMANTIC_PIPELINE_SOURCES {
		assert_source_excludes(path, source, ADDRESS_PATCH_DEPENDENCIES);
	}
	let dag_merge_source = include_str!("planning/dag_merge.rs")
		.split("// Tests")
		.next()
		.expect("semantic DAG merge production source");
	assert_source_excludes(
		"planning/dag_merge.rs",
		dag_merge_source,
		ADDRESS_PATCH_DEPENDENCIES,
	);
	assert_source_excludes(
		"planning/dag_merge.rs",
		dag_merge_source,
		DAG_MERGE_FACADE_DEPENDENCIES,
	);
	assert_source_excludes(
		"planning/module_view.rs",
		include_str!("planning/module_view.rs"),
		&["MergeKernelMode", "MergeEvaluationKernel"],
	);
	assert_source_excludes(
		"output/materialize/structural.rs",
		include_str!("output/materialize/structural.rs"),
		&["MergeKernelMode", "MergeEvaluationKernel", "reference::"],
	);
}

fn assert_source_excludes(path: &str, source: &str, dependencies: &[&str]) {
	for dependency in dependencies {
		assert!(
			!source.contains(dependency),
			"{path} must not depend on forbidden symbol {dependency}"
		);
	}
}
