//! Summaries and machine-readable output of a completed run.

use crate::judge::{CaseResult, Status};
use crate::model::RunId;
use crate::plan::{NotRun, NotRunReason};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// What the run used, recorded so a result can be reproduced.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnvironmentRecord {
	pub game_version: Option<String>,
	pub dlc: Vec<String>,
	pub mods: Vec<String>,
	pub runner: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Summary {
	pub passed: usize,
	pub smoke: usize,
	pub xfailed: usize,
	pub xpassed: usize,
	pub failed: usize,
	pub setup_error: usize,
	pub incomplete: usize,
	pub protocol_error: usize,
	pub runtime_error: usize,
	pub skipped: usize,
	pub ignored: usize,
	pub deselected: usize,
	/// Sum of distinct session wall times.
	pub wall_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
	pub run_id: RunId,
	pub results: Vec<CaseResult>,
	pub not_run: Vec<NotRun>,
	pub environment: EnvironmentRecord,
}

impl Report {
	pub fn new(
		run_id: RunId,
		results: Vec<CaseResult>,
		not_run: Vec<NotRun>,
		environment: EnvironmentRecord,
	) -> Self {
		Self {
			run_id,
			results,
			not_run,
			environment,
		}
	}

	pub fn summary(&self) -> Summary {
		let mut summary = Summary::default();
		for result in &self.results {
			let counter = match result.status {
				Status::Passed => &mut summary.passed,
				Status::Smoke => &mut summary.smoke,
				Status::XFailed => &mut summary.xfailed,
				Status::XPassed { .. } => &mut summary.xpassed,
				Status::Failed => &mut summary.failed,
				Status::SetupError => &mut summary.setup_error,
				Status::Incomplete => &mut summary.incomplete,
				Status::ProtocolError => &mut summary.protocol_error,
				Status::RuntimeError => &mut summary.runtime_error,
				Status::Skipped => &mut summary.skipped,
				Status::Ignored => &mut summary.ignored,
			};
			*counter += 1;
		}
		for not_run in &self.not_run {
			match not_run.reason {
				NotRunReason::Deselected => summary.deselected += 1,
				NotRunReason::Skipped { .. } => summary.skipped += 1,
				NotRunReason::Ignored { .. } => summary.ignored += 1,
			}
		}
		// Cases of one shared session report the same session timing once.
		let mut sessions = BTreeSet::new();
		for result in &self.results {
			if sessions.insert(result.session.as_str()) {
				summary.wall_ms += result.timing.wall_ms;
			}
		}
		summary
	}

	pub fn slowest(&self, count: usize) -> Vec<&CaseResult> {
		let mut results: Vec<_> = self.results.iter().collect();
		results.sort_by_key(|result| std::cmp::Reverse(result.timing.wall_ms));
		results.truncate(count);
		results
	}

	/// Node IDs to select with `--last-failed` next time.
	pub fn failed_nodes(&self) -> BTreeSet<String> {
		self.results
			.iter()
			.filter(|result| !result.status.is_success())
			.map(|result| result.node.to_string())
			.collect()
	}

	/// 0 when something ran or was explicitly skipped and nothing failed.
	pub fn exit_code(&self) -> i32 {
		let summary = self.summary();
		let anything = !self.results.is_empty() || summary.skipped + summary.ignored > 0;
		i32::from(
			!anything
				|| self
					.results
					.iter()
					.any(|result| !result.status.is_success()),
		)
	}

	pub fn to_json(&self) -> String {
		serde_json::to_string_pretty(self).expect("reports serialize")
	}

	/// JUnit XML as understood by common CI services.
	pub fn to_junit(&self) -> String {
		let summary = self.summary();
		let failures = summary.failed
			+ self
				.results
				.iter()
				.filter(|result| result.status == Status::XPassed { strict: true })
				.count();
		let errors = summary.setup_error
			+ summary.incomplete
			+ summary.protocol_error
			+ summary.runtime_error;
		let skipped = summary.skipped + summary.ignored;
		let tests = self.results.len() + skipped;
		let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
		let _ = writeln!(
			xml,
			"<testsuites><testsuite name=\"foch\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{:.3}\">",
			summary.wall_ms as f64 / 1000.0
		);
		for result in &self.results {
			let _ = write!(
				xml,
				"<testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">",
				escape(result.node.path.as_str()),
				escape(&result.node.local()),
				result.timing.wall_ms as f64 / 1000.0
			);
			let detail: Vec<String> = result
				.failures
				.iter()
				.map(|failure| match &failure.span {
					Some(span) => format!("{span} {}: {}", failure.check, failure.message),
					None => format!("{}: {}", failure.check, failure.message),
				})
				.chain(result.notes.iter().cloned())
				.collect();
			let detail = escape(&detail.join("\n"));
			match result.status {
				Status::Failed | Status::XPassed { strict: true } => {
					let _ = write!(
						xml,
						"<failure message=\"{:?}\">{detail}</failure>",
						result.status
					);
				}
				Status::SetupError
				| Status::Incomplete
				| Status::ProtocolError
				| Status::RuntimeError => {
					let _ = write!(
						xml,
						"<error message=\"{:?}\">{detail}</error>",
						result.status
					);
				}
				_ => {}
			}
			xml.push_str("</testcase>\n");
		}
		for not_run in &self.not_run {
			let message = match &not_run.reason {
				NotRunReason::Deselected => continue,
				NotRunReason::Skipped { message } | NotRunReason::Ignored { message } => {
					message.clone().unwrap_or_default()
				}
			};
			let _ = writeln!(
				xml,
				"<testcase classname=\"{}\" name=\"{}\"><skipped message=\"{}\"/></testcase>",
				escape(not_run.node.path.as_str()),
				escape(&not_run.node.local()),
				escape(&message)
			);
		}
		xml.push_str("</testsuite></testsuites>\n");
		xml
	}
}

fn escape(text: &str) -> String {
	text.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}
