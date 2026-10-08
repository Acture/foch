//! From collected cases to game sessions.
//!
//! [`expand`] selects cases and resolves their fixtures. After [`crate::lint`]
//! has estimated what each case touches, [`group`] decides which cases share
//! a game session. Sharing is only an optimization: a case's verdict must not
//! depend on it, so any doubt keeps the case alone.

use crate::collect::Collection;
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::lint::LintResult;
use crate::model::{
	AiMode, CaseIndex, EventInfo, Fixture, FixtureName, GameDate, HelperFile, NodeId,
	SessionRequest, Tag, TestCase,
};
use crate::select::Selector;
use crate::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoredMode {
	/// Rust's default: `#ignore` cases do not run.
	#[default]
	Exclude,
	/// `--include-ignored`.
	Include,
	/// `--ignored`: run only `#ignore` cases.
	Only,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpandOptions {
	pub selector: Selector,
	pub ignored: IgnoredMode,
	/// Upper bound of cases that would run; each costs a game launch or more.
	pub max_cases: usize,
}

impl Default for ExpandOptions {
	fn default() -> Self {
		Self {
			selector: Selector::default(),
			ignored: IgnoredMode::default(),
			max_cases: 200,
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotRunReason {
	Deselected,
	Skipped { message: Option<String> },
	Ignored { message: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotRun {
	pub node: NodeId,
	pub origin: SourceSpan,
	pub reason: NotRunReason,
}

/// A selected case with its fixtures in execution order: dependencies first,
/// each fixture once.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedCase {
	pub index: CaseIndex,
	pub case: TestCase,
	pub fixtures: Vec<Fixture>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expanded {
	pub cases: Vec<PlannedCase>,
	pub not_run: Vec<NotRun>,
	pub events: Vec<EventInfo>,
	pub helpers: Vec<HelperFile>,
}

pub fn expand(
	collection: &Collection,
	options: &ExpandOptions,
) -> Result<Expanded, Vec<Diagnostic>> {
	let fixtures: BTreeMap<&FixtureName, &Fixture> = collection
		.fixtures
		.iter()
		.map(|fixture| (&fixture.name, fixture))
		.collect();
	let mut diagnostics = Vec::new();
	for fixture in &collection.fixtures {
		if let Err(diagnostic) = resolve(
			&fixture.uses,
			&fixtures,
			&[&fixture.name],
			Some(&fixture.origin),
		) {
			diagnostics.push(diagnostic);
		}
	}
	let mut cases = Vec::new();
	let mut not_run = Vec::new();
	for case in &collection.cases {
		let ignored = case.marks.ignore.is_some();
		let reason = if !options.selector.matches(case)
			|| (options.ignored == IgnoredMode::Only && !ignored)
		{
			Some(NotRunReason::Deselected)
		} else if let Some(skip) = &case.marks.skip {
			Some(NotRunReason::Skipped {
				message: skip.reason.clone(),
			})
		} else if ignored && options.ignored == IgnoredMode::Exclude {
			Some(NotRunReason::Ignored {
				message: case
					.marks
					.ignore
					.as_ref()
					.and_then(|marker| marker.reason.clone()),
			})
		} else {
			None
		};
		if let Some(reason) = reason {
			not_run.push(NotRun {
				node: case.node.clone(),
				origin: case.origin.clone(),
				reason,
			});
			continue;
		}
		match resolve(&case.uses, &fixtures, &[], Some(&case.origin)) {
			Ok(resolved) => cases.push(PlannedCase {
				index: CaseIndex(cases.len()),
				case: case.clone(),
				fixtures: resolved,
			}),
			Err(diagnostic) => diagnostics.push(diagnostic),
		}
	}
	if cases.len() > options.max_cases {
		diagnostics.push(Diagnostic::error(
			DiagnosticCode::TooManyCases,
			None,
			format!(
				"{} cases selected, more than the budget of {}; narrow the selection or raise the budget",
				cases.len(),
				options.max_cases
			),
		));
	}
	if !diagnostics.is_empty() {
		return Err(diagnostics);
	}
	Ok(Expanded {
		cases,
		not_run,
		events: collection.events.clone(),
		helpers: collection.helpers.clone(),
	})
}

/// Depth-first resolution: dependencies before dependents, each once.
fn resolve(
	uses: &[FixtureName],
	fixtures: &BTreeMap<&FixtureName, &Fixture>,
	stack: &[&FixtureName],
	origin: Option<&SourceSpan>,
) -> Result<Vec<Fixture>, Diagnostic> {
	fn visit<'a>(
		name: &'a FixtureName,
		fixtures: &BTreeMap<&'a FixtureName, &'a Fixture>,
		stack: &mut Vec<&'a FixtureName>,
		done: &mut Vec<Fixture>,
		origin: Option<&SourceSpan>,
	) -> Result<(), Diagnostic> {
		if done.iter().any(|fixture| &fixture.name == name) {
			return Ok(());
		}
		if stack.contains(&name) {
			let cycle: Vec<_> = stack
				.iter()
				.map(ToString::to_string)
				.chain([name.to_string()])
				.collect();
			return Err(Diagnostic::error(
				DiagnosticCode::FixtureCycle,
				origin.cloned(),
				format!("fixture dependency cycle: {}", cycle.join(" -> ")),
			));
		}
		let fixture = fixtures.get(name).ok_or_else(|| {
			Diagnostic::error(
				DiagnosticCode::UnknownFixture,
				origin.cloned(),
				format!("unknown fixture {name}"),
			)
		})?;
		stack.push(name);
		for dependency in &fixture.uses {
			visit(dependency, fixtures, stack, done, Some(&fixture.origin))?;
		}
		stack.pop();
		done.push((*fixture).clone());
		Ok(())
	}
	let mut stack = stack.to_vec();
	let mut done = Vec::new();
	for name in uses {
		visit(name, fixtures, &mut stack, &mut done, origin)?;
	}
	Ok(done)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupOptions {
	/// Project default for cases that do not declare `session`.
	pub default_session: SessionRequest,
	/// `--isolate`: every case runs alone.
	pub force_isolate: bool,
	/// Whether the runner declares support for several cases in one session.
	pub runner_shared_sessions: bool,
	pub max_session_cases: usize,
}

impl Default for GroupOptions {
	fn default() -> Self {
		Self {
			default_session: SessionRequest::Auto,
			force_isolate: false,
			runner_shared_sessions: false,
			max_session_cases: 32,
		}
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
	Dedicated,
	Shared,
}

/// One game launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
	pub id: SessionId,
	pub date: GameDate,
	pub ai: AiMode,
	/// The country the game starts as. Other cases in a shared session run in
	/// computer-controlled countries.
	pub player: Tag,
	pub cases: Vec<CaseIndex>,
	pub isolation: Isolation,
}

/// Why a case did not share a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AloneReason {
	UserDeclared,
	ProjectDefault,
	ForcedByCli,
	RunnerUnsupported,
	AiEnabled,
	RequiresPlayer,
	/// Touches state Foch cannot bound, such as scope changes.
	UnknownEffect {
		detail: String,
	},
	/// Touches global state such as global flags.
	GlobalState,
	SameTag {
		other: CaseIndex,
	},
	Overlap {
		other: CaseIndex,
	},
	/// No other case could share with it.
	OnlyCandidate,
	/// Re-run alone after failing in a shared session.
	IsolatedRerun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
	pub cases: Vec<PlannedCase>,
	pub not_run: Vec<NotRun>,
	pub sessions: Vec<Session>,
	pub alone_reasons: BTreeMap<CaseIndex, AloneReason>,
	pub events: Vec<EventInfo>,
	pub helpers: Vec<HelperFile>,
}

impl Plan {
	pub fn case(&self, index: CaseIndex) -> &PlannedCase {
		&self.cases[index.0]
	}

	pub fn session(&self, id: SessionId) -> &Session {
		&self.sessions[id.0]
	}

	/// A plan that runs the given cases again, each in its own session. Used
	/// to confirm a failure seen in a shared session.
	pub fn isolated_rerun(&self, cases: &[CaseIndex]) -> Plan {
		let mut sessions = Vec::new();
		let mut alone_reasons = BTreeMap::new();
		for &index in cases {
			let case = &self.case(index).case;
			alone_reasons.insert(index, AloneReason::IsolatedRerun);
			sessions.push(Session {
				id: SessionId(sessions.len()),
				date: case.start.date,
				ai: case.ai,
				player: case.start.tag.clone(),
				cases: vec![index],
				isolation: Isolation::Dedicated,
			});
		}
		Plan {
			cases: self.cases.clone(),
			not_run: Vec::new(),
			sessions,
			alone_reasons,
			events: self.events.clone(),
			helpers: self.helpers.clone(),
		}
	}
}

pub fn group(expanded: Expanded, lint: &LintResult, options: &GroupOptions) -> Plan {
	let mut alone_reasons = BTreeMap::new();
	// Candidate shared groups keyed by what one launch fixes.
	let mut groups: Vec<(GameDate, Vec<CaseIndex>, BTreeSet<Tag>)> = Vec::new();
	let mut conflicts: BTreeMap<CaseIndex, AloneReason> = BTreeMap::new();
	for planned in &expanded.cases {
		let case = &planned.case;
		let index = planned.index;
		let footprint = lint.footprints.get(&index);
		let reason = if case.marks.session == Some(SessionRequest::Alone) {
			Some(AloneReason::UserDeclared)
		} else if options.force_isolate {
			Some(AloneReason::ForcedByCli)
		} else if options.default_session == SessionRequest::Alone && case.marks.session.is_none() {
			Some(AloneReason::ProjectDefault)
		} else if !options.runner_shared_sessions {
			Some(AloneReason::RunnerUnsupported)
		} else if case.ai == AiMode::On {
			Some(AloneReason::AiEnabled)
		} else if case.marks.requires_player {
			Some(AloneReason::RequiresPlayer)
		} else {
			match footprint {
				None => Some(AloneReason::UnknownEffect {
					detail: "no static analysis result".into(),
				}),
				Some(footprint) if footprint.opaque.is_some() => Some(AloneReason::UnknownEffect {
					detail: footprint.opaque.clone().unwrap_or_default(),
				}),
				Some(footprint) if footprint.global => Some(AloneReason::GlobalState),
				Some(_) => None,
			}
		};
		if let Some(reason) = reason {
			alone_reasons.insert(index, reason);
			continue;
		}
		let tags = &footprint.expect("checked above").tags;
		let mut placed = false;
		for (date, members, used) in &mut groups {
			if *date != case.start.date || members.len() >= options.max_session_cases {
				continue;
			}
			if let Some(&other) = members
				.iter()
				.find(|&&other| expanded.cases[other.0].case.start.tag == case.start.tag)
			{
				conflicts
					.entry(index)
					.or_insert(AloneReason::SameTag { other });
				continue;
			}
			if !used.is_disjoint(tags) {
				let other = members
					.iter()
					.copied()
					.find(|other| !lint.footprints[other].tags.is_disjoint(tags))
					.unwrap_or(members[0]);
				conflicts
					.entry(index)
					.or_insert(AloneReason::Overlap { other });
				continue;
			}
			members.push(index);
			used.extend(tags.iter().cloned());
			placed = true;
			break;
		}
		if !placed {
			groups.push((case.start.date, vec![index], tags.clone()));
		}
	}

	let mut sessions = Vec::new();
	let mut shared_members = BTreeSet::new();
	for (date, members, _) in &groups {
		if members.len() > 1 {
			shared_members.extend(members.iter().copied());
			sessions.push(Session {
				id: SessionId(sessions.len()),
				date: *date,
				ai: AiMode::Off,
				player: expanded.cases[members[0].0].case.start.tag.clone(),
				cases: members.clone(),
				isolation: Isolation::Shared,
			});
		}
	}
	for planned in &expanded.cases {
		let index = planned.index;
		if shared_members.contains(&index) {
			continue;
		}
		alone_reasons.entry(index).or_insert_with(|| {
			conflicts
				.remove(&index)
				.unwrap_or(AloneReason::OnlyCandidate)
		});
		sessions.push(Session {
			id: SessionId(sessions.len()),
			date: planned.case.start.date,
			ai: planned.case.ai,
			player: planned.case.start.tag.clone(),
			cases: vec![index],
			isolation: Isolation::Dedicated,
		});
	}
	Plan {
		cases: expanded.cases,
		not_run: expanded.not_run,
		sessions,
		alone_reasons,
		events: expanded.events,
		helpers: expanded.helpers,
	}
}
