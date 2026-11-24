use foch_plugin_directed_pacts::{command::Reply, pact::*, receipt::*, war::*};

fn pact() -> Pact {
	Pact {
		id: PactId(9),
		proposer: CountryId(1),
		partner: CountryId(2),
		target: CountryId(3),
		country_tags: ["FRA".into(), "SWE".into(), "HAB".into()],
		proposed_on: 100,
		expires: 200,
		obligations: vec![Obligation {
			obligor: CountryId(2),
			beneficiary: CountryId(1),
			scope: Scope::Defense,
			matching: MatchRule::PrincipalWarAgainstEnemyParticipant,
		}],
		state: PactState::Proposed,
		attempts: vec![joined()],
	}
}

fn joined() -> Attempt {
	Attempt {
		pact: PactId(9),
		obligation: 0,
		request: JoinRequest {
			war: WarId {
				serial: 20,
				started: 101,
				declarer: CountryId(3),
				declared_on: CountryId(1),
			},
			country: CountryId(2),
			side: Side::Defender,
			revision: 2,
		},
		outcome: Fulfillment::Joined,
	}
}

#[test]
fn correlated_script_requests_do_not_accept_an_actor_or_host_envelope() {
	let request =
		Correlation::parse_flag("foch_dp_req_7__swe_joins_hab_war__propose_SWE_HAB_defense_365")
			.unwrap()
			.unwrap();
	assert_eq!(request.0.case(), "swe_joins_hab_war");
	assert_eq!(request.0.request(), "7");
	assert_eq!(request.1, "propose SWE HAB defense 365");
	assert!(Correlation::parse_flag("unrelated_game_flag").is_none());
	for flag in [
		"foch_dp_req_0__case__list",
		"foch_dp_req_07__case__list",
		"foch_dp_req_7____list",
		"foch_dp_req_7__case__list__actor_FRA",
		"foch_dp_req_7__case\nforged__list",
	] {
		assert!(Correlation::parse_flag(flag).unwrap().is_err(), "{flag}");
	}
}

#[test]
fn actual_join_receipts_are_flat_v1_strings_with_exact_war_and_call_count() {
	let correlation = Correlation::new("swe_joins_hab_war", "7").unwrap();
	let receipt = Receipt::join(&correlation, &pact(), &joined(), 1).unwrap();
	let message = receipt.message().unwrap();
	let payload: serde_json::Value =
		serde_json::from_str(message.strip_prefix("FOCH_RECEIPT/1 ").unwrap()).unwrap();
	assert!(
		payload
			.as_object()
			.unwrap()
			.values()
			.all(|value| value.is_string())
	);
	assert_eq!(payload["type"], "join");
	assert_eq!(payload["request"], "7");
	assert_eq!(payload["status"], "joined");
	assert_eq!(payload["country"], "SWE");
	assert_eq!(payload["target"], "HAB");
	assert_eq!(payload["side"], "defender");
	assert_eq!(payload["war"], "20:101:3:1");
	assert_eq!(payload["native_calls"], "1");
	for field in [
		"run_id",
		"plugin_id",
		"pid",
		"phase",
		"version",
		"format",
		"timestamp_ms",
	] {
		assert!(payload.get(field).is_none());
	}
	assert!(Receipt::join(&correlation, &pact(), &joined(), 0).is_err());
	assert!(Receipt::join(&correlation, &pact(), &joined(), 2).is_err());
	let mut unrecorded = pact();
	unrecorded.attempts.clear();
	assert!(Receipt::join(&correlation, &unrecorded, &joined(), 1).is_err());
	assert!(Receipt::already_fulfilled(&correlation, &unrecorded, &joined()).is_err());
	let mut mismatch = joined();
	mismatch.request.country = CountryId(1);
	assert!(Receipt::join(&correlation, &pact(), &mismatch, 1).is_err());
}

#[test]
fn repeated_fulfillment_reports_zero_calls_and_refusal_preserves_explanation() {
	let correlation = Correlation::new("repeat", "8").unwrap();
	let payload = Receipt::already_fulfilled(&correlation, &pact(), &joined())
		.unwrap()
		.message()
		.unwrap();
	assert!(payload.contains("\"status\":\"already_fulfilled\""));
	assert!(payload.contains("\"native_calls\":\"0\""));
	let mut refused = joined();
	refused.outcome = Fulfillment::Blocked(JoinRefusal::Truce(CountryId(3)));
	let mut refused_pact = pact();
	refused_pact.attempts = vec![refused.clone()];
	let receipt = Receipt::join(&correlation, &refused_pact, &refused, 0)
		.unwrap()
		.message()
		.unwrap();
	assert!(receipt.contains("\"status\":\"refused\""));
	assert!(receipt.contains("truce"));
	assert!(Receipt::already_fulfilled(&correlation, &pact(), &refused).is_err());
	let command = Receipt::command(&correlation, CountryId(2), &Ok(Reply::Updated(PactId(9))))
		.message()
		.unwrap();
	assert!(command.contains("\"type\":\"command\""));
	assert!(command.contains("\"status\":\"updated\""));
}
