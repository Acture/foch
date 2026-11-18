use foch_plugin_directed_pacts::command::{Command, Reply, execute};
use foch_plugin_directed_pacts::{pact::*, war::*};

fn world() -> World {
	World {
		campaign: "one".into(),
		day: 10,
		revision: 1,
		countries: [
			(CountryId(1), "FRA"),
			(CountryId(2), "SWE"),
			(CountryId(3), "HAB"),
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

#[test]
fn commands_have_no_caller_supplied_actor_and_show_all_terms() {
	let world = world();
	let mut book = PactBook::new(world.campaign.clone());
	let command = Command::parse("propose SWE HAB defense 365 reciprocal", &world).unwrap();
	let Reply::Proposed(id) = execute(&mut book, &world, CountryId(1), command).unwrap() else {
		panic!("proposal expected");
	};
	let list = execute(&mut book, &world, CountryId(1), Command::List).unwrap();
	let Reply::Pacts(pacts) = list else {
		panic!("list expected");
	};
	assert_eq!(pacts[0].country_tags, ["FRA", "SWE", "HAB"]);
	assert_eq!(pacts[0].expires, 375);
	assert_eq!(pacts[0].obligations.len(), 2);
	assert_eq!(pacts[0].state, PactState::Proposed);
	assert!(execute(&mut book, &world, CountryId(1), Command::Accept(id)).is_err());
	execute(&mut book, &world, CountryId(2), Command::Accept(id)).unwrap();
	execute(&mut book, &world, CountryId(2), Command::End(id)).unwrap();
	assert_eq!(
		book.list()[0].state,
		PactState::Ended(EndReason::Terminated)
	);
}

#[test]
fn parser_rejects_ambiguous_tags_unknown_commands_and_overflow() {
	let world = world();
	for text in [
		"",
		"propose SWE HAB offense 10",
		"propose SWE HAB defense 0",
		"propose SWE HAB defense 36501",
		"propose SWE HAB defense 99999999999999999999",
		"propose SWE XXX defense 1",
		"propose SWE HAB defense 10 maybe",
		"accept 0",
		"list anything",
		"accept 1 SWE",
	] {
		assert!(Command::parse(text, &world).is_err(), "{text}");
	}
}

#[test]
fn script_flags_use_the_same_parser_and_ignore_unrelated_game_flags() {
	let world = world();
	assert!(Command::parse_flag("ordinary_flag", &world).is_none());
	assert_eq!(
		Command::parse_flag("foch_dp_cmd_accept_1", &world)
			.unwrap()
			.unwrap(),
		Command::Accept(PactId(1))
	);
	assert!(
		Command::parse_flag("foch_dp_cmd_propose_SWE_HAB_both_10_bad", &world)
			.unwrap()
			.is_err()
	);
	assert_eq!(
		Command::parse_flag("foch_dp_cmd_propose_SWE_HAB_defense_365_reciprocal", &world)
			.unwrap()
			.unwrap(),
		Command::parse("propose SWE HAB defense 365 reciprocal", &world).unwrap()
	);
}

#[test]
fn list_refreshes_expiry_and_cannot_leak_another_countrys_pact() {
	let mut world = world();
	let mut book = PactBook::new(world.campaign.clone());
	execute(
		&mut book,
		&world,
		CountryId(1),
		Command::parse("propose SWE HAB both 1", &world).unwrap(),
	)
	.unwrap();
	let Reply::Pacts(pacts) = execute(&mut book, &world, CountryId(3), Command::List).unwrap()
	else {
		panic!("list expected");
	};
	assert!(pacts.is_empty());
	world.day = 11;
	let Reply::Pacts(pacts) = execute(&mut book, &world, CountryId(2), Command::List).unwrap()
	else {
		panic!("list expected");
	};
	assert_eq!(pacts[0].state, PactState::Ended(EndReason::Expired));
}
