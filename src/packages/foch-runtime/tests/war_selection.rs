use foch_runtime::war::*;

fn world() -> World {
	World {
		campaign: "selector".into(),
		day: 100,
		revision: 1,
		countries: [
			(CountryId(1), "FRA"),
			(CountryId(2), "HAB"),
			(CountryId(3), "ENG"),
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

fn war(serial: u32, attacker: u32, defender: u32) -> War {
	War {
		id: WarId {
			serial,
			started: 90,
			declarer: CountryId(attacker),
			declared_on: CountryId(defender),
		},
		attackers: vec![CountryId(attacker)],
		defenders: vec![CountryId(defender)],
	}
}

#[test]
fn selector_uses_actual_opposing_participation_and_ignores_unrelated_wars() {
	let mut world = world();
	let mut matching = war(1, 3, 1);
	matching.attackers.push(CountryId(2));
	world.wars = vec![war(2, 2, 3), matching.clone()];
	assert_eq!(
		world
			.war_between(CountryId(1), CountryId(2))
			.unwrap()
			.unwrap()
			.id,
		matching.id
	);
	assert_eq!(
		world
			.war_between(CountryId(2), CountryId(1))
			.unwrap()
			.unwrap()
			.id,
		matching.id
	);
	assert!(world.war_between(CountryId(1), CountryId(1)).is_err());
	assert!(world.war_between(CountryId(1), CountryId(99)).is_err());
}

#[test]
fn multiple_wars_are_ambiguous_until_a_matching_identity_is_selected() {
	let mut world = world();
	let one = war(1, 1, 2);
	let two = war(2, 2, 1);
	world.wars = vec![two.clone(), one.clone()];
	assert!(matches!(
		world.war_between(CountryId(1), CountryId(2)),
		Err(WarSelectionError::Ambiguous(_))
	));
	assert_eq!(
		world
			.select_war(CountryId(1), CountryId(2), one.id)
			.unwrap()
			.id,
		one.id
	);
	assert_eq!(
		world
			.select_war(CountryId(1), CountryId(2), two.id)
			.unwrap()
			.id,
		two.id
	);
	assert!(
		world
			.select_war(CountryId(1), CountryId(3), one.id)
			.is_err()
	);
}

#[test]
fn no_war_returns_none_and_inconsistent_snapshot_is_refused() {
	let mut world = world();
	assert_eq!(world.war_between(CountryId(1), CountryId(2)).unwrap(), None);
	let mut inconsistent = war(1, 1, 2);
	inconsistent.defenders.push(CountryId(1));
	world.wars.push(inconsistent);
	assert!(world.war_between(CountryId(1), CountryId(2)).is_err());
}
