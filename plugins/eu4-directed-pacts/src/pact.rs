//! Explicit directed obligations, consent, lifecycle and exact-war fulfillment.

use crate::war::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const FORMAT: u32 = 1;
const MAX_SAVE_BYTES: usize = 1_048_576;
const MAX_PACTS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct PactId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
	Attack,
	Defense,
	Both,
}

impl Scope {
	fn matches(self, side: Side) -> bool {
		matches!(
			(self, side),
			(Self::Both, _) | (Self::Attack, Side::Attacker) | (Self::Defense, Side::Defender)
		)
	}
	fn overlaps(self, other: Self) -> bool {
		self == other || self == Self::Both || other == Self::Both
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchRule {
	/// Beneficiary personally declared or was declared on; target must be
	/// an actual enemy participant, including a non-leading participant.
	PrincipalWarAgainstEnemyParticipant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Obligation {
	pub obligor: CountryId,
	pub beneficiary: CountryId,
	pub scope: Scope,
	pub matching: MatchRule,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
	pub partner: CountryId,
	pub target: CountryId,
	pub scope: Scope,
	pub reciprocal: bool,
	/// Exclusive deadline, in the provider's game-day epoch.
	pub expires: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
	Expired,
	CountryInvalid,
	Terminated,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PactState {
	Proposed,
	Active {
		accepted_on: i32,
		excluded_wars: BTreeSet<WarId>,
	},
	Rejected,
	Ended(EndReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
	Accept,
	Reject,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fulfillment {
	Joined,
	AlreadyTogether,
	Blocked(JoinRefusal),
}

impl Fulfillment {
	fn complete(&self) -> bool {
		!matches!(self, Self::Blocked(_))
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
	pub pact: PactId,
	pub obligation: usize,
	pub request: JoinRequest,
	pub outcome: Fulfillment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pact {
	pub id: PactId,
	pub proposer: CountryId,
	pub partner: CountryId,
	pub target: CountryId,
	pub country_tags: [String; 3],
	pub proposed_on: i32,
	pub expires: i32,
	pub obligations: Vec<Obligation>,
	pub state: PactState,
	pub attempts: Vec<Attempt>,
}

impl Pact {
	fn live(&self) -> bool {
		matches!(self.state, PactState::Proposed | PactState::Active { .. })
	}
	fn participants(&self) -> [CountryId; 3] {
		[self.proposer, self.partner, self.target]
	}
	fn refresh(&mut self, world: &World) {
		if !self.live() {
			return;
		}
		if world.day >= self.expires {
			self.state = PactState::Ended(EndReason::Expired);
		} else if self
			.participants()
			.iter()
			.zip(&self.country_tags)
			.any(|(id, tag)| {
				world
					.countries
					.get(id)
					.is_none_or(|country| country.tag != *tag)
			}) {
			self.state = PactState::Ended(EndReason::CountryInvalid);
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PactBook {
	format: u32,
	campaign: String,
	next_id: u64,
	observed_day: Option<i32>,
	pacts: Vec<Pact>,
}

impl PactBook {
	pub fn new(campaign: String) -> Self {
		Self {
			format: FORMAT,
			campaign,
			next_id: 1,
			observed_day: None,
			pacts: vec![],
		}
	}
	pub fn list(&self) -> &[Pact] {
		&self.pacts
	}
	pub fn inspect(&mut self, world: &World, actor: CountryId) -> Result<Vec<Pact>, String> {
		self.check_world(world)?;
		if !world.countries.contains_key(&actor) {
			return Err("acting country does not exist".into());
		}
		self.refresh(world);
		Ok(self
			.pacts
			.iter()
			.filter(|pact| pact.proposer == actor || pact.partner == actor)
			.cloned()
			.collect())
	}

	fn check_world(&self, world: &World) -> Result<(), String> {
		if self.campaign != world.campaign {
			return Err("pact state belongs to a different campaign".into());
		}
		if self.observed_day.is_some_and(|day| world.day < day) {
			return Err(
				"pact state is newer than the loaded game; restore the matching save payload"
					.into(),
			);
		}
		world.validate()
	}

	fn refresh(&mut self, world: &World) {
		self.observed_day = Some(world.day);
		for pact in &mut self.pacts {
			pact.refresh(world);
		}
	}

	fn eligible_parties(
		world: &World,
		proposer: CountryId,
		proposal: &Proposal,
	) -> Result<(), String> {
		let countries = [proposer, proposal.partner, proposal.target];
		if countries.into_iter().collect::<BTreeSet<_>>().len() != 3 {
			return Err("signatories and target must be three distinct countries".into());
		}
		if countries.iter().any(|id| !world.countries.contains_key(id)) {
			return Err("a signatory or target no longer exists".into());
		}
		if [proposer, proposal.partner]
			.iter()
			.any(|id| world.countries[id].independent != Some(true))
		{
			return Err("independent foreign policy must be verified for both signatories".into());
		}
		if world.enemies(proposer, proposal.partner) {
			return Err("signatories are currently enemies".into());
		}
		if proposal.expires <= world.day || proposal.expires as i64 - world.day as i64 > 36500 {
			return Err("validity must be between 1 and 36500 days".into());
		}
		Ok(())
	}

	pub fn propose(
		&mut self,
		world: &World,
		actor: CountryId,
		proposal: Proposal,
	) -> Result<PactId, String> {
		self.check_world(world)?;
		self.refresh(world);
		Self::eligible_parties(world, actor, &proposal)?;
		let mut obligations = vec![Obligation {
			obligor: proposal.partner,
			beneficiary: actor,
			scope: proposal.scope,
			matching: MatchRule::PrincipalWarAgainstEnemyParticipant,
		}];
		if proposal.reciprocal {
			obligations.push(Obligation {
				obligor: actor,
				beneficiary: proposal.partner,
				..obligations[0].clone()
			});
		}
		if self.pacts.iter().any(|pact| {
			pact.live()
				&& pact.target == proposal.target
				&& pact.obligations.iter().any(|old| {
					obligations.iter().any(|new| {
						old.obligor == new.obligor
							&& old.beneficiary == new.beneficiary
							&& old.scope.overlaps(new.scope)
					})
				})
		}) {
			return Err("a pending or active pact already covers this directed obligation".into());
		}
		if self.pacts.len() >= MAX_PACTS {
			return Err("campaign pact limit reached".into());
		}
		let id = PactId(self.next_id);
		let next = self
			.next_id
			.checked_add(1)
			.ok_or("pact identity exhausted")?;
		self.pacts.push(Pact {
			id,
			proposer: actor,
			partner: proposal.partner,
			target: proposal.target,
			country_tags: [actor, proposal.partner, proposal.target]
				.map(|id| world.countries[&id].tag.clone()),
			proposed_on: world.day,
			expires: proposal.expires,
			obligations,
			state: PactState::Proposed,
			attempts: vec![],
		});
		self.next_id = next;
		Ok(id)
	}

	pub fn respond(
		&mut self,
		world: &World,
		actor: CountryId,
		id: PactId,
		response: Response,
	) -> Result<(), String> {
		self.check_world(world)?;
		self.refresh(world);
		let pact = self
			.pacts
			.iter_mut()
			.find(|pact| pact.id == id)
			.ok_or("unknown pact")?;
		if pact.partner != actor || pact.state != PactState::Proposed {
			return Err("only the recipient may answer a pending proposal".into());
		}
		if response == Response::Reject {
			pact.state = PactState::Rejected;
			return Ok(());
		}
		Self::eligible_parties(
			world,
			pact.proposer,
			&Proposal {
				partner: pact.partner,
				target: pact.target,
				scope: pact.obligations[0].scope,
				reciprocal: pact.obligations.len() == 2,
				expires: pact.expires,
			},
		)?;
		pact.state = PactState::Active {
			accepted_on: world.day,
			excluded_wars: world.wars.iter().map(|war| war.id).collect(),
		};
		Ok(())
	}

	pub fn end(&mut self, world: &World, actor: CountryId, id: PactId) -> Result<(), String> {
		self.check_world(world)?;
		self.refresh(world);
		let pact = self
			.pacts
			.iter_mut()
			.find(|pact| pact.id == id)
			.ok_or("unknown pact")?;
		if actor != pact.proposer && actor != pact.partner {
			return Err("only signatories may end a pact".into());
		}
		if !pact.live() {
			return Err("pact has already ended".into());
		}
		pact.state = PactState::Ended(EndReason::Terminated);
		Ok(())
	}

	pub fn process(
		&mut self,
		world: &World,
		bridge: &mut impl WarBridge,
	) -> Result<Vec<Attempt>, String> {
		self.check_world(world)?;
		self.refresh(world);
		let mut results = vec![];
		for pact in &mut self.pacts {
			let PactState::Active {
				accepted_on,
				excluded_wars,
			} = &pact.state
			else {
				continue;
			};
			for (index, obligation) in pact.obligations.iter().enumerate() {
				for war in &world.wars {
					if war.id.started < *accepted_on || excluded_wars.contains(&war.id) {
						continue;
					}
					let side = if war.id.declarer == obligation.beneficiary {
						Side::Attacker
					} else if war.id.declared_on == obligation.beneficiary {
						Side::Defender
					} else {
						continue;
					};
					if !obligation.scope.matches(side)
						|| war.side(obligation.beneficiary) != Some(side)
						|| war.side(pact.target) != Some(side.opposite())
					{
						continue;
					}
					let previous = pact.attempts.iter().position(|attempt| {
						attempt.obligation == index && attempt.request.war == war.id
					});
					if previous.is_some_and(|i| {
						pact.attempts[i].outcome.complete()
							|| pact.attempts[i].request.revision == world.revision
					}) {
						continue;
					}
					let request = JoinRequest {
						war: war.id,
						country: obligation.obligor,
						side,
						revision: world.revision,
					};
					let outcome = match war.side(obligation.obligor) {
						Some(own) if own == side => Fulfillment::AlreadyTogether,
						Some(_) => Fulfillment::Blocked(JoinRefusal::AlreadyEnemy),
						None => {
							let check =
								if world.countries[&obligation.obligor].independent != Some(true) {
									Err(JoinRefusal::Subject)
								} else if war
									.participants(side)
									.iter()
									.any(|ally| world.enemies(obligation.obligor, *ally))
									|| world.wars.iter().any(|other| {
										other.side(obligation.obligor).is_some_and(|own| {
											war.participants(side.opposite())
												.iter()
												.any(|enemy| other.side(*enemy) == Some(own))
										})
									}) {
									Err(JoinRefusal::OpposingWar)
								} else {
									bridge.validate_join(&request)
								};
							match check.and_then(|()| bridge.join_specific_war(&request)) {
								Ok(receipt) if receipt.request == request => Fulfillment::Joined,
								Ok(_) => Fulfillment::Blocked(JoinRefusal::InconsistentReceipt),
								Err(reason) => Fulfillment::Blocked(reason),
							}
						}
					};
					let attempt = Attempt {
						pact: pact.id,
						obligation: index,
						request,
						outcome,
					};
					if let Some(i) = previous {
						pact.attempts[i] = attempt.clone();
					} else {
						pact.attempts.push(attempt.clone());
					}
					results.push(attempt);
				}
			}
		}
		Ok(results)
	}

	/// The host must embed these bytes in this campaign's game save. A global
	/// JSON sidecar cannot provide save-slot or rollback consistency.
	pub fn save(&self) -> Result<Vec<u8>, String> {
		let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
		if bytes.len() > MAX_SAVE_BYTES {
			return Err("pact save exceeds 1 MiB".into());
		}
		Ok(bytes)
	}

	pub fn restore(campaign: &str, bytes: &[u8]) -> Result<Self, String> {
		if bytes.len() > MAX_SAVE_BYTES {
			return Err("pact save exceeds 1 MiB".into());
		}
		let book: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
		if book.format != FORMAT
			|| book.campaign != campaign
			|| campaign.is_empty()
			|| book.pacts.len() > MAX_PACTS
			|| book.next_id == 0
		{
			return Err("unsupported pact save or campaign mismatch".into());
		}
		let mut ids = BTreeSet::new();
		for pact in &book.pacts {
			if !ids.insert(pact.id)
				|| pact.id.0 == 0
				|| pact.id.0 >= book.next_id
				|| pact.proposed_on >= pact.expires
				|| pact.obligations.is_empty()
				|| pact.obligations.len() > 2
				|| pact
					.participants()
					.into_iter()
					.collect::<BTreeSet<_>>()
					.len() != 3
			{
				return Err("invalid saved pact identity or terms".into());
			}
			if book.observed_day.is_none_or(|day| day < pact.proposed_on)
				|| pact.expires as i64 - pact.proposed_on as i64 > 36500
				|| pact.country_tags.iter().any(|tag| {
					tag.len() != 3
						|| !tag
							.bytes()
							.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
				}) {
				return Err("invalid saved country identities or dates".into());
			}
			if let PactState::Active { accepted_on, .. } = pact.state
				&& (accepted_on < pact.proposed_on
					|| accepted_on >= pact.expires
					|| book.observed_day.is_none_or(|day| day < accepted_on))
			{
				return Err("invalid saved acceptance date".into());
			}
			for (i, obligation) in pact.obligations.iter().enumerate() {
				let (obligor, beneficiary) = if i == 0 {
					(pact.partner, pact.proposer)
				} else {
					(pact.proposer, pact.partner)
				};
				if obligation.obligor != obligor || obligation.beneficiary != beneficiary {
					return Err("invalid saved obligation direction".into());
				}
			}
			let mut attempts = BTreeSet::new();
			for attempt in &pact.attempts {
				if attempt.pact != pact.id
					|| attempt.obligation >= pact.obligations.len()
					|| !attempts.insert((attempt.obligation, attempt.request.war))
					|| attempt.request.country != pact.obligations[attempt.obligation].obligor
				{
					return Err("invalid saved fulfillment identity".into());
				}
				let obligation = &pact.obligations[attempt.obligation];
				let beneficiary = match attempt.request.side {
					Side::Attacker => attempt.request.war.declarer,
					Side::Defender => attempt.request.war.declared_on,
				};
				if beneficiary != obligation.beneficiary
					|| !obligation.scope.matches(attempt.request.side)
					|| attempt.request.war.started < pact.proposed_on
					|| book
						.observed_day
						.is_none_or(|day| attempt.request.war.started > day)
				{
					return Err("invalid saved fulfillment side or war".into());
				}
			}
		}
		Ok(book)
	}
}
