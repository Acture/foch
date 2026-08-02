use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CandidateView {
	pub mod_id: String,
	pub mod_display_name: String,
	pub precedence: usize,
	pub change_summary: Vec<String>,
	pub candidate_rendered: String,
}

#[derive(Debug, Clone)]
pub struct ConflictView {
	pub file_path: PathBuf,
	pub address_path: Vec<String>,
	pub address_key: String,
	pub conflict_id: String,
	pub reason: String,
	pub vanilla_snippet: Option<String>,
	pub candidates: Vec<CandidateView>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::merge::conflict_handler::{ConflictDecision, ConflictHandler};

	struct HighestPrecedenceHandler;

	impl ConflictHandler for HighestPrecedenceHandler {
		fn on_conflict(&mut self, view: &ConflictView) -> ConflictDecision {
			view.candidates
				.iter()
				.enumerate()
				.max_by_key(|(_, candidate)| candidate.precedence)
				.map(|(candidate, _)| ConflictDecision::PickCandidate {
					candidate,
					record: None,
				})
				.unwrap_or(ConflictDecision::Defer { record: None })
		}
	}

	#[test]
	fn handler_can_decide_from_conflict_view_alone() {
		let view = ConflictView {
			file_path: PathBuf::from("common/example.txt"),
			address_path: vec!["root".to_string()],
			address_key: "owner".to_string(),
			conflict_id: "abc123".to_string(),
			reason: "conflicting scalar values".to_string(),
			vanilla_snippet: Some("owner = FRA".to_string()),
			candidates: vec![
				CandidateView {
					mod_id: "mod-low".to_string(),
					mod_display_name: "Low".to_string(),
					precedence: 1,
					change_summary: vec!["set owner".to_string()],
					candidate_rendered: "owner = LOW".to_string(),
				},
				CandidateView {
					mod_id: "mod-high".to_string(),
					mod_display_name: "High".to_string(),
					precedence: 9,
					change_summary: vec!["set owner".to_string()],
					candidate_rendered: "owner = HIGH".to_string(),
				},
			],
		};

		let decision = HighestPrecedenceHandler.on_conflict(&view);

		assert_eq!(
			decision,
			ConflictDecision::PickCandidate {
				candidate: 1,
				record: None
			}
		);
	}
}
