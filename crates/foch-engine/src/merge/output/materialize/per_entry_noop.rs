use std::collections::HashMap;

use foch_language::analyzer::content_family::{ContentFamilyDescriptor, MergeKeySource};
use foch_language::analyzer::parser::{AstStatement, AstValue};
use foch_language::analyzer::semantic_index::is_decision_container_key;

use super::cross_file_dedup::{container_child_field_value_key, scalar_assignment_value};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PerEntryNoopLookupKey {
	path: Vec<String>,
	key: String,
}

pub(super) fn drop_per_entry_noop_duplicates(
	merged_statements: Vec<AstStatement>,
	vanilla_statements: &[AstStatement],
	descriptor: &ContentFamilyDescriptor,
) -> (Vec<AstStatement>, usize) {
	if !descriptor.capabilities.dedup_policy.per_entry_safe() {
		return (merged_statements, 0);
	}
	let Some(merge_key_source) = descriptor.merge_key_source else {
		return (merged_statements, 0);
	};
	if matches!(merge_key_source, MergeKeySource::LeafPath) {
		return (merged_statements, 0);
	}

	let vanilla_lookup = build_per_entry_noop_lookup(vanilla_statements, merge_key_source);
	if vanilla_lookup.is_empty() {
		return (merged_statements, 0);
	}

	filter_per_entry_noop_statements(merged_statements, merge_key_source, &vanilla_lookup)
}

fn build_per_entry_noop_lookup(
	statements: &[AstStatement],
	merge_key_source: MergeKeySource,
) -> HashMap<PerEntryNoopLookupKey, Vec<AstStatement>> {
	let mut lookup: HashMap<PerEntryNoopLookupKey, Vec<AstStatement>> = HashMap::new();
	for statement in statements {
		if let Some(key) = per_entry_noop_top_level_key(statement, merge_key_source) {
			lookup.entry(key).or_default().push(statement.clone());
		}
		for (key, child) in per_entry_noop_child_entries(statement, merge_key_source) {
			lookup.entry(key).or_default().push(child.clone());
		}
	}
	lookup
}

fn filter_per_entry_noop_statements(
	statements: Vec<AstStatement>,
	merge_key_source: MergeKeySource,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> (Vec<AstStatement>, usize) {
	let mut filtered = Vec::with_capacity(statements.len());
	let mut dropped = 0usize;
	for statement in statements {
		if let Some(key) = per_entry_noop_top_level_key(&statement, merge_key_source)
			&& per_entry_noop_matches_vanilla(&key, &statement, vanilla_lookup)
		{
			dropped += 1;
			continue;
		}

		let (statement, child_dropped) =
			filter_per_entry_noop_child_statements(statement, merge_key_source, vanilla_lookup);
		dropped += child_dropped;
		filtered.push(statement);
	}
	(filtered, dropped)
}

fn filter_per_entry_noop_child_statements(
	statement: AstStatement,
	merge_key_source: MergeKeySource,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> (AstStatement, usize) {
	match statement {
		AstStatement::Assignment {
			key,
			key_span,
			value: AstValue::Block {
				items,
				span: value_span,
			},
			span,
		} if per_entry_noop_container_is_filterable(&key, merge_key_source) => {
			let mut filtered_items = Vec::with_capacity(items.len());
			let mut dropped = 0usize;
			for item in items {
				if let Some(lookup_key) = per_entry_noop_child_key(&key, &item, merge_key_source)
					&& per_entry_noop_matches_vanilla(&lookup_key, &item, vanilla_lookup)
				{
					dropped += 1;
					continue;
				}
				filtered_items.push(item);
			}
			(
				AstStatement::Assignment {
					key,
					key_span,
					value: AstValue::Block {
						items: filtered_items,
						span: value_span,
					},
					span,
				},
				dropped,
			)
		}
		other => (other, 0),
	}
}

fn per_entry_noop_matches_vanilla(
	key: &PerEntryNoopLookupKey,
	statement: &AstStatement,
	vanilla_lookup: &HashMap<PerEntryNoopLookupKey, Vec<AstStatement>>,
) -> bool {
	vanilla_lookup.get(key).is_some_and(|vanilla_entries| {
		vanilla_entries.iter().any(|vanilla| {
			crate::merge::patch::ast_statements_semantically_equal(vanilla, statement)
		})
	})
}

fn per_entry_noop_top_level_key(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Option<PerEntryNoopLookupKey> {
	match merge_key_source {
		MergeKeySource::AssignmentKey => match statement {
			AstStatement::Assignment { key, .. } => Some(PerEntryNoopLookupKey {
				path: Vec::new(),
				key: key.clone(),
			}),
			_ => None,
		},
		MergeKeySource::FieldValue(field) => {
			let AstStatement::Assignment {
				value: AstValue::Block { items, .. },
				..
			} = statement
			else {
				return None;
			};
			scalar_assignment_value(items, field).map(|key| PerEntryNoopLookupKey {
				path: Vec::new(),
				key,
			})
		}
		MergeKeySource::ContainerChildFieldValue { container, .. } => {
			let AstStatement::Assignment { key, .. } = statement else {
				return None;
			};
			(key != container).then(|| PerEntryNoopLookupKey {
				path: Vec::new(),
				key: key.clone(),
			})
		}
		MergeKeySource::ContainerChildKey | MergeKeySource::LeafPath => None,
	}
}

fn per_entry_noop_child_entries(
	statement: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Vec<(PerEntryNoopLookupKey, &AstStatement)> {
	let AstStatement::Assignment {
		key,
		value: AstValue::Block { items, .. },
		..
	} = statement
	else {
		return Vec::new();
	};
	if !per_entry_noop_container_is_filterable(key, merge_key_source) {
		return Vec::new();
	}
	items
		.iter()
		.filter_map(|item| {
			per_entry_noop_child_key(key, item, merge_key_source)
				.map(|lookup_key| (lookup_key, item))
		})
		.collect()
}

fn per_entry_noop_container_is_filterable(
	container: &str,
	merge_key_source: MergeKeySource,
) -> bool {
	match merge_key_source {
		MergeKeySource::ContainerChildKey => is_decision_container_key(container),
		MergeKeySource::ContainerChildFieldValue {
			container: expected,
			..
		} => container == expected,
		_ => false,
	}
}

fn per_entry_noop_child_key(
	container: &str,
	child: &AstStatement,
	merge_key_source: MergeKeySource,
) -> Option<PerEntryNoopLookupKey> {
	match merge_key_source {
		MergeKeySource::ContainerChildKey => {
			if !is_decision_container_key(container) {
				return None;
			}
			let AstStatement::Assignment { key, .. } = child else {
				return None;
			};
			Some(PerEntryNoopLookupKey {
				path: vec![container.to_string()],
				key: key.clone(),
			})
		}
		MergeKeySource::ContainerChildFieldValue {
			container: expected,
			child_key_field,
			child_types,
		} => {
			if container != expected {
				return None;
			}
			container_child_field_value_key(child, child_key_field, child_types).map(|key| {
				PerEntryNoopLookupKey {
					path: vec![container.to_string()],
					key,
				}
			})
		}
		_ => None,
	}
}
