use foch_plugin_directed_pacts::pact::*;
use foch_plugin_directed_pacts::war::*;

const FRA: CountryId = CountryId(1);
const SWE: CountryId = CountryId(2);
const HAB: CountryId = CountryId(3);
const ENG: CountryId = CountryId(4);
const DAN: CountryId = CountryId(5);

fn world() -> World {
	World {
		campaign: "campaign-one".into(),
		day: 100,
		revision: 1,
		countries: [
			(FRA, "FRA"),
			(SWE, "SWE"),
			(HAB, "HAB"),
			(ENG, "ENG"),
			(DAN, "DAN"),
		]
		.into_iter()
		.map(|(id, tag)| {
			(
				id,
				Country {
					tag: tag.into(),
					independent: Some(true),
				},
			)
		})
		.collect(),
		wars: vec![],
	}
}

fn war(id: u32, attacker: CountryId, defender: CountryId) -> War {
	War {
		id: WarId {
			serial: id,
			started: 101,
			declarer: attacker,
			declared_on: defender,
		},
		attackers: vec![attacker],
		defenders: vec![defender],
	}
}

#[derive(Default)]
struct Bridge {
	joins: Vec<JoinRequest>,
	validations: usize,
	join_calls: usize,
	refuse: Option<JoinRefusal>,
}

impl WarBridge for Bridge {
	fn validate_join(&mut self, request: &JoinRequest) -> Result<(), JoinRefusal> {
		self.validations += 1;
		assert_ne!(request.country, request.war.declarer);
		self.refuse.clone().map_or(Ok(()), Err)
	}
	fn join_specific_war(&mut self, request: &JoinRequest) -> Result<JoinReceipt, JoinRefusal> {
		self.join_calls += 1;
		self.validate_join(request)?;
		self.joins.push(request.clone());
		Ok(JoinReceipt {
			request: request.clone(),
		})
	}
}

fn accepted(world: &World, scope: Scope, reciprocal: bool) -> PactBook {
	let mut book = PactBook::new(world.campaign.clone());
	let id = book
		.propose(
			world,
			FRA,
			Proposal {
				partner: SWE,
				target: HAB,
				scope,
				reciprocal,
				expires: 120,
			},
		)
		.unwrap();
	book.respond(world, SWE, id, Response::Accept).unwrap();
	book
}

#[test]
fn defensive_pact_joins_exact_matching_war_with_target_as_secondary_enemy() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Defense, false);
	state.day = 101;
	state.revision += 1;
	let mut matching = war(40, ENG, FRA);
	matching.attackers.push(HAB);
	state.wars = vec![war(41, DAN, FRA), matching.clone(), war(42, HAB, ENG)];
	let mut bridge = Bridge::default();
	let results = book.process(&state, &mut bridge).unwrap();
	assert_eq!(
		bridge.joins,
		vec![JoinRequest {
			war: matching.id,
			country: SWE,
			side: Side::Defender,
			revision: state.revision
		}]
	);
	assert_eq!(results.len(), 1);
	assert_eq!(results[0].outcome, Fulfillment::Joined);
}

#[test]
fn directed_defense_never_calls_a_join_for_unrelated_or_friendly_target_wars() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Defense, false);
	state.day = 101;
	let unrelated = war(10, ENG, FRA);
	let mut friendly_target = war(11, ENG, FRA);
	friendly_target.defenders.push(HAB);
	let mut departed_target = war(12, HAB, FRA);
	departed_target.attackers = vec![ENG];
	let mut bridge = Bridge::default();
	for defensive_war in [unrelated, friendly_target, departed_target] {
		state.revision += 1;
		// A separate war involving the named target does not broaden the pact
		// into all of FRA's defensive wars. HAB must actually oppose FRA in
		// the exact war being considered, even if HAB originally declared it.
		state.wars = vec![defensive_war, war(20, HAB, DAN)];
		assert!(book.process(&state, &mut bridge).unwrap().is_empty());
		assert!(bridge.joins.is_empty());
	}
}

#[test]
fn scope_and_reciprocity_are_explicit_and_existing_wars_are_excluded() {
	for (scope, reciprocal, expected) in [
		(Scope::Defense, false, 0),
		(Scope::Attack, false, 1),
		(Scope::Both, false, 1),
		(Scope::Both, true, 2),
	] {
		let mut state = world();
		let mut existing = war(1, HAB, FRA);
		existing.id.started = 100;
		state.wars.push(existing);
		let mut book = accepted(&state, scope, reciprocal);
		state.day = 101;
		state.wars.extend([war(2, FRA, HAB), war(3, HAB, SWE)]);
		let mut bridge = Bridge::default();
		book.process(&state, &mut bridge).unwrap();
		assert_eq!(
			bridge.joins.len(),
			expected,
			"{scope:?} reciprocal={reciprocal}"
		);
		assert!(!bridge.joins.iter().any(|join| join.war.serial == 1));
	}
}

#[test]
fn repeated_fulfillment_and_save_restore_are_idempotent() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Both, false);
	state.day = 101;
	state.wars.push(war(1, HAB, FRA));
	let mut bridge = Bridge::default();
	book.process(&state, &mut bridge).unwrap();
	book.process(&state, &mut bridge).unwrap();
	let bytes = book.save().unwrap();
	let mut restored = PactBook::restore(&state.campaign, &bytes).unwrap();
	state.revision += 1;
	restored.process(&state, &mut bridge).unwrap();
	assert_eq!(bridge.joins.len(), 1);
	assert_eq!(restored.list(), book.list());
	assert!(PactBook::restore("another-campaign", &bytes).is_err());
}

#[test]
fn illegal_entry_is_explained_and_never_marked_fulfilled() {
	for refusal in [
		JoinRefusal::Truce(HAB),
		JoinRefusal::Subject,
		JoinRefusal::OpposingWar,
		JoinRefusal::Unsupported("native eligibility unavailable".into()),
	] {
		let mut state = world();
		let mut book = accepted(&state, Scope::Defense, false);
		state.day = 101;
		state.wars.push(war(1, HAB, FRA));
		let mut bridge = Bridge {
			refuse: Some(refusal.clone()),
			..Bridge::default()
		};
		let result = book.process(&state, &mut bridge).unwrap();
		assert_eq!(result[0].outcome, Fulfillment::Blocked(refusal.clone()));
		assert!(!refusal.explanation().is_empty());
		assert!(bridge.joins.is_empty());
		assert!(book.process(&state, &mut bridge).unwrap().is_empty());
	}
}

#[test]
fn expiration_invalid_parties_and_termination_prevent_entry() {
	for mode in 0..4 {
		let mut state = world();
		let mut book = accepted(&state, Scope::Both, false);
		state.day = 101;
		match mode {
			0 => state.day = 120,
			1 => {
				state.countries.remove(&HAB);
			}
			2 => {
				state.countries.remove(&SWE);
			}
			_ => book.end(&state, FRA, PactId(1)).unwrap(),
		}
		if state.countries.contains_key(&HAB) {
			state.wars.push(war(1, HAB, FRA));
		}
		let mut bridge = Bridge::default();
		book.process(&state, &mut bridge).unwrap();
		assert!(bridge.joins.is_empty());
		assert!(matches!(book.list()[0].state, PactState::Ended(_)));
	}
}

#[test]
fn proposal_requires_distinct_sovereign_parties_and_recipient_consent() {
	let mut state = world();
	let mut book = PactBook::new(state.campaign.clone());
	let terms = Proposal {
		partner: SWE,
		target: HAB,
		scope: Scope::Both,
		reciprocal: false,
		expires: 120,
	};
	let id = book.propose(&state, FRA, terms.clone()).unwrap();
	assert!(book.respond(&state, FRA, id, Response::Accept).is_err());
	assert!(book.propose(&state, FRA, terms.clone()).is_err());
	book.respond(&state, SWE, id, Response::Reject).unwrap();
	assert!(matches!(book.list()[0].state, PactState::Rejected));
	assert!(
		book.propose(
			&state,
			FRA,
			Proposal {
				target: FRA,
				..terms.clone()
			}
		)
		.is_err()
	);
	state.countries.get_mut(&SWE).unwrap().independent = Some(false);
	assert!(book.propose(&state, FRA, terms.clone()).is_err());
	state.countries.get_mut(&SWE).unwrap().independent = None;
	assert!(book.propose(&state, FRA, terms).is_err());
}

#[test]
fn inconsistent_world_is_rejected_before_any_engine_mutation() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Both, false);
	state.day = 101;
	let mut inconsistent = war(1, HAB, FRA);
	inconsistent.defenders.push(HAB);
	state.wars.push(inconsistent);
	let mut bridge = Bridge::default();
	assert!(book.process(&state, &mut bridge).is_err());
	assert!(bridge.joins.is_empty());
}

#[test]
fn already_on_either_side_is_handled_without_an_engine_join() {
	for (side, expected) in [
		(Side::Defender, Fulfillment::AlreadyTogether),
		(
			Side::Attacker,
			Fulfillment::Blocked(JoinRefusal::AlreadyEnemy),
		),
	] {
		let mut state = world();
		let mut book = accepted(&state, Scope::Both, false);
		state.day = 101;
		let mut matching = war(1, HAB, FRA);
		match side {
			Side::Defender => matching.defenders.push(SWE),
			Side::Attacker => matching.attackers.push(SWE),
		}
		state.wars.push(matching);
		let mut bridge = Bridge::default();
		assert_eq!(
			book.process(&state, &mut bridge).unwrap()[0].outcome,
			expected
		);
		assert!(bridge.joins.is_empty());
	}
}

#[test]
fn only_new_snapshot_retries_a_refused_entry_and_reused_serial_is_a_new_war() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Both, false);
	state.day = 101;
	state.wars.push(war(1, HAB, FRA));
	let mut bridge = Bridge {
		refuse: Some(JoinRefusal::Truce(HAB)),
		..Bridge::default()
	};
	book.process(&state, &mut bridge).unwrap();
	bridge.refuse = None;
	assert!(book.process(&state, &mut bridge).unwrap().is_empty());
	state.revision += 1;
	book.process(&state, &mut bridge).unwrap();
	state.day = 102;
	state.revision += 1;
	state.wars[0].id.started = 102;
	book.process(&state, &mut bridge).unwrap();
	assert_eq!(bridge.joins.len(), 2);
}

#[test]
fn both_signatories_need_confirmed_independence_before_fulfillment() {
	for (country, independence) in [
		(FRA, Some(false)),
		(FRA, None),
		(SWE, Some(false)),
		(SWE, None),
	] {
		let mut state = world();
		let mut book = accepted(&state, Scope::Both, false);
		state.day = 101;
		state.revision += 1;
		state.countries.get_mut(&country).unwrap().independent = independence;
		state.wars.push(war(1, FRA, HAB));
		let mut bridge = Bridge::default();
		let attempts = book.process(&state, &mut bridge).unwrap();
		assert_eq!(bridge.validations, 0, "{country:?} {independence:?}");
		assert_eq!(bridge.join_calls, 0);
		assert_eq!(attempts.len(), 1);
		let Fulfillment::Blocked(reason) = &attempts[0].outcome else {
			panic!("ineligible signatories cannot fulfill a pact");
		};
		if independence == Some(false) {
			assert_eq!(*reason, JoinRefusal::Subject);
		} else {
			assert!(matches!(reason, JoinRefusal::Unsupported(_)));
			assert!(reason.explanation().contains("unverified"));
			assert!(
				reason
					.explanation()
					.contains(&state.countries[&country].tag)
			);
		}
		assert!(matches!(book.list()[0].state, PactState::Active { .. }));
		assert_eq!(book.list()[0].attempts, attempts);
		book = PactBook::restore(&state.campaign, &book.save().unwrap()).unwrap();
		assert_eq!(book.list()[0].attempts, attempts);
		assert!(book.process(&state, &mut bridge).unwrap().is_empty());
		state.countries.get_mut(&country).unwrap().independent = Some(true);
		state.revision += 1;
		assert_eq!(
			book.process(&state, &mut bridge).unwrap()[0].outcome,
			Fulfillment::Joined
		);
		assert_eq!(bridge.join_calls, 1);
		assert_eq!(bridge.joins.len(), 1);
	}
}

#[test]
fn independence_is_required_even_when_signatories_are_already_together() {
	for country in [FRA, SWE] {
		for independence in [Some(false), None] {
			let mut state = world();
			let mut book = accepted(&state, Scope::Both, false);
			state.day = 101;
			state.revision += 1;
			let mut matching = war(1, FRA, HAB);
			matching.attackers.push(SWE);
			state.wars.push(matching);
			state.countries.get_mut(&country).unwrap().independent = independence;
			let mut bridge = Bridge::default();
			assert!(matches!(
				book.process(&state, &mut bridge).unwrap()[0].outcome,
				Fulfillment::Blocked(_)
			));
			assert_eq!((bridge.validations, bridge.join_calls), (0, 0));
			assert!(matches!(book.list()[0].state, PactState::Active { .. }));
			state.countries.get_mut(&country).unwrap().independent = Some(true);
			state.revision += 1;
			assert_eq!(
				book.process(&state, &mut bridge).unwrap()[0].outcome,
				Fulfillment::AlreadyTogether
			);
			assert_eq!((bridge.validations, bridge.join_calls), (0, 0));
		}
	}
}

#[test]
fn replaced_country_identity_ends_the_pact_and_future_state_cannot_be_replayed() {
	let mut state = world();
	let mut book = accepted(&state, Scope::Both, false);
	state.countries.get_mut(&HAB).unwrap().tag = "AUS".into();
	let mut bridge = Bridge::default();
	book.process(&state, &mut bridge).unwrap();
	assert_eq!(
		book.list()[0].state,
		PactState::Ended(EndReason::CountryInvalid)
	);
	let mut state = world();
	let mut book = accepted(&state, Scope::Both, false);
	state.day = 110;
	book.process(&state, &mut bridge).unwrap();
	let mut restored = PactBook::restore(&state.campaign, &book.save().unwrap()).unwrap();
	state.day = 101;
	assert!(restored.process(&state, &mut bridge).is_err());
}

#[test]
fn forged_saved_ledger_cannot_suppress_a_different_side() {
	let state = world();
	let mut book = accepted(&state, Scope::Both, false);
	let mut later = state.clone();
	later.day = 101;
	later.wars.push(war(1, HAB, FRA));
	book.process(&later, &mut Bridge::default()).unwrap();
	let mut save: serde_json::Value = serde_json::from_slice(&book.save().unwrap()).unwrap();
	save["pacts"][0]["attempts"][0]["request"]["side"] = "attacker".into();
	assert!(PactBook::restore(&state.campaign, &serde_json::to_vec(&save).unwrap()).is_err());
}

#[test]
fn restore_rejects_overlapping_live_pacts() {
	let state = world();
	let mut incorrectly_accepted = vec![];
	for (old_scope, new_scope) in [
		(Scope::Both, Scope::Both),
		(Scope::Both, Scope::Attack),
		(Scope::Attack, Scope::Both),
		(Scope::Defense, Scope::Both),
		(Scope::Attack, Scope::Attack),
		(Scope::Defense, Scope::Defense),
	] {
		let book = accepted(&state, old_scope, false);
		let actual_save: serde_json::Value = serde_json::from_slice(&book.save().unwrap()).unwrap();
		let states = [PactState::Proposed, book.list()[0].state.clone()];
		for first in &states {
			for second in &states {
				let mut save = actual_save.clone();
				let mut duplicate = save["pacts"][0].clone();
				duplicate["id"] = 2.into();
				duplicate["state"] = serde_json::to_value(second).unwrap();
				duplicate["obligations"][0]["scope"] = serde_json::to_value(new_scope).unwrap();
				save["pacts"][0]["state"] = serde_json::to_value(first).unwrap();
				save["pacts"].as_array_mut().unwrap().push(duplicate);
				save["next_id"] = 3.into();
				if PactBook::restore(&state.campaign, &serde_json::to_vec(&save).unwrap()).is_ok() {
					incorrectly_accepted.push((
						old_scope,
						new_scope,
						first.clone(),
						second.clone(),
					));
				}
			}
		}
	}
	assert!(
		incorrectly_accepted.is_empty(),
		"accepted overlapping live pacts: {incorrectly_accepted:?}"
	);
}

#[test]
fn restore_accepts_nonoverlapping_live_pacts() {
	let state = world();
	for (first_scope, actor, partner, target, scope) in [
		(Scope::Attack, FRA, SWE, HAB, Scope::Defense),
		(Scope::Defense, FRA, SWE, HAB, Scope::Attack),
		(Scope::Both, SWE, FRA, HAB, Scope::Both),
		(Scope::Both, FRA, SWE, ENG, Scope::Both),
	] {
		let mut book = accepted(&state, first_scope, false);
		let id = book
			.propose(
				&state,
				actor,
				Proposal {
					partner,
					target,
					scope,
					reciprocal: false,
					expires: 120,
				},
			)
			.unwrap();
		book.respond(&state, partner, id, Response::Accept).unwrap();
		let restored = PactBook::restore(&state.campaign, &book.save().unwrap()).unwrap();
		assert_eq!(restored.list(), book.list());
	}
}

#[test]
fn restore_accepts_overlap_with_rejected_or_ended_history() {
	let state = world();
	for rejected in [false, true] {
		let mut book = PactBook::new(state.campaign.clone());
		let terms = Proposal {
			partner: SWE,
			target: HAB,
			scope: Scope::Both,
			reciprocal: false,
			expires: 120,
		};
		let first = book.propose(&state, FRA, terms.clone()).unwrap();
		book.respond(
			&state,
			SWE,
			first,
			if rejected {
				Response::Reject
			} else {
				Response::Accept
			},
		)
		.unwrap();
		if !rejected {
			book.end(&state, FRA, first).unwrap();
		}
		let second = book.propose(&state, FRA, terms).unwrap();
		book.respond(&state, SWE, second, Response::Accept).unwrap();
		let restored = PactBook::restore(&state.campaign, &book.save().unwrap()).unwrap();
		assert_eq!(restored.list(), book.list());
	}
}
