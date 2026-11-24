//! One command surface for a player console adapter and framework fixtures.
//! The adapter supplies the actual acting country; command text cannot override it.

use crate::pact::*;
use crate::war::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
	Propose(Proposal),
	Accept(PactId),
	Reject(PactId),
	End(PactId),
	List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
	Proposed(PactId),
	Updated(PactId),
	Pacts(Vec<Pact>),
}

impl Command {
	/// Reserved script flag payload. The engine adapter must authenticate the
	/// actual scope and consume the request only after recording its outcome.
	pub fn parse_flag(flag: &str, world: &World) -> Option<Result<Self, String>> {
		let text = flag.strip_prefix("foch_dp_cmd_")?;
		if flag.len() > 256
			|| !text
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
		{
			return Some(Err("invalid directed pact command flag".into()));
		}
		Some(Self::parse(&text.replace('_', " "), world))
	}

	pub fn parse(text: &str, world: &World) -> Result<Self, String> {
		let words: Vec<_> = text.split_ascii_whitespace().collect();
		match words.as_slice() {
			["list"] => Ok(Self::List),
			[verb @ ("accept" | "reject" | "end"), number] => {
				let id = number.parse::<u64>().ok().filter(|id| *id > 0).map(PactId).ok_or("expected a positive pact ID")?;
				Ok(match *verb { "accept" => Self::Accept(id), "reject" => Self::Reject(id), _ => Self::End(id) })
			}
			["propose", partner, target, scope, days] | ["propose", partner, target, scope, days, "reciprocal"] => {
				let lookup = |tag: &str| -> Result<CountryId, String> {
					let found: Vec<_> = world.countries.iter().filter(|(_, country)| country.tag == tag).map(|(id, _)| *id).collect();
					match found.as_slice() { [id] => Ok(*id), _ => Err(format!("country {tag} is absent or ambiguous")) }
				};
				let scope = match *scope { "attack" => Scope::Attack, "defense" => Scope::Defense, "both" => Scope::Both, _ => return Err("scope must be attack, defense or both".into()) };
				let duration = days.parse::<i32>().ok().filter(|days| (1..=36500).contains(days)).ok_or("duration must be 1..36500 days")?;
				let expires = world.day.checked_add(duration).ok_or("expiry date overflow")?;
				Ok(Self::Propose(Proposal { partner: lookup(partner)?, target: lookup(target)?, scope, reciprocal: words.len() == 6, expires }))
			}
			_ => Err("expected: propose PARTNER TARGET attack|defense|both DAYS [reciprocal], accept ID, reject ID, end ID, or list".into()),
		}
	}
}

pub fn execute(
	book: &mut PactBook,
	world: &World,
	actor: CountryId,
	command: Command,
) -> Result<Reply, String> {
	match command {
		Command::Propose(proposal) => book.propose(world, actor, proposal).map(Reply::Proposed),
		Command::Accept(id) => book
			.respond(world, actor, id, Response::Accept)
			.map(|()| Reply::Updated(id)),
		Command::Reject(id) => book
			.respond(world, actor, id, Response::Reject)
			.map(|()| Reply::Updated(id)),
		Command::End(id) => book.end(world, actor, id).map(|()| Reply::Updated(id)),
		Command::List => book.inspect(world, actor).map(Reply::Pacts),
	}
}
