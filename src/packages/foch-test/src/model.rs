//! The test model shared by every stage.
//!
//! Validated values such as [`GameDate`] and [`Tag`] come from
//! `foch-annotation`, which parses them for annotations and editors alike;
//! later stages never re-validate them.

use crate::source::{RelPath, SourceSpan};
pub use foch_annotation::builtin::MAX_ADVANCE_DAYS;
pub use foch_annotation::value::{Clause, EventId, GameDate, NativeBlock, Tag, identifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

macro_rules! string_newtype {
	($name:ident, $parse:path) => {
		impl $name {
			pub fn as_str(&self) -> &str {
				&self.0
			}
		}

		impl TryFrom<String> for $name {
			type Error = String;

			fn try_from(value: String) -> Result<Self, String> {
				$parse(&value)
			}
		}

		impl From<$name> for String {
			fn from(value: $name) -> Self {
				value.0
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
				formatter.write_str(&self.0)
			}
		}
	};
}

/// Name of a fixture: an ASCII identifier.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FixtureName(String);

impl FixtureName {
	pub fn parse(value: &str) -> Result<Self, String> {
		if identifier(value) && value.len() <= 128 {
			Ok(Self(value.into()))
		} else {
			Err(format!(
				"invalid fixture name {value:?}; expected an identifier"
			))
		}
	}
}
string_newtype!(FixtureName, FixtureName::parse);

/// Identifies one execution of the test command. It appears in generated
/// script and log markers, so it is restricted to a safe alphabet.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RunId(String);

impl RunId {
	pub fn parse(value: &str) -> Result<Self, String> {
		if !value.is_empty()
			&& value.len() <= 64
			&& value
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
		{
			Ok(Self(value.into()))
		} else {
			Err("run ID must contain 1..=64 ASCII letters, digits, underscores or hyphens".into())
		}
	}
}
string_newtype!(RunId, RunId::parse);

pub(crate) fn valid_case_name(name: &str) -> Result<(), String> {
	if name.trim().is_empty()
		|| name.chars().count() > 128
		|| name.chars().any(char::is_control)
		|| name.contains("::")
		|| name.contains(['[', ']'])
		|| name.starts_with('#')
	{
		Err("test name must contain 1..=128 characters, without control characters, \"::\", brackets or a leading #".into())
	} else {
		Ok(())
	}
}

pub(crate) fn valid_mod_name(name: &str) -> Result<(), String> {
	if name.trim().is_empty() || name.contains("::") || name.chars().any(char::is_control) {
		Err(format!(
			"invalid mod name {name:?}; it must be nonempty without \"::\" or control characters"
		))
	} else {
		Ok(())
	}
}

/// A case's name within its target. Unnamed inline cases use their position
/// among the annotations on the same event, which shifts when annotations are
/// added or removed.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseName {
	Named(String),
	Index(u32),
}

impl fmt::Display for CaseName {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Named(name) => formatter.write_str(name),
			Self::Index(index) => write!(formatter, "#{index}"),
		}
	}
}

/// Human-readable address of a case, modeled on pytest node IDs:
/// `my-mod::events/reforms.txt::reforms.1::grants_flag[tag=DAN]`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct NodeId {
	pub mod_name: String,
	pub path: RelPath,
	/// The event an inline annotation is attached to; `None` for `tests/` cases.
	pub target: Option<EventId>,
	pub name: CaseName,
	pub params: Vec<(String, String)>,
}

impl NodeId {
	/// The address without the mod prefix, as typed inside a single mod.
	pub fn local(&self) -> String {
		let mut text = self.path.to_string();
		if let Some(target) = &self.target {
			text.push_str("::");
			text.push_str(target.as_str());
		}
		text.push_str("::");
		text.push_str(&self.name.to_string());
		if !self.params.is_empty() {
			let params: Vec<_> = self
				.params
				.iter()
				.map(|(key, value)| format!("{key}={value}"))
				.collect();
			text.push('[');
			text.push_str(&params.join(","));
			text.push(']');
		}
		text
	}
}

impl fmt::Display for NodeId {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{}::{}", self.mod_name, self.local())
	}
}

impl FromStr for NodeId {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, String> {
		let (body, params) = match value.strip_suffix(']') {
			Some(rest) => {
				let (body, params) = rest
					.rsplit_once('[')
					.ok_or("unbalanced parameter brackets")?;
				let params = params
					.split(',')
					.map(|pair| {
						pair.split_once('=')
							.map(|(key, value)| (key.to_string(), value.to_string()))
							.ok_or_else(|| format!("invalid parameter {pair:?}"))
					})
					.collect::<Result<Vec<_>, _>>()?;
				(body, params)
			}
			None => (value, Vec::new()),
		};
		let segments: Vec<_> = body.split("::").collect();
		let (mod_name, path, target, name) = match segments.as_slice() {
			[mod_name, path, target, name] => (mod_name, path, Some(EventId::parse(target)?), name),
			[mod_name, path, name] => (mod_name, path, None, name),
			_ => {
				return Err(format!(
					"node ID {value:?} must be mod::path[::event]::name"
				));
			}
		};
		valid_mod_name(mod_name)?;
		let name = match name.strip_prefix('#') {
			Some(index) => CaseName::Index(
				index
					.parse()
					.map_err(|_| format!("invalid case index {name:?}"))?,
			),
			None => {
				valid_case_name(name)?;
				CaseName::Named((*name).into())
			}
		};
		Ok(Self {
			mod_name: (*mod_name).into(),
			path: RelPath::new(path)?,
			target,
			name,
			params,
		})
	}
}

/// Stable identity of a case's executable content: changes when its start,
/// steps, fixtures or AI mode change, but not when lines merely move or marks
/// such as `#skip` are added.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentId(pub(crate) String);

impl ContentId {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

/// Position of a case in a plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaseIndex(pub usize);

#[derive(
	Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AiMode {
	/// Computer-controlled countries act. Only on explicit request, because
	/// their decisions make results vary between runs.
	On,
	/// The default, for deterministic results: computer-controlled countries
	/// do not act. Requires a runner that declares this capability; what
	/// remains active (random events, monthly updates) depends on the
	/// verified runner mechanism.
	#[default]
	Off,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRequest {
	/// May share one game session with compatible cases.
	#[default]
	Auto,
	/// Must run in its own game session.
	Alone,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Start {
	pub date: GameDate,
	pub tag: Tag,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Step {
	Effect { block: NativeBlock },
	Fire { event: EventId, span: SourceSpan },
	AdvanceDays { days: u32, span: SourceSpan },
	Expect { clauses: Vec<Clause> },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Marker {
	pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct XFail {
	pub reason: Option<String>,
	/// An unexpected pass fails the run.
	pub strict: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Marks {
	pub skip: Option<Marker>,
	pub ignore: Option<Marker>,
	pub xfail: Option<XFail>,
	pub labels: BTreeSet<String>,
	pub session: Option<SessionRequest>,
	/// The case must control the player country, so it cannot share a session.
	pub requires_player: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestCase {
	pub node: NodeId,
	pub content_id: ContentId,
	pub origin: SourceSpan,
	pub start: Start,
	pub ai: AiMode,
	pub uses: Vec<FixtureName>,
	pub steps: Vec<Step>,
	pub marks: Marks,
}

impl TestCase {
	/// A case without any expectation only proves that its steps completed.
	pub fn is_smoke(&self) -> bool {
		!self
			.steps
			.iter()
			.any(|step| matches!(step, Step::Expect { .. }))
	}

	pub(crate) fn compute_content_id(&mut self) {
		#[derive(Serialize)]
		struct Identity<'a> {
			node: String,
			start: &'a Start,
			ai: AiMode,
			uses: &'a [FixtureName],
			steps: Vec<String>,
		}
		let steps = self
			.steps
			.iter()
			.map(|step| match step {
				Step::Effect { block } => format!("effect {}", block.text),
				Step::Fire { event, .. } => format!("fire {event}"),
				Step::AdvanceDays { days, .. } => format!("advance {days}"),
				Step::Expect { clauses } => {
					let clauses: Vec<_> =
						clauses.iter().map(|clause| clause.text.as_str()).collect();
					format!("expect {}", clauses.join("\n"))
				}
			})
			.collect();
		let identity = Identity {
			node: self.node.to_string(),
			start: &self.start,
			ai: self.ai,
			uses: &self.uses,
			steps,
		};
		self.content_id = ContentId(digest(&identity));
	}
}

/// A named, reusable preparation step, executed in the case's country scope
/// before the case's own steps. There is no teardown: every session is
/// discarded after it runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
	pub name: FixtureName,
	pub origin: SourceSpan,
	pub uses: Vec<FixtureName>,
	pub effect: Option<NativeBlock>,
	/// Preconditions after `effect`; a false clause is a setup error, not a
	/// failure of the tested behavior.
	pub check: Vec<Clause>,
}

/// A top-level `country_event` seen while collecting, used to validate `fire`
/// targets and to estimate what a case touches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventInfo {
	pub id: EventId,
	pub origin: SourceSpan,
	/// Only hidden, triggered-only country events can be invoked directly.
	pub invocable: bool,
	/// The event block without its outer braces.
	pub body: String,
	/// Defined below `tests/events/`, so it only exists in the test layer.
	pub helper: bool,
}

/// A helper event file copied into the generated test layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelperFile {
	pub path: RelPath,
	pub text: String,
}

pub(crate) fn digest(value: &impl Serialize) -> String {
	let bytes = serde_json::to_vec(value).expect("test model values serialize");
	format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn run_ids_reject_script_injection() {
		assert!(RunId::parse("run-1_a").is_ok());
		for bad in ["", "bad run", "../x", "x\"", &"x".repeat(65)] {
			assert!(RunId::parse(bad).is_err(), "{bad:?}");
		}
		assert!(FixtureName::parse("at_war").is_ok());
		assert!(FixtureName::parse("1war").is_err());
	}

	#[test]
	fn node_ids_round_trip() {
		for text in [
			"My Mod::events/reforms.txt::reforms.1::grants flag",
			"m::events/a.txt::a.1::#2",
			"m::tests/war.txt::declare_then_peace",
			"m::events/a.txt::a.1::union[tag=DAN,time=1500.1.1]",
		] {
			let id: NodeId = text.parse().unwrap();
			assert_eq!(id.to_string(), text);
		}
		assert!("m::a.txt".parse::<NodeId>().is_err());
		assert!("m::a.txt::x::y::z".parse::<NodeId>().is_err());
	}
}
