use std::collections::BTreeSet;
use std::time::Instant;

use foch::merge::kernel::{ConflictResolution, StructuralConflict};
use foch_language::analyzer::content_family::{MergePolicies, OneSidedRemovalPolicy};
use foch_language::analyzer::parser::{AstFile, AstStatement, AstValue};

use crate::merge::model::SemanticPartitionId;

use super::trivia::{attach_trivia, detach_trivia, merge_trivia, merge_trivia_n_way};
use super::{
	AstAdapterError, ClausewitzKernelFacts, ClausewitzMergeTimings, ClausewitzScalarReduction,
	merge_clausewitz_files, merge_clausewitz_files_n_way,
	merge_clausewitz_files_n_way_with_resolutions,
};

#[derive(Clone, Debug)]
pub struct ClausewitzDefinitionModuleOutcome {
	tentative_ast: AstFile,
	conflicts: Vec<StructuralConflict>,
	scalar_reductions: Vec<ClausewitzScalarReduction>,
	kernel_facts: Vec<ClausewitzKernelFacts>,
	timings: ClausewitzMergeTimings,
	base_definitions: usize,
	active_definitions: usize,
	copy_through_definitions: usize,
	structured_definitions: usize,
}

impl ClausewitzDefinitionModuleOutcome {
	pub fn resolved_ast(&self) -> Option<&AstFile> {
		self.conflicts.is_empty().then_some(&self.tentative_ast)
	}

	pub const fn tentative_ast(&self) -> &AstFile {
		&self.tentative_ast
	}

	pub fn conflicts(&self) -> &[StructuralConflict] {
		&self.conflicts
	}

	pub fn scalar_reductions(&self) -> &[ClausewitzScalarReduction] {
		&self.scalar_reductions
	}

	pub(crate) fn into_tree_parts(
		self,
	) -> (AstFile, Vec<StructuralConflict>, Vec<ClausewitzKernelFacts>) {
		(self.tentative_ast, self.conflicts, self.kernel_facts)
	}

	pub const fn timings(&self) -> ClausewitzMergeTimings {
		self.timings
	}

	pub const fn base_definitions(&self) -> usize {
		self.base_definitions
	}

	pub const fn active_definitions(&self) -> usize {
		self.active_definitions
	}

	pub const fn copy_through_definitions(&self) -> usize {
		self.copy_through_definitions
	}

	pub const fn structured_definitions(&self) -> usize {
		self.structured_definitions
	}
}

/// Merge one complete assignment-key definition module. Independent top-level
/// definitions are partitioned for matching, then reassembled deterministically.
pub fn merge_clausewitz_definition_module(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzDefinitionModuleOutcome, AstAdapterError> {
	merge_clausewitz_definition_module_inner(base, left, right, policies)
}

pub(crate) fn merge_clausewitz_definition_module_n_way_with_resolutions(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzDefinitionModuleOutcome, AstAdapterError> {
	merge_clausewitz_definition_module_n_way_inner(base, revisions, policies, resolutions)
}

fn merge_clausewitz_definition_module_n_way_inner(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	resolutions: &[ConflictResolution],
) -> Result<ClausewitzDefinitionModuleOutcome, AstAdapterError> {
	if revisions.is_empty() {
		return Err(AstAdapterError::InvalidTree(
			"definition-module merge requires at least one revision".to_string(),
		));
	}
	let base_definitions = top_level_assignment_keys(base).len();
	if std::iter::once(base)
		.chain(revisions.iter().copied())
		.any(has_top_level_items)
	{
		let outcome = merge_files_n_way(base, revisions, policies, resolutions)?;
		let active_definitions = std::iter::once(base)
			.chain(revisions.iter().copied())
			.flat_map(top_level_assignment_keys)
			.collect::<BTreeSet<_>>()
			.len();
		let conflicts = outcome.conflicts().to_vec();
		let scalar_reductions = outcome.scalar_reductions();
		let timings = outcome.timings();
		let (tentative_ast, kernel_fact) = outcome.into_parts(SemanticPartitionId::File);
		return Ok(ClausewitzDefinitionModuleOutcome {
			tentative_ast,
			conflicts,
			scalar_reductions,
			kernel_facts: vec![kernel_fact],
			timings,
			base_definitions,
			active_definitions,
			copy_through_definitions: 0,
			structured_definitions: 1,
		});
	}

	let (base, base_trivia) = detach_trivia(base);
	let detached_revisions = revisions
		.iter()
		.map(|revision| detach_trivia(revision))
		.collect::<Vec<_>>();
	let revision_files = detached_revisions
		.iter()
		.map(|(file, _)| file)
		.collect::<Vec<_>>();
	let revision_trivia = detached_revisions
		.iter()
		.map(|(_, trivia)| trivia)
		.collect::<Vec<_>>();
	let keys = std::iter::once(&base)
		.chain(revision_files.iter().copied())
		.flat_map(top_level_assignment_keys)
		.collect::<BTreeSet<_>>();
	let mut statements = Vec::new();
	let mut conflicts = Vec::new();
	let mut scalar_reductions = Vec::new();
	let mut kernel_facts = Vec::new();
	let mut timings = ClausewitzMergeTimings::default();
	let mut active_definitions = 0;
	let copy_through_definitions = 0;
	let mut structured_definitions = 0;
	let total = keys.len();
	let progress_step = (total / 10).max(1);
	let started = Instant::now();
	for (index, key) in keys.into_iter().enumerate() {
		let base_group = select_definition(&base, &key);
		let revision_groups = revision_files
			.iter()
			.map(|revision| select_definition(revision, &key))
			.collect::<Vec<_>>();
		if revision_groups
			.iter()
			.all(|revision| statements_content_equal(&base_group.statements, &revision.statements))
		{
			statements.extend(base_group.statements);
		} else {
			active_definitions += 1;
			let revision_group_refs = revision_groups.iter().collect::<Vec<_>>();
			let partition_resolutions = resolutions
				.iter()
				.filter(|resolution| resolution.conflict.semantic_path.first() == Some(&key))
				.cloned()
				.collect::<Vec<_>>();
			let outcome = merge_files_n_way(
				&base_group,
				&revision_group_refs,
				policies,
				&partition_resolutions,
			)?;
			conflicts.extend(outcome.conflicts().iter().cloned().map(|mut conflict| {
				conflict.detail = format!("definition `{key}`: {}", conflict.detail);
				conflict
			}));
			scalar_reductions.extend(outcome.scalar_reductions());
			let partition_timings = outcome.timings();
			timings.matcher_ns = timings
				.matcher_ns
				.saturating_add(partition_timings.matcher_ns);
			timings.delta_ns = timings.delta_ns.saturating_add(partition_timings.delta_ns);
			timings.pcs_ns = timings.pcs_ns.saturating_add(partition_timings.pcs_ns);
			timings.policy_ns = timings
				.policy_ns
				.saturating_add(partition_timings.policy_ns);
			let (partition_ast, kernel_fact) =
				outcome.into_parts(SemanticPartitionId::Definition(key.clone()));
			statements.extend(partition_ast.statements);
			kernel_facts.push(kernel_fact);
			structured_definitions += 1;
		}

		let completed = index + 1;
		if total >= 20 && (completed == 1 || completed == total || completed % progress_step == 0) {
			let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
			let eta_ms = elapsed_ms.saturating_mul((total - completed) as u64) / completed as u64;
			eprintln!(
				"[structured-module] {} definitions {completed}/{total} active={active_definitions} copy_through={copy_through_definitions} structured={structured_definitions} elapsed_ms={elapsed_ms} eta_ms={eta_ms}",
				base.path.display(),
			);
		}
	}
	statements.sort_by(compare_top_level_statements);
	let mut tentative_ast = AstFile {
		path: base.path.clone(),
		statements,
	};
	let trivia = merge_trivia_n_way(&base_trivia, &revision_trivia);
	attach_trivia(&mut tentative_ast, &trivia);
	Ok(ClausewitzDefinitionModuleOutcome {
		tentative_ast,
		conflicts,
		scalar_reductions,
		kernel_facts,
		timings,
		base_definitions,
		active_definitions,
		copy_through_definitions,
		structured_definitions,
	})
}

fn merge_clausewitz_definition_module_inner(
	base: &AstFile,
	left: &AstFile,
	right: &AstFile,
	policies: &MergePolicies,
) -> Result<ClausewitzDefinitionModuleOutcome, AstAdapterError> {
	let base_definitions = top_level_assignment_keys(base).len();
	if [base, left, right]
		.iter()
		.any(|file| has_top_level_items(file))
	{
		let outcome = merge_clausewitz_files(base, left, right, policies)?;
		let conflicts = outcome.conflicts().to_vec();
		let scalar_reductions = outcome.scalar_reductions();
		let timings = outcome.timings();
		let (tentative_ast, kernel_fact) = outcome.into_parts(SemanticPartitionId::File);
		return Ok(ClausewitzDefinitionModuleOutcome {
			tentative_ast,
			conflicts,
			scalar_reductions,
			kernel_facts: vec![kernel_fact],
			timings,
			base_definitions,
			active_definitions: top_level_assignment_keys(base)
				.into_iter()
				.chain(top_level_assignment_keys(left))
				.chain(top_level_assignment_keys(right))
				.collect::<BTreeSet<_>>()
				.len(),
			copy_through_definitions: 0,
			structured_definitions: 1,
		});
	}
	let (base, base_trivia) = detach_trivia(base);
	let (left, left_trivia) = detach_trivia(left);
	let (right, right_trivia) = detach_trivia(right);

	let keys = top_level_assignment_keys(&base)
		.into_iter()
		.chain(top_level_assignment_keys(&left))
		.chain(top_level_assignment_keys(&right))
		.collect::<BTreeSet<_>>();
	let mut statements = Vec::new();
	let mut conflicts = Vec::new();
	let mut scalar_reductions = Vec::new();
	let mut kernel_facts = Vec::new();
	let mut timings = ClausewitzMergeTimings::default();
	let mut active_definitions = 0;
	let mut copy_through_definitions = 0;
	let mut structured_definitions = 0;
	let total = keys.len();
	let progress_step = (total / 10).max(1);
	let started = Instant::now();
	for (index, key) in keys.into_iter().enumerate() {
		let base_group = select_definition(&base, &key);
		let left_group = select_definition(&left, &key);
		let right_group = select_definition(&right, &key);
		if statements_content_equal(&base_group.statements, &left_group.statements)
			&& statements_content_equal(&base_group.statements, &right_group.statements)
		{
			statements.extend(base_group.statements);
		} else {
			active_definitions += 1;
			if let Some(selected) =
				direct_three_way_selection(&base_group, &left_group, &right_group, policies)
			{
				statements.extend(selected.iter().cloned());
				copy_through_definitions += 1;
			} else {
				let outcome =
					merge_clausewitz_files(&base_group, &left_group, &right_group, policies)?;
				conflicts.extend(outcome.conflicts().iter().cloned().map(|mut conflict| {
					conflict.detail = format!("definition `{key}`: {}", conflict.detail);
					conflict
				}));
				scalar_reductions.extend(outcome.scalar_reductions());
				let partition_timings = outcome.timings();
				timings.matcher_ns = timings
					.matcher_ns
					.saturating_add(partition_timings.matcher_ns);
				timings.delta_ns = timings.delta_ns.saturating_add(partition_timings.delta_ns);
				timings.pcs_ns = timings.pcs_ns.saturating_add(partition_timings.pcs_ns);
				timings.policy_ns = timings
					.policy_ns
					.saturating_add(partition_timings.policy_ns);
				let (partition_ast, kernel_fact) =
					outcome.into_parts(SemanticPartitionId::Definition(key.clone()));
				statements.extend(partition_ast.statements);
				kernel_facts.push(kernel_fact);
				structured_definitions += 1;
			}
		}

		let completed = index + 1;
		if total >= 20 && (completed == 1 || completed == total || completed % progress_step == 0) {
			let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
			let eta_ms = elapsed_ms.saturating_mul((total - completed) as u64) / completed as u64;
			eprintln!(
				"[structured-module] {} definitions {completed}/{total} active={active_definitions} copy_through={copy_through_definitions} structured={structured_definitions} elapsed_ms={elapsed_ms} eta_ms={eta_ms}",
				base.path.display(),
			);
		}
	}
	statements.sort_by(compare_top_level_statements);
	let mut tentative_ast = AstFile {
		path: base.path.clone(),
		statements,
	};
	let trivia = merge_trivia(&base_trivia, &left_trivia, &right_trivia);
	attach_trivia(&mut tentative_ast, &trivia);
	Ok(ClausewitzDefinitionModuleOutcome {
		tentative_ast,
		conflicts,
		scalar_reductions,
		kernel_facts,
		timings,
		base_definitions,
		active_definitions,
		copy_through_definitions,
		structured_definitions,
	})
}

fn merge_files_n_way(
	base: &AstFile,
	revisions: &[&AstFile],
	policies: &MergePolicies,
	resolutions: &[ConflictResolution],
) -> Result<super::ClausewitzMergeOutcome, AstAdapterError> {
	if resolutions.is_empty() {
		merge_clausewitz_files_n_way(base, revisions, policies)
	} else {
		merge_clausewitz_files_n_way_with_resolutions(base, revisions, policies, resolutions)
	}
}

fn direct_three_way_selection<'a>(
	base: &'a AstFile,
	left: &'a AstFile,
	right: &'a AstFile,
	policies: &MergePolicies,
) -> Option<&'a [AstStatement]> {
	if [base, left, right]
		.iter()
		.any(|file| contains_control_flow(&file.statements))
	{
		return None;
	}
	if statements_content_equal(&left.statements, &right.statements) {
		return Some(&left.statements);
	}
	if policies.one_sided_removal != OneSidedRemovalPolicy::Remove {
		return None;
	}
	if statements_content_equal(&base.statements, &left.statements) {
		return Some(&right.statements);
	}
	if statements_content_equal(&base.statements, &right.statements) {
		return Some(&left.statements);
	}
	None
}

fn select_definition(ast: &AstFile, key: &str) -> AstFile {
	AstFile {
		path: ast.path.clone(),
		statements: ast
			.statements
			.iter()
			.filter(|statement| {
				matches!(statement, AstStatement::Assignment { key: candidate, .. } if candidate == key)
			})
			.cloned()
			.collect(),
	}
}

fn top_level_assignment_keys(ast: &AstFile) -> BTreeSet<String> {
	ast.statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment { key, .. } => Some(key.clone()),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		})
		.collect()
}

pub(crate) fn definition_module_partition_ids(files: &[&AstFile]) -> Vec<SemanticPartitionId> {
	if files.iter().copied().any(has_top_level_items) {
		return vec![SemanticPartitionId::File];
	}
	files
		.iter()
		.copied()
		.flat_map(top_level_assignment_keys)
		.map(SemanticPartitionId::Definition)
		.collect::<BTreeSet<_>>()
		.into_iter()
		.collect()
}

fn has_top_level_items(ast: &AstFile) -> bool {
	ast.statements
		.iter()
		.any(|statement| matches!(statement, AstStatement::Item { .. }))
}

fn statements_content_equal(left: &[AstStatement], right: &[AstStatement]) -> bool {
	left.len() == right.len()
		&& left
			.iter()
			.zip(right)
			.all(|(left, right)| statement_content_equal(left, right))
}

fn statement_content_equal(left: &AstStatement, right: &AstStatement) -> bool {
	match (left, right) {
		(
			AstStatement::Assignment {
				key: left_key,
				value: left_value,
				..
			},
			AstStatement::Assignment {
				key: right_key,
				value: right_value,
				..
			},
		) => left_key == right_key && value_content_equal(left_value, right_value),
		(
			AstStatement::Item {
				value: left_value, ..
			},
			AstStatement::Item {
				value: right_value, ..
			},
		) => value_content_equal(left_value, right_value),
		(AstStatement::Comment { text: left, .. }, AstStatement::Comment { text: right, .. }) => {
			left == right
		}
		_ => false,
	}
}

fn value_content_equal(left: &AstValue, right: &AstValue) -> bool {
	match (left, right) {
		(AstValue::Scalar { value: left, .. }, AstValue::Scalar { value: right, .. }) => {
			left == right
		}
		(AstValue::Block { items: left, .. }, AstValue::Block { items: right, .. }) => {
			statements_content_equal(left, right)
		}
		_ => false,
	}
}

fn contains_control_flow(statements: &[AstStatement]) -> bool {
	statements.iter().any(|statement| match statement {
		AstStatement::Assignment { value, .. } => {
			super::control_flow::branch_key(statement).is_some()
				|| value_contains_control_flow(value)
		}
		AstStatement::Item { value, .. } => value_contains_control_flow(value),
		AstStatement::Comment { .. } => false,
	})
}

fn value_contains_control_flow(value: &AstValue) -> bool {
	matches!(value, AstValue::Block { items, .. } if contains_control_flow(items))
}

fn compare_top_level_statements(left: &AstStatement, right: &AstStatement) -> std::cmp::Ordering {
	match (left, right) {
		(
			AstStatement::Assignment { key: left, .. },
			AstStatement::Assignment { key: right, .. },
		) => left.cmp(right),
		(AstStatement::Assignment { .. }, _) => std::cmp::Ordering::Less,
		(_, AstStatement::Assignment { .. }) => std::cmp::Ordering::Greater,
		(AstStatement::Item { .. }, AstStatement::Comment { .. }) => std::cmp::Ordering::Less,
		(AstStatement::Comment { .. }, AstStatement::Item { .. }) => std::cmp::Ordering::Greater,
		_ => std::cmp::Ordering::Equal,
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use foch_language::analyzer::content_family::{MergePolicies, OneSidedRemovalPolicy};
	use foch_language::analyzer::parser::parse_clausewitz_content;

	use super::merge_clausewitz_definition_module;

	fn parse(source: &str) -> foch_language::analyzer::parser::AstFile {
		let parsed = parse_clausewitz_content(PathBuf::from("common/test/test.txt"), source);
		assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
		parsed.ast
	}

	fn emit(file: &foch_language::analyzer::parser::AstFile) -> String {
		crate::emit::emit_clausewitz_statements(&file.statements).expect("emit module")
	}

	#[test]
	fn complete_module_retains_inactive_definitions_and_top_level_trivia() {
		let base = parse("# retained\nalpha = { value = 1 }\nbeta = { value = 2 }\n");
		let left = parse("# retained\nalpha = { value = 3 }\nbeta = { value = 2 }\n");
		let right = parse("# retained\nalpha = { value = 1 extra = yes }\nbeta = { value = 2 }\n");

		let outcome =
			merge_clausewitz_definition_module(&base, &left, &right, &MergePolicies::default())
				.expect("merge complete module");
		let output = emit(outcome.resolved_ast().expect("conflict-free module"));

		assert!(output.contains("# retained"), "{output}");
		assert!(output.contains("beta ="), "{output}");
		assert!(output.contains("value = 2"), "{output}");
		assert!(output.contains("value = 3"), "{output}");
		assert!(output.contains("extra = yes"), "{output}");
	}

	#[test]
	fn policy_aware_partition_preserves_boolean_alternatives() {
		let base = parse(
			"institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = cloves } } }\n",
		);
		let right = parse(
			"institution = { can_embrace = { OR = { trade_goods = ivory trade_goods = fur } } }\n",
		);
		let policies = MergePolicies {
			one_sided_removal: OneSidedRemovalPolicy::PreserveBooleanAlternatives,
			..MergePolicies::default()
		};

		let outcome = merge_clausewitz_definition_module(&base, &base, &right, &policies)
			.expect("merge boolean alternatives");
		let output = emit(outcome.resolved_ast().expect("conflict-free module"));

		assert_eq!(outcome.copy_through_definitions(), 0);
		assert_eq!(outcome.structured_definitions(), 1);
		for trade_good in ["ivory", "cloves", "fur"] {
			assert!(
				output.contains(&format!("trade_goods = {trade_good}")),
				"{output}"
			);
		}
	}

	#[test]
	fn ordinary_remove_policy_uses_three_way_copy_through() {
		let base = parse("alpha = { value = 1 }\n");
		let right = parse("alpha = { value = 2 }\n");

		let outcome =
			merge_clausewitz_definition_module(&base, &base, &right, &MergePolicies::default())
				.expect("merge ordinary replacement");
		let output = emit(outcome.resolved_ast().expect("conflict-free module"));

		assert_eq!(outcome.copy_through_definitions(), 1);
		assert_eq!(outcome.structured_definitions(), 0);
		assert!(output.contains("value = 2"), "{output}");
	}
}
