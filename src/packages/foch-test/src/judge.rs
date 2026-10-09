//! Turn what a runner produced into one status per case.
//!
//! This is the only place that decides whether a case passed. It combines
//! the engine log with the process outcome, so rules such as "`xfail` only
//! absorbs assertion failures, never crashes" hold in one place.

use crate::compile::{Bundle, CheckKind, PlannedCheck, RunContext, compile};
use crate::model::{CaseIndex, ContentId, GameDate, NodeId, Start};
use crate::plan::{Isolation, Plan};
use crate::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "exit", rename_all = "snake_case")]
pub enum RunnerExit {
	Success,
	Failed { code: Option<i32> },
	TimedOut { seconds: u64 },
	LaunchFailed { message: String },
	Cancelled,
}

/// Wall-clock cost of a session, in milliseconds. The phases are present
/// only when the runner reports them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Timing {
	pub wall_ms: u64,
	pub launch_ms: Option<u64>,
	pub in_game_ms: Option<u64>,
	pub teardown_ms: Option<u64>,
}

/// Material the caller collected from one runner execution.
#[derive(Clone, Debug)]
pub struct RunArtifacts<'a> {
	pub game_log: Option<&'a str>,
	pub error_log: Option<&'a str>,
	pub exit: RunnerExit,
	pub timing: Timing,
	/// The user's real game profile was unchanged by the run.
	pub profile_untouched: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
	Passed,
	/// No expectation; only proves that the steps completed.
	Smoke,
	XFailed,
	XPassed {
		strict: bool,
	},
	Skipped,
	Ignored,
	Failed,
	/// A fixture check or the scope check was false.
	SetupError,
	Incomplete,
	ProtocolError,
	RuntimeError,
}

impl Status {
	pub fn is_success(&self) -> bool {
		matches!(
			self,
			Self::Passed
				| Self::Smoke
				| Self::XFailed
				| Self::XPassed { strict: false }
				| Self::Skipped
				| Self::Ignored
		)
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckFailure {
	pub check: String,
	pub span: Option<SourceSpan>,
	pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
	pub index: CaseIndex,
	pub node: NodeId,
	pub content_id: ContentId,
	pub origin: SourceSpan,
	/// Start date and country, kept so a report is readable on its own.
	pub start: Start,
	#[serde(flatten)]
	pub status: Status,
	pub failures: Vec<CheckFailure>,
	pub passed_checks: Vec<String>,
	pub missing_checks: Vec<String>,
	/// Identity of the game session that ran the case.
	pub session: String,
	/// Session cost; shared by every case of a shared session.
	pub timing: Timing,
	pub isolation: Isolation,
	pub notes: Vec<String>,
}

pub fn judge(bundle: &Bundle, plan: &Plan, artifacts: &RunArtifacts) -> Vec<CaseResult> {
	let session = plan.sessions.get(bundle.session.0);
	let isolation = session.map_or(Isolation::Dedicated, |session| session.isolation);
	let mut runtime = Vec::new();
	match &artifacts.exit {
		RunnerExit::Success => {}
		RunnerExit::Failed { code } => {
			runtime.push(format!("runner exited unsuccessfully ({code:?})"))
		}
		RunnerExit::TimedOut { seconds } => {
			runtime.push(format!("runner timed out after {seconds} seconds"))
		}
		RunnerExit::LaunchFailed { message } => {
			runtime.push(format!("cannot launch runner: {message}"))
		}
		RunnerExit::Cancelled => runtime.push("run was cancelled".into()),
	}
	// EU4 writes ambient entries to error.log on every launch (missing
	// localisation, other mods, vanilla warnings), so a non-empty log is not
	// itself a failure. Only entries attributable to the generated test layer
	// make the run untrustworthy; the rest are reported but do not fail it.
	let mut info = Vec::new();
	match artifacts.error_log {
		None => runtime.push("runner must produce an isolated error.log".into()),
		Some(log) => {
			let analysis = analyze_error_log(&bundle.namespace, &bundle.test_mod_name, log);
			if !analysis.attributed.is_empty() {
				runtime.push(analysis.attributed_summary());
			}
			if analysis.ambient > 0 {
				info.push(analysis.ambient_summary());
			}
		}
	}
	if artifacts.game_log.is_none() {
		runtime.push("runner must produce game.log".into());
	}
	if !artifacts.profile_untouched {
		runtime.push("the real game profile changed during the run; isolation was violated".into());
	}

	// A manifest that differs from what the plan compiles to cannot be
	// trusted to describe the required evidence.
	let context = RunContext {
		run_id: bundle.run_id.clone(),
		mod_name: bundle.mod_name.clone(),
	};
	let manifest = match compile(plan, bundle.session, &context) {
		Ok(expected) if &expected == bundle => Ok(()),
		Ok(_) => Err("bundle differs from the plan; unsupported or altered manifest".to_string()),
		Err(error) => Err(format!("plan no longer compiles: {error}")),
	};
	let records = match (&manifest, artifacts.game_log) {
		(Err(message), _) => Err(message.clone()),
		(Ok(()), Some(log)) => parse_log(bundle, log),
		(Ok(()), None) => Ok(BTreeMap::new()),
	};

	bundle
		.cases
		.iter()
		.map(|&index| {
			let planned = plan.case(index);
			let case = &planned.case;
			let mut result = CaseResult {
				index,
				node: case.node.clone(),
				content_id: case.content_id.clone(),
				origin: case.origin.clone(),
				start: case.start.clone(),
				status: Status::RuntimeError,
				failures: Vec::new(),
				passed_checks: Vec::new(),
				missing_checks: Vec::new(),
				session: bundle.marker_prefix.clone(),
				timing: artifacts.timing,
				isolation,
				notes: runtime.clone(),
			};
			result.notes.extend(info.iter().cloned());
			let records = match &records {
				Ok(records) => records.get(&index).cloned().unwrap_or_default(),
				Err(message) => {
					result.status = if runtime.is_empty() {
						Status::ProtocolError
					} else {
						Status::RuntimeError
					};
					result.notes.push(message.clone());
					return result;
				}
			};
			let checks: Vec<&PlannedCheck> = bundle
				.checks
				.iter()
				.filter(|check| check.case == index)
				.collect();
			let window = bundle.windows.iter().find(|window| window.case == index);
			let verdict = window
				.ok_or_else(|| "case has no window in the bundle".to_string())
				.and_then(|window| evaluate(&checks, window.begin, window.end, &records));
			if !runtime.is_empty() {
				result.status = Status::RuntimeError;
				if let Ok(verdict) = verdict {
					result.passed_checks = verdict.passed;
					result.failures = verdict.failed;
				}
				return result;
			}
			let verdict = match verdict {
				Ok(verdict) => verdict,
				Err(message) => {
					result.status = Status::ProtocolError;
					result.notes.push(message);
					return result;
				}
			};
			let setup_failed = verdict
				.failed
				.iter()
				.any(|failure| !failure.check.starts_with("expect."));
			let expect_failed = verdict
				.failed
				.iter()
				.any(|failure| failure.check.starts_with("expect."));
			let complete = verdict.missing.is_empty();
			let xfail = case.marks.xfail.as_ref();
			result.status = if setup_failed {
				Status::SetupError
			} else if expect_failed {
				match (xfail, complete) {
					(Some(_), true) => Status::XFailed,
					(Some(_), false) => Status::Incomplete,
					(None, _) => Status::Failed,
				}
			} else if !complete {
				Status::Incomplete
			} else if let Some(xfail) = xfail {
				Status::XPassed {
					strict: xfail.strict,
				}
			} else if case.is_smoke() {
				Status::Smoke
			} else {
				Status::Passed
			};
			result.passed_checks = verdict.passed;
			result.failures = verdict.failed;
			result.missing_checks = verdict.missing;
			result
		})
		.collect()
}

/// Combine a shared-session failure with its isolated re-run. The isolated
/// result decides the case; a disagreement is reported because it means the
/// grouping analysis let cases interfere.
pub fn reconcile(shared: &CaseResult, isolated: CaseResult) -> CaseResult {
	let mut result = isolated;
	if result.status.is_success() && !shared.status.is_success() {
		result.notes.push(format!(
			"failed as {:?} in a shared session but passed alone; cases interfered, so the grouping analysis is wrong for this case",
			shared.status
		));
	} else {
		result
			.notes
			.push("failure in a shared session confirmed by an isolated re-run".into());
	}
	result
}

/// A reading of the engine `error.log`, splitting entries that the generated
/// test layer caused from the ambient entries every launch produces.
#[derive(Clone, Debug, Default)]
struct ErrorAnalysis {
	/// Entries that name the generated test layer, so the run cannot be trusted.
	attributed: Vec<String>,
	/// How many entries were unrelated to the test layer.
	ambient: usize,
	/// A few ambient entries, for a readable note.
	ambient_sample: Vec<String>,
}

impl ErrorAnalysis {
	fn attributed_summary(&self) -> String {
		format!(
			"engine error.log reports {} error(s) in the generated test layer:\n\t{}",
			self.attributed.len(),
			self.attributed.join("\n\t")
		)
	}

	fn ambient_summary(&self) -> String {
		let plural = if self.ambient == 1 {
			"entry"
		} else {
			"entries"
		};
		let mut summary = format!(
			"engine error.log has {} {plural} unrelated to the test layer; not failing this run",
			self.ambient
		);
		if !self.ambient_sample.is_empty() {
			summary.push_str(":\n\t");
			summary.push_str(&self.ambient_sample.join("\n\t"));
			if self.ambient > self.ambient_sample.len() {
				summary.push_str("\n\t...");
			}
		}
		summary
	}
}

/// Attribute `error.log` entries to the generated test layer or to ambient
/// engine noise. The namespace prefixes every generated event id and file
/// name, and the test mod descriptor name is equally unique, so an entry that
/// quotes either is one the test layer caused.
fn analyze_error_log(namespace: &str, test_mod_name: &str, log: &str) -> ErrorAnalysis {
	let mut analysis = ErrorAnalysis::default();
	for entry in error_entries(log) {
		if entry.contains(namespace) || entry.contains(test_mod_name) {
			analysis.attributed.push(entry);
		} else {
			analysis.ambient += 1;
			if analysis.ambient_sample.len() < 3 {
				analysis.ambient_sample.push(entry);
			}
		}
	}
	analysis
}

/// Split the engine `error.log` into entries. EU4 begins each with a
/// `[time][source]:` header line; lines that do not start with `[` continue
/// the entry above them. Blank lines are dropped.
fn error_entries(log: &str) -> Vec<String> {
	let mut entries: Vec<String> = Vec::new();
	for line in log.lines() {
		let line = line.trim();
		if line.is_empty() {
			continue;
		}
		if line.starts_with('[') || entries.is_empty() {
			entries.push(line.to_string());
		} else {
			let entry = entries.last_mut().expect("entries is non-empty");
			entry.push(' ');
			entry.push_str(line);
		}
	}
	entries
}

#[derive(Clone, Debug)]
struct Record {
	date: GameDate,
	kind: RecordKind,
}

#[derive(Clone, Debug, PartialEq)]
enum RecordKind {
	Begin,
	End,
	Pass(String),
	Fail(String),
}

/// Records by case. Any marker that does not belong to this session's
/// identity is a protocol error for the whole session.
fn parse_log(bundle: &Bundle, log: &str) -> Result<BTreeMap<CaseIndex, Vec<Record>>, String> {
	let mut records: BTreeMap<CaseIndex, Vec<Record>> = BTreeMap::new();
	for (number, line) in log.lines().enumerate() {
		if !line.contains("FOCH_TEST_") {
			continue;
		}
		let context = |message: &str| format!("game.log line {}: {message}", number + 1);
		let (_, event) = line
			.split_once("EVENT [")
			.ok_or_else(|| context("test marker is not an engine EVENT record"))?;
		let (date, payload) = event
			.split_once("]:")
			.ok_or_else(|| context("malformed EVENT date"))?;
		let date = GameDate::parse(date).map_err(|_| context("invalid EVENT date"))?;
		let rest = payload
			.trim()
			.strip_prefix(bundle.marker_prefix.as_str())
			.and_then(|rest| rest.strip_prefix(" c"))
			.ok_or_else(|| context("foreign run identity or unsupported protocol"))?;
		let (case, record) = rest
			.split_once(' ')
			.ok_or_else(|| context("malformed record"))?;
		let case = CaseIndex(case.parse().map_err(|_| context("malformed case index"))?);
		if !bundle.cases.contains(&case) {
			return Err(context("record for a case outside this session"));
		}
		let kind = match record
			.split_ascii_whitespace()
			.collect::<Vec<_>>()
			.as_slice()
		{
			["BEGIN"] => RecordKind::Begin,
			["END"] => RecordKind::End,
			["PASS", check] => RecordKind::Pass((*check).into()),
			["FAIL", check] => RecordKind::Fail((*check).into()),
			_ => return Err(context("malformed or unknown test record")),
		};
		records.entry(case).or_default().push(Record { date, kind });
	}
	Ok(records)
}

struct Verdict {
	passed: Vec<String>,
	failed: Vec<CheckFailure>,
	missing: Vec<String>,
}

/// Strictly checks one case's records: BEGIN first, END last, each check at
/// most once, in plan order, on its planned engine date.
fn evaluate(
	checks: &[&PlannedCheck],
	begin: GameDate,
	end: GameDate,
	records: &[Record],
) -> Result<Verdict, String> {
	let mut began = false;
	let mut ended = false;
	let mut seen: BTreeMap<String, bool> = BTreeMap::new();
	let mut last = None;
	for record in records {
		if ended {
			return Err("record appears after END".into());
		}
		match &record.kind {
			RecordKind::Begin => {
				if began {
					return Err("duplicate BEGIN".into());
				}
				expect_date(record.date, begin, "BEGIN")?;
				began = true;
			}
			RecordKind::End => {
				if !began {
					return Err("END before BEGIN".into());
				}
				expect_date(record.date, end, "END")?;
				ended = true;
			}
			RecordKind::Pass(label) | RecordKind::Fail(label) => {
				if !began {
					return Err("check before BEGIN".into());
				}
				let position = checks
					.iter()
					.position(|check| &check.kind.label() == label)
					.ok_or_else(|| format!("unknown check {label}"))?;
				if seen.contains_key(label) {
					return Err(format!("duplicate result for {label}"));
				}
				if last.is_some_and(|previous| position < previous) {
					return Err(format!("out-of-order check {label}"));
				}
				expect_date(record.date, checks[position].date, label)?;
				last = Some(position);
				seen.insert(label.clone(), matches!(record.kind, RecordKind::Pass(_)));
			}
		}
	}
	let mut verdict = Verdict {
		passed: Vec::new(),
		failed: Vec::new(),
		missing: Vec::new(),
	};
	if !began {
		verdict.missing.push("BEGIN".into());
	}
	for check in checks {
		let label = check.kind.label();
		match seen.get(&label) {
			Some(true) => verdict.passed.push(label),
			Some(false) => verdict.failed.push(CheckFailure {
				message: match &check.kind {
					CheckKind::Scope => "the case did not run in its declared country".into(),
					CheckKind::Fixture { fixture, .. } => {
						format!("fixture {fixture} precondition is false")
					}
					CheckKind::Expect { .. } => "condition is false".into(),
					CheckKind::Invoked { .. } => "event invocation failed".into(),
				},
				check: label,
				span: Some(check.span.clone()),
			}),
			None => verdict.missing.push(label),
		}
	}
	if !ended {
		verdict.missing.push("END".into());
	}
	Ok(verdict)
}

fn expect_date(actual: GameDate, expected: GameDate, stage: &str) -> Result<(), String> {
	if actual == expected {
		Ok(())
	} else {
		Err(format!(
			"{stage} engine date is {actual}; expected {expected}"
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn error_entries_group_continuation_lines() {
		let log = "\
[gui.cpp:1]: missing sprite\n\
[events.cpp:2]: unexpected token\n\tin foch_test_mod_abc.1\n\
\n\
[save.cpp:3]: warning";
		assert_eq!(
			error_entries(log),
			[
				"[gui.cpp:1]: missing sprite",
				"[events.cpp:2]: unexpected token in foch_test_mod_abc.1",
				"[save.cpp:3]: warning",
			]
		);
	}

	#[test]
	fn only_test_layer_entries_are_attributed() {
		let namespace = "foch_test_mymod_abc123";
		let test_mod = "My Mod tests [abc123]";
		let log = format!(
			"[pdx_d3d9]: device lost\n\
			[localization.cpp:9]: missing key KEY_X\n\
			[events.cpp:2]: invalid effect in {namespace}.txt\n\
			[mods.cpp:4]: could not load mod \"{test_mod}\"\n\
			[map.cpp:7]: province 1 has no owner",
		);
		let analysis = analyze_error_log(namespace, test_mod, &log);
		assert_eq!(analysis.attributed.len(), 2);
		assert!(analysis.attributed[0].contains(namespace));
		assert_eq!(analysis.ambient, 3);
		assert_eq!(analysis.ambient_sample.len(), 3);
		assert!(
			analysis
				.attributed_summary()
				.contains("generated test layer")
		);
		assert!(analysis.ambient_summary().contains("not failing this run"));
	}

	#[test]
	fn ambient_only_log_attributes_nothing() {
		let analysis = analyze_error_log(
			"foch_test_x_000000",
			"X tests [000000]",
			"[a.cpp:1]: one\n[b.cpp:2]: two",
		);
		assert!(analysis.attributed.is_empty());
		assert_eq!(analysis.ambient, 2);
	}
}
