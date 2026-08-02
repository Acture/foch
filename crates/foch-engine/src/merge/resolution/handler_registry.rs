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
//! New handlers should be added to [`dispatch`] and accompanied by a
//! unit test exercising the conflict-classification logic. Handlers must never
//! resort to silent last-writer choices for
//! ambiguous cases — explicit named handlers like `last_writer` make the
//! choice the user's, not the engine's.
//!
//! [`ResolutionDecision::Handler`]: foch_core::config::ResolutionDecision::Handler
//! [`LookupHandler`]: super::conflict_handler::LookupHandler
//! [`HandlerResolutionRecord`]: foch_core::model::HandlerResolutionRecord

use foch_core::model::HandlerResolutionRecord;

use super::conflict_handler::ConflictDecision;
use super::conflict_view::ConflictView;

/// Dispatch a named handler against a single conflict. Returns
/// [`ConflictDecision::Defer`] when the handler name is unknown so that the
/// surrounding chain (e.g. interactive prompt) can still take over instead
/// of aborting; the unknown-handler diagnostic is logged on stderr.
pub fn dispatch(name: &str, view: &ConflictView) -> ConflictDecision {
	if name.eq_ignore_ascii_case("last_writer") {
		last_writer(view)
	} else if name.eq_ignore_ascii_case("defer") {
		defer(view)
	} else if name.eq_ignore_ascii_case("keep_existing") {
		keep_existing(view)
	} else {
		eprintln!("{}", unknown_handler_diagnostic(name, view));
		ConflictDecision::Defer { record: None }
	}
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

/// Pick the candidate with the largest `(precedence, mod_id, candidate index)` tuple. Tie-breaks
/// on lexicographically larger `mod_id` so the result is fully deterministic
/// even when two contributors land at the same precedence (an unusual case
/// inside one DAG level, but possible across pre-collapsed siblings).
fn last_writer(view: &ConflictView) -> ConflictDecision {
	let Some((candidate, winner)) = view
		.candidates
		.iter()
		.enumerate()
		.max_by(|(left_index, left), (right_index, right)| {
			left.precedence
				.cmp(&right.precedence)
				.then_with(|| left.mod_id.cmp(&right.mod_id))
				.then_with(|| left_index.cmp(right_index))
		})
		.map(|(candidate, view)| (candidate, view.mod_id.clone()))
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
	ConflictDecision::PickCandidate {
		candidate,
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

	use super::*;
	use crate::merge::conflict_view::{CandidateView, ConflictView};

	fn view_for(file: &str, candidates: &[(&str, usize)]) -> ConflictView {
		ConflictView {
			file_path: PathBuf::from(file),
			address_path: vec!["province".to_string(), "12".to_string()],
			address_key: "owner".to_string(),
			conflict_id: "test-conflict-id".to_string(),
			reason: "test conflict".to_string(),
			vanilla_snippet: None,
			candidates: candidates
				.iter()
				.map(|(mod_id, precedence)| CandidateView {
					mod_id: (*mod_id).to_string(),
					mod_display_name: (*mod_id).to_string(),
					precedence: *precedence,
					change_summary: Vec::new(),
					candidate_rendered: String::new(),
				})
				.collect(),
		}
	}

	#[test]
	fn last_writer_picks_highest_precedence() {
		let view = view_for(
			"history/provinces/12-foo.txt",
			&[("mod-a", 0), ("mod-b", 5), ("mod-c", 2)],
		);
		let decision = dispatch("last_writer", &view);
		match decision {
			ConflictDecision::PickCandidate {
				candidate,
				record: Some(record),
			} => {
				assert_eq!(candidate, 1);
				assert_eq!(record.action, "last_writer");
				assert_eq!(record.source.as_deref(), Some("mod-b"));
				assert!(record.rationale.unwrap().contains("mod-b"));
				assert_eq!(record.path, "history/provinces/12-foo.txt");
			}
			other => panic!("expected PickCandidate with record, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_breaks_precedence_ties_lexicographically() {
		let view = view_for(
			"common/anything.txt",
			&[("mod-a", 3), ("mod-z", 3), ("mod-m", 3)],
		);
		let decision = dispatch("last_writer", &view);
		match decision {
			ConflictDecision::PickCandidate { candidate, .. } => {
				assert_eq!(candidate, 1);
			}
			other => panic!("expected PickCandidate, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_handles_empty_candidate_list_via_defer() {
		let view = view_for("foo.txt", &[]);
		let decision = dispatch("last_writer", &view);
		assert!(matches!(decision, ConflictDecision::Defer { .. }));
	}

	#[test]
	fn defer_handler_returns_defer_with_record() {
		let view = view_for("foo.txt", &[("mod-a", 0)]);
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
		let view = view_for("foo.txt", &[("mod-a", 0)]);
		let decision = dispatch("keep_existing", &view);
		assert!(matches!(decision, ConflictDecision::KeepExisting));
	}

	#[test]
	fn unknown_handler_defers_with_warning() {
		let view = view_for("foo.txt", &[("mod-a", 0)]);
		let decision = dispatch("made_up_handler", &view);
		assert!(matches!(decision, ConflictDecision::Defer { .. }));
	}

	#[test]
	fn unknown_handler_diagnostic_lowercases_name() {
		let view = view_for("foo.txt", &[("mod-a", 0)]);

		let diagnostic = unknown_handler_diagnostic("Made_Up_Handler", &view);

		assert!(diagnostic.contains("`made_up_handler`"));
		assert!(!diagnostic.contains("`Made_Up_Handler`"));
	}

	#[test]
	fn dispatch_is_case_insensitive() {
		let view = view_for("x.txt", &[("mod-a", 0)]);
		assert!(matches!(
			dispatch("LAST_WRITER", &view),
			ConflictDecision::PickCandidate { .. }
		));
		assert!(matches!(
			dispatch("Defer", &view),
			ConflictDecision::Defer { .. }
		));
	}
}
