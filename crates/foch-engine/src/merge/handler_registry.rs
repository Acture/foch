//! Merge conflict handler registry.
//!
//! Pattern-rule resolutions in `foch.toml` may reference a named handler
//! (e.g. `handler = "last_writer"`) instead of binding to a specific mod.
//! At lookup time the resolution map yields a [`ResolutionDecision::Handler`]
//! and the merge engine's [`LookupHandler`] forwards it here. Each builtin
//! is responsible for inspecting the conflict and producing a concrete
//! [`ConflictDecision`] paired with a [`HandlerResolutionRecord`] so the
//! merge report can audit the chosen action.
//!
//! New handlers should be added to [`builtin_registry`] and accompanied by a
//! unit test exercising the conflict-classification logic. Handlers must never
//! resort to silent last-writer choices for
//! ambiguous cases — explicit named handlers like `last_writer` make the
//! choice the user's, not the engine's.
//!
//! [`ResolutionDecision::Handler`]: foch_core::config::ResolutionDecision::Handler
//! [`LookupHandler`]: super::conflict_handler::LookupHandler
//! [`HandlerResolutionRecord`]: foch_core::model::HandlerResolutionRecord

use std::collections::BTreeMap;
use std::sync::OnceLock;

use foch_core::model::HandlerResolutionRecord;

use super::conflict_handler::ConflictDecision;
use super::conflict_view::ConflictView;

type HandlerFn = fn(&ConflictView) -> ConflictDecision;

pub struct HandlerRegistry {
	handlers: BTreeMap<String, HandlerFn>,
}

impl HandlerRegistry {
	pub fn dispatch(&self, name: &str, view: &ConflictView) -> ConflictDecision {
		match self.handlers.get(&name.to_ascii_lowercase()) {
			Some(handler) => handler(view),
			None => {
				eprintln!("{}", unknown_handler_diagnostic(name, view));
				ConflictDecision::Defer { record: None }
			}
		}
	}

	#[allow(dead_code)]
	pub fn names(&self) -> Vec<String> {
		self.handlers.keys().cloned().collect()
	}
}

/// Dispatch a named handler against a single conflict. Returns
/// [`ConflictDecision::Defer`] when the handler name is unknown so that the
/// surrounding chain (e.g. interactive prompt) can still take over instead
/// of aborting; the unknown-handler diagnostic is logged on stderr.
pub fn dispatch(name: &str, view: &ConflictView) -> ConflictDecision {
	builtin_registry().dispatch(name, view)
}

pub fn builtin_registry() -> &'static HandlerRegistry {
	static BUILTIN_REGISTRY: OnceLock<HandlerRegistry> = OnceLock::new();
	BUILTIN_REGISTRY.get_or_init(|| {
		let mut handlers: BTreeMap<String, HandlerFn> = BTreeMap::new();
		handlers.insert("last_writer".to_string(), last_writer);
		handlers.insert("defer".to_string(), defer);
		handlers.insert("keep_existing".to_string(), keep_existing);
		HandlerRegistry { handlers }
	})
}

fn unknown_handler_diagnostic(name: &str, view: &ConflictView) -> String {
	let lower_name = name.to_ascii_lowercase();
	format!(
		"[foch] unknown merge handler `{lower_name}`; deferring conflict at {}::{}",
		view.file_path.display(),
		view.address_key
	)
}

fn defer(view: &ConflictView) -> ConflictDecision {
	ConflictDecision::Defer {
		record: Some(HandlerResolutionRecord {
			path: view.file_path.to_string_lossy().replace('\\', "/"),
			action: "defer".to_string(),
			source: None,
			rationale: Some("matched DSL handler=defer rule".to_string()),
		}),
	}
}

fn keep_existing(view: &ConflictView) -> ConflictDecision {
	let _ = view;
	ConflictDecision::KeepExisting
}

/// Pick the patch with the largest `(precedence, mod_id)` pair. Tie-breaks
/// on lexicographically larger `mod_id` so the result is fully deterministic
/// even when two contributors land at the same precedence (an unusual case
/// inside one DAG level, but possible across pre-collapsed siblings).
fn last_writer(view: &ConflictView) -> ConflictDecision {
	let Some(winner) = view
		.candidates
		.iter()
		.max_by(|a, b| {
			a.precedence
				.cmp(&b.precedence)
				.then_with(|| a.mod_id.cmp(&b.mod_id))
		})
		.map(|candidate| candidate.mod_id.clone())
	else {
		return ConflictDecision::Defer { record: None };
	};
	let mod_ids: Vec<&str> = view
		.candidates
		.iter()
		.map(|candidate| candidate.mod_id.as_str())
		.collect();
	let rationale = format!(
		"last_writer picked `{winner}` from contributors [{}] (highest precedence wins, mod_id ties broken lexicographically)",
		mod_ids.join(", ")
	);
	ConflictDecision::PickMod {
		mod_id: winner.clone(),
		record: Some(HandlerResolutionRecord {
			path: view.file_path.to_string_lossy().replace('\\', "/"),
			action: "last_writer".to_string(),
			source: Some(winner),
			rationale: Some(rationale),
		}),
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::super::patch::ClausewitzPatch;
	use super::super::patch_merge::{AttributedPatch, PatchAddress, PatchConflict};
	use super::*;
	use crate::merge::conflict_view::{CandidateView, ConflictView};
	use foch_language::analyzer::parser::{ScalarValue, Span, SpanRange};

	fn span() -> SpanRange {
		SpanRange {
			start: Span {
				line: 1,
				column: 1,
				offset: 0,
			},
			end: Span {
				line: 1,
				column: 2,
				offset: 1,
			},
		}
	}

	fn scalar_patch(value: &str) -> ClausewitzPatch {
		ClausewitzPatch::SetValue {
			path: vec![],
			key: "owner".to_string(),
			old_value: foch_language::analyzer::parser::AstValue::Scalar {
				value: ScalarValue::String("base".to_string()),
				span: span(),
			},
			new_value: foch_language::analyzer::parser::AstValue::Scalar {
				value: ScalarValue::String(value.to_string()),
				span: span(),
			},
		}
	}

	fn attributed(mod_id: &str, precedence: usize, value: &str) -> AttributedPatch {
		AttributedPatch {
			mod_id: mod_id.to_string(),
			precedence,
			patch: scalar_patch(value),
		}
	}

	fn address() -> PatchAddress {
		PatchAddress {
			path: vec!["province".to_string(), "12".to_string()],
			key: "owner".to_string(),
		}
	}

	fn conflict_with(patches: Vec<AttributedPatch>) -> PatchConflict {
		PatchConflict {
			patches,
			reason: "test conflict".to_string(),
		}
	}

	fn view_for(file: &str, address: &PatchAddress, conflict: &PatchConflict) -> ConflictView {
		ConflictView {
			file_path: PathBuf::from(file),
			address_path: address.path.clone(),
			address_key: address.key.clone(),
			conflict_id: "test-conflict-id".to_string(),
			reason: conflict.reason.clone(),
			vanilla_snippet: None,
			candidates: conflict
				.patches
				.iter()
				.map(|patch| CandidateView {
					mod_id: patch.mod_id.clone(),
					mod_display_name: patch.mod_id.clone(),
					precedence: patch.precedence,
					patch_summary: Vec::new(),
					patch_rendered: String::new(),
				})
				.collect(),
		}
	}

	#[test]
	fn last_writer_picks_highest_precedence() {
		let conflict = conflict_with(vec![
			attributed("mod-a", 0, "a"),
			attributed("mod-b", 5, "b"),
			attributed("mod-c", 2, "c"),
		]);
		let address = address();
		let view = view_for("history/provinces/12-foo.txt", &address, &conflict);
		let decision = dispatch("last_writer", &view);
		match decision {
			ConflictDecision::PickMod {
				mod_id,
				record: Some(record),
			} => {
				assert_eq!(mod_id, "mod-b");
				assert_eq!(record.action, "last_writer");
				assert_eq!(record.source.as_deref(), Some("mod-b"));
				assert!(record.rationale.unwrap().contains("mod-b"));
				assert_eq!(record.path, "history/provinces/12-foo.txt");
			}
			other => panic!("expected PickMod with record, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_breaks_precedence_ties_lexicographically() {
		let conflict = conflict_with(vec![
			attributed("mod-a", 3, "a"),
			attributed("mod-z", 3, "z"),
			attributed("mod-m", 3, "m"),
		]);
		let address = address();
		let view = view_for("common/anything.txt", &address, &conflict);
		let decision = dispatch("last_writer", &view);
		match decision {
			ConflictDecision::PickMod { mod_id, .. } => {
				assert_eq!(mod_id, "mod-z");
			}
			other => panic!("expected PickMod, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_handles_empty_patch_list_via_defer() {
		let conflict = conflict_with(vec![]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);
		let decision = dispatch("last_writer", &view);
		assert!(matches!(decision, ConflictDecision::Defer { .. }));
	}

	#[test]
	fn defer_handler_returns_defer_with_record() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);
		let decision = dispatch("defer", &view);
		match decision {
			ConflictDecision::Defer {
				record: Some(record),
			} => {
				assert_eq!(record.path, "foo.txt");
				assert_eq!(record.action, "defer");
				assert_eq!(record.source, None);
				assert_eq!(
					record.rationale.as_deref(),
					Some("matched DSL handler=defer rule")
				);
			}
			other => panic!("expected Defer with record, got {other:?}"),
		}
	}

	#[test]
	fn keep_existing_handler_returns_keep_existing() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);
		let decision = dispatch("keep_existing", &view);
		assert!(matches!(decision, ConflictDecision::KeepExisting));
	}

	#[test]
	fn unknown_handler_defers_with_warning() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);
		let decision = dispatch("made_up_handler", &view);
		assert!(matches!(decision, ConflictDecision::Defer { .. }));
	}

	#[test]
	fn unknown_handler_diagnostic_lowercases_name() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);

		let diagnostic = unknown_handler_diagnostic("Made_Up_Handler", &view);

		assert!(diagnostic.contains("`made_up_handler`"));
		assert!(!diagnostic.contains("`Made_Up_Handler`"));
	}

	#[test]
	fn registry_dispatch_unknown_defers() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("foo.txt", &address, &conflict);
		let registry = builtin_registry();
		let decision = registry.dispatch("made_up_handler", &view);
		assert!(matches!(decision, ConflictDecision::Defer { .. }));
	}

	#[test]
	fn registry_lists_builtin_names() {
		let names = builtin_registry().names();
		assert!(names.contains(&"last_writer".to_string()));
		assert!(names.contains(&"defer".to_string()));
		assert!(names.contains(&"keep_existing".to_string()));
	}

	#[test]
	fn dispatch_is_case_insensitive() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let address = address();
		let view = view_for("x.txt", &address, &conflict);
		assert!(matches!(
			dispatch("LAST_WRITER", &view),
			ConflictDecision::PickMod { .. }
		));
		assert!(matches!(
			dispatch("Defer", &view),
			ConflictDecision::Defer { .. }
		));
	}
}
