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
//! New handlers should be added to [`dispatch`] (case-insensitive name match)
//! and accompanied by a unit test exercising the conflict-classification
//! logic. Handlers must never resort to silent last-writer fallbacks for
//! ambiguous cases — explicit named handlers like `last_writer` make the
//! choice the user's, not the engine's.
//!
//! [`ResolutionDecision::Handler`]: foch_core::config::ResolutionDecision::Handler
//! [`LookupHandler`]: super::conflict_handler::LookupHandler
//! [`HandlerResolutionRecord`]: foch_core::model::HandlerResolutionRecord

use std::path::Path;

use foch_core::model::HandlerResolutionRecord;

use super::conflict_handler::ConflictDecision;
use super::patch_merge::{PatchAddress, PatchConflict};

/// Dispatch a named handler against a single conflict. Returns
/// [`ConflictDecision::Defer`] when the handler name is unknown so that the
/// surrounding chain (e.g. interactive prompt) can still take over instead
/// of aborting; the unknown-handler diagnostic is logged on stderr.
pub fn dispatch(
	name: &str,
	current_file: &Path,
	address: &PatchAddress,
	conflict: &PatchConflict,
) -> ConflictDecision {
	match name.to_ascii_lowercase().as_str() {
		"last_writer" => last_writer(name, current_file, address, conflict),
		"defer" => defer(),
		"keep_existing" => keep_existing(name, current_file, address),
		other => {
			eprintln!(
				"[foch] unknown merge handler `{other}`; deferring conflict at {}::{}",
				current_file.display(),
				address.key
			);
			ConflictDecision::Defer
		}
	}
}

fn defer() -> ConflictDecision {
	ConflictDecision::Defer
}

fn keep_existing(name: &str, current_file: &Path, address: &PatchAddress) -> ConflictDecision {
	let _ = (name, current_file, address);
	ConflictDecision::KeepExisting
}

/// Pick the patch with the largest `(precedence, mod_id)` pair. Tie-breaks
/// on lexicographically larger `mod_id` so the result is fully deterministic
/// even when two contributors land at the same precedence (an unusual case
/// inside one DAG level, but possible across pre-collapsed siblings).
fn last_writer(
	name: &str,
	current_file: &Path,
	_address: &PatchAddress,
	conflict: &PatchConflict,
) -> ConflictDecision {
	let Some(winner) = conflict
		.patches
		.iter()
		.max_by(|a, b| {
			a.precedence
				.cmp(&b.precedence)
				.then_with(|| a.mod_id.cmp(&b.mod_id))
		})
		.map(|patch| patch.mod_id.clone())
	else {
		return ConflictDecision::Defer;
	};
	let mod_ids: Vec<&str> = conflict
		.patches
		.iter()
		.map(|patch| patch.mod_id.as_str())
		.collect();
	let rationale = format!(
		"last_writer picked `{winner}` from contributors [{}] (highest precedence wins, mod_id ties broken lexicographically)",
		mod_ids.join(", ")
	);
	ConflictDecision::PickModWithRecord {
		mod_id: winner.clone(),
		record: HandlerResolutionRecord {
			path: current_file.to_string_lossy().replace('\\', "/"),
			action: name.to_ascii_lowercase(),
			source: Some(winner),
			rationale: Some(rationale),
		},
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::super::patch::ClausewitzPatch;
	use super::super::patch_merge::AttributedPatch;
	use super::*;
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

	#[test]
	fn last_writer_picks_highest_precedence() {
		let conflict = conflict_with(vec![
			attributed("mod-a", 0, "a"),
			attributed("mod-b", 5, "b"),
			attributed("mod-c", 2, "c"),
		]);
		let decision = dispatch(
			"last_writer",
			&PathBuf::from("history/provinces/12-foo.txt"),
			&address(),
			&conflict,
		);
		match decision {
			ConflictDecision::PickModWithRecord { mod_id, record } => {
				assert_eq!(mod_id, "mod-b");
				assert_eq!(record.action, "last_writer");
				assert_eq!(record.source.as_deref(), Some("mod-b"));
				assert!(record.rationale.unwrap().contains("mod-b"));
				assert_eq!(record.path, "history/provinces/12-foo.txt");
			}
			other => panic!("expected PickModWithRecord, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_breaks_precedence_ties_lexicographically() {
		let conflict = conflict_with(vec![
			attributed("mod-a", 3, "a"),
			attributed("mod-z", 3, "z"),
			attributed("mod-m", 3, "m"),
		]);
		let decision = dispatch(
			"last_writer",
			&PathBuf::from("common/anything.txt"),
			&address(),
			&conflict,
		);
		match decision {
			ConflictDecision::PickModWithRecord { mod_id, .. } => {
				assert_eq!(mod_id, "mod-z");
			}
			other => panic!("expected PickModWithRecord, got {other:?}"),
		}
	}

	#[test]
	fn last_writer_handles_empty_patch_list_via_defer() {
		let conflict = conflict_with(vec![]);
		let decision = dispatch(
			"last_writer",
			&PathBuf::from("foo.txt"),
			&address(),
			&conflict,
		);
		assert!(matches!(decision, ConflictDecision::Defer));
	}

	#[test]
	fn defer_handler_returns_defer() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let decision = dispatch("defer", &PathBuf::from("foo.txt"), &address(), &conflict);
		assert!(matches!(decision, ConflictDecision::Defer));
	}

	#[test]
	fn keep_existing_handler_returns_keep_existing() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let decision = dispatch(
			"keep_existing",
			&PathBuf::from("foo.txt"),
			&address(),
			&conflict,
		);
		assert!(matches!(decision, ConflictDecision::KeepExisting));
	}

	#[test]
	fn unknown_handler_defers_with_warning() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		let decision = dispatch(
			"made_up_handler",
			&PathBuf::from("foo.txt"),
			&address(),
			&conflict,
		);
		assert!(matches!(decision, ConflictDecision::Defer));
	}

	#[test]
	fn dispatch_is_case_insensitive() {
		let conflict = conflict_with(vec![attributed("mod-a", 0, "a")]);
		assert!(matches!(
			dispatch(
				"LAST_WRITER",
				&PathBuf::from("x.txt"),
				&address(),
				&conflict
			),
			ConflictDecision::PickModWithRecord { .. }
		));
		assert!(matches!(
			dispatch("Defer", &PathBuf::from("x.txt"), &address(), &conflict),
			ConflictDecision::Defer
		));
	}
}
