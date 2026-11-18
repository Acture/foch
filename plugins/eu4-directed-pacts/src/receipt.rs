//! Plugin-owned receipt payloads. The host supplies the execution envelope.
//!
//! Construction records an observed result; it does not execute a command,
//! enter a war, acknowledge persistence or activate the plugin.

use crate::{command::Reply, pact::*, war::*};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correlation {
	case: String,
	request: String,
}

impl Correlation {
	pub fn new(case: &str, request: &str) -> Result<Self, String> {
		if case.is_empty()
			|| case.len() > 128
			|| !case
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
			|| request.parse::<u64>().ok().filter(|id| *id > 0).is_none()
			|| request.starts_with('0')
			|| !request.bytes().all(|byte| byte.is_ascii_digit())
		{
			return Err("expected a case name and canonical positive request number".into());
		}
		Ok(Self {
			case: case.into(),
			request: request.into(),
		})
	}

	pub fn case(&self) -> &str {
		&self.case
	}
	pub fn request(&self) -> &str {
		&self.request
	}

	/// `foch_dp_req_REQUEST__CASE__COMMAND` authenticates no actor. The engine
	/// adapter must obtain that identity from the actual script country scope.
	pub fn parse_flag(flag: &str) -> Option<Result<(Self, String), String>> {
		let payload = flag.strip_prefix("foch_dp_req_")?;
		Some((|| {
			if flag.len() > 256 {
				return Err("directed pact request is too long".into());
			}
			let mut parts = payload.splitn(3, "__");
			let request = parts.next().ok_or("missing request number")?;
			let case = parts.next().ok_or("missing case name")?;
			let command = parts.next().ok_or("missing command")?;
			if command.is_empty()
				|| command.contains("__")
				|| !command
					.bytes()
					.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
			{
				return Err("invalid directed pact request command".into());
			}
			Ok((Self::new(case, request)?, command.replace('_', " ")))
		})())
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt(BTreeMap<&'static str, String>);

impl Receipt {
	fn new(correlation: &Correlation, kind: &str, status: &str) -> Self {
		Self(
			[
				("case", correlation.case.clone()),
				("request", correlation.request.clone()),
				("type", kind.into()),
				("status", status.into()),
			]
			.into(),
		)
	}

	pub fn command(
		correlation: &Correlation,
		actor: CountryId,
		result: &Result<Reply, String>,
	) -> Self {
		let status = match result {
			Ok(Reply::Proposed(_)) => "proposed",
			Ok(Reply::Updated(_)) => "updated",
			Ok(Reply::Pacts(_)) => "listed",
			Err(_) => "refused",
		};
		let mut receipt = Self::new(correlation, "command", status);
		receipt.0.insert("actor", actor.0.to_string());
		match result {
			Ok(Reply::Proposed(id) | Reply::Updated(id)) => {
				receipt.0.insert("pact", id.0.to_string());
			}
			Ok(Reply::Pacts(pacts)) => {
				receipt.0.insert("count", pacts.len().to_string());
			}
			Err(reason) => {
				receipt.0.insert("reason", reason.clone());
			}
		}
		receipt
	}

	fn entry(
		correlation: &Correlation,
		pact: &Pact,
		attempt: &Attempt,
		status: &str,
		native_calls: u32,
	) -> Result<Self, String> {
		let obligation = pact
			.obligations
			.get(attempt.obligation)
			.ok_or("receipt obligation is absent")?;
		if attempt.pact != pact.id
			|| attempt.request.country != obligation.obligor
			|| native_calls > 1
			|| !pact.attempts.contains(attempt)
		{
			return Err("receipt does not identify this pact's exact obligation".into());
		}
		let country = if obligation.obligor == pact.proposer {
			&pact.country_tags[0]
		} else if obligation.obligor == pact.partner {
			&pact.country_tags[1]
		} else {
			return Err("receipt obligor is not a signatory".into());
		};
		let war = attempt.request.war;
		let mut receipt = Self::new(correlation, "join", status);
		receipt.0.extend([
			("pact", pact.id.0.to_string()),
			("obligation", attempt.obligation.to_string()),
			("country", country.clone()),
			("target", pact.country_tags[2].clone()),
			(
				"side",
				match attempt.request.side {
					Side::Attacker => "attacker",
					Side::Defender => "defender",
				}
				.into(),
			),
			(
				"war",
				format!(
					"{}:{}:{}:{}",
					war.serial, war.started, war.declarer.0, war.declared_on.0
				),
			),
			("native_calls", native_calls.to_string()),
		]);
		Ok(receipt)
	}

	/// `native_calls` comes from the adapter's actual engine call counter,
	/// including rejected calls. It must not count bridge validation requests.
	pub fn join(
		correlation: &Correlation,
		pact: &Pact,
		attempt: &Attempt,
		native_calls: u32,
	) -> Result<Self, String> {
		let status = match &attempt.outcome {
			Fulfillment::Joined if native_calls == 1 => "joined",
			Fulfillment::AlreadyTogether if native_calls == 0 => "already_together",
			Fulfillment::Blocked(_) if native_calls <= 1 => "refused",
			_ => return Err("receipt outcome disagrees with the actual engine call count".into()),
		};
		let mut receipt = Self::entry(correlation, pact, attempt, status, native_calls)?;
		if let Fulfillment::Blocked(reason) = &attempt.outcome {
			receipt.0.insert("reason", reason.explanation());
		}
		Ok(receipt)
	}

	/// Replay observation reads the persisted successful ledger. It must never
	/// call the native join operation simply to produce a duplicate receipt.
	pub fn already_fulfilled(
		correlation: &Correlation,
		pact: &Pact,
		attempt: &Attempt,
	) -> Result<Self, String> {
		if matches!(attempt.outcome, Fulfillment::Blocked(_)) {
			return Err("a blocked attempt is not fulfilled".into());
		}
		Self::entry(correlation, pact, attempt, "already_fulfilled", 0)
	}

	/// All values are strings. Reserved run/plugin/process envelope fields are
	/// structurally unavailable to callers of this payload builder.
	pub fn message(&self) -> Result<String, String> {
		serde_json::to_string(&self.0)
			.map(|json| format!("FOCH_RECEIPT/1 {json}"))
			.map_err(|error| error.to_string())
	}
}
