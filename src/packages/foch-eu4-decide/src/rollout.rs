//! Generated rollouts for the one player army.
//!
//! Each episode is sampled from names read off the database objects.
//! Booking goes through [`crate::Pipeline`]: the order is opened, the integer
//! `AddEffectiveWarScore` wrote is appended, then the order is closed. The
//! pipeline stamps the loaded rule generation. A mod override drops the group.

use crate::general::{GeneralOrder, GeneralPolicy, ORDER_COUNT};
use crate::ideas::NameTable;
use crate::loaded::LoadedDatabase;
use crate::pipeline::{OrderScore, Pipeline, TrackedObjects};
use crate::rules::ScoreRules;
use crate::sites::{WARGOAL_ENTRY_NAME, entry_offset, read_entry_name};

const ORDERS: [GeneralOrder; ORDER_COUNT] = [
	GeneralOrder::Engage,
	GeneralOrder::Siege,
	GeneralOrder::Hold,
	GeneralOrder::MarchFast,
	GeneralOrder::MarchSafe,
];

/// Where in the deterministic episode stream to start, how many episodes to
/// generate, and the step size on the four order rows. Successive calls with
/// growing `first` continue the stream instead of replaying it.
#[derive(Clone, Copy, Debug)]
pub struct Rollout {
	pub first: u32,
	pub steps: u32,
	pub learning_rate: f32,
}

/// One sampled player army. Names are the ones the load hooks stored.
#[derive(Clone, Debug, PartialEq)]
pub struct SampledArmy {
	pub army: i32,
	pub personality: String,
	pub ideas: Vec<String>,
	pub reform: String,
	/// Name on the wargoal object after the loader returns.
	pub wargoal: String,
}

/// Objects still in the databases after the load functions return.
pub struct MergedObjects<'a> {
	pub ideas: &'a [&'a [u8]],
	pub leaders: &'a [&'a [u8]],
	pub reforms: &'a [&'a [u8]],
	pub wargoals: &'a [&'a [u8]],
}

/// Sample `steps` armies from names read off those objects and book every order.
///
/// `book` is the integer `AddEffectiveWarScore` writes for that order's copy.
/// No leader object, or defines that have not been applied, generates nothing.
/// Returns how many groups changed weights.
pub fn train<F, H>(
	policy: &mut GeneralPolicy,
	table: &NameTable,
	objects: &MergedObjects<'_>,
	defines: &ScoreRules,
	rollout: Rollout,
	mut heap: H,
	mut book: F,
) -> u32
where
	F: FnMut(&SampledArmy, GeneralOrder) -> OrderScore,
	H: FnMut(u64, usize) -> Option<Vec<u8>>,
{
	if objects.leaders.is_empty() || objects.wargoals.is_empty() || !defines.defines_applied() {
		return 0;
	}
	let mut pipeline = Pipeline::new(policy.clone(), table.clone(), rollout.learning_rate);
	pipeline.read_objects(LoadedDatabase::Ideas, objects.ideas, &mut heap);
	pipeline.read_objects(
		LoadedDatabase::LeaderPersonalities,
		objects.leaders,
		&mut heap,
	);
	pipeline.read_objects(
		LoadedDatabase::GovernmentReforms,
		objects.reforms,
		&mut heap,
	);
	pipeline.read_wargoals(objects.wargoals, &mut heap);
	for (key, value) in defines.defines() {
		pipeline.note_define(key, value);
	}
	if defines.defines_applied() {
		pipeline.note_defines_applied();
	}
	if !pipeline.ready() || pipeline.leader_names().is_empty() {
		*policy = pipeline.into_policy();
		return 0;
	}
	// Names aligned with the object lists, so a pool that repeats one name
	// to balance a booking class still reports the name that was booked.
	let leader_names = aligned_names(
		&mut heap,
		objects.leaders,
		entry_offset(LoadedDatabase::LeaderPersonalities).unwrap_or(0),
	);
	let idea_names = aligned_names(
		&mut heap,
		objects.ideas,
		entry_offset(LoadedDatabase::Ideas).unwrap_or(0),
	);
	let reform_names = aligned_names(
		&mut heap,
		objects.reforms,
		entry_offset(LoadedDatabase::GovernmentReforms).unwrap_or(0),
	);
	let wargoal_names = aligned_names(&mut heap, objects.wargoals, WARGOAL_ENTRY_NAME);
	let mut updated = 0;
	for index in rollout.first..rollout.first + rollout.steps {
		let draw = sample(
			objects.leaders.len(),
			objects.ideas.len(),
			objects.reforms.len(),
			objects.wargoals.len(),
			index,
		);
		let personality = objects.leaders[draw.personality];
		let idea_objects: Vec<&[u8]> = draw
			.ideas
			.iter()
			.map(|index| objects.ideas[*index])
			.collect();
		let reform = draw.reform.map(|index| objects.reforms[index]);
		let wargoal = objects.wargoals[draw.wargoal];
		let mut army_bytes = vec![0u8; 0x28];
		let id = index.saturating_add(1) as i32;
		army_bytes[0x24..0x28].copy_from_slice(&id.to_le_bytes());
		let Some(army) = pipeline.track(
			&TrackedObjects {
				army: &army_bytes,
				personality,
				ideas: &idea_objects,
				reform,
				wargoal,
			},
			&mut heap,
		) else {
			continue;
		};
		let sampled = SampledArmy {
			army,
			personality: leader_names[draw.personality].clone(),
			ideas: draw
				.ideas
				.iter()
				.map(|index| idea_names[*index].clone())
				.collect(),
			reform: draw
				.reform
				.map(|index| reform_names[index].clone())
				.unwrap_or_default(),
			wargoal: wargoal_names[draw.wargoal].clone(),
		};
		for order in ORDERS {
			let score = book(&sampled, order);
			if pipeline.begin_order(army, order) {
				pipeline.add_effective(score.delta);
				pipeline.end_order(score.manpower_lost, score.gold_spent);
			}
		}
		if pipeline.commit(army).trained {
			updated += 1;
		}
	}
	*policy = pipeline.into_policy();
	updated
}

/// One group whose four integers were already written by `AddEffectiveWarScore`.
///
/// `rules` is [`crate::ScoreRules::identity`] at booking time. Offline fitting
/// replays the integers and does not ask for a new score.
#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
	pub army: Vec<u8>,
	pub personality: Vec<u8>,
	pub ideas: Vec<Vec<u8>>,
	pub reform: Option<Vec<u8>>,
	pub wargoal: Vec<u8>,
	pub scores: [OrderScore; ORDER_COUNT],
	pub rules: u64,
}

/// Replay booked groups with EU4 not running.
///
/// A group from another rule generation is skipped. Returns how many groups changed weights.
pub fn fit<H>(
	policy: &mut GeneralPolicy,
	table: &NameTable,
	objects: &MergedObjects<'_>,
	defines: &ScoreRules,
	episodes: &[Episode],
	learning_rate: f32,
	mut heap: H,
) -> u32
where
	H: FnMut(u64, usize) -> Option<Vec<u8>>,
{
	if objects.leaders.is_empty() || objects.wargoals.is_empty() || !defines.defines_applied() {
		return 0;
	}
	let mut pipeline = Pipeline::new(policy.clone(), table.clone(), learning_rate);
	pipeline.read_objects(LoadedDatabase::Ideas, objects.ideas, &mut heap);
	pipeline.read_objects(
		LoadedDatabase::LeaderPersonalities,
		objects.leaders,
		&mut heap,
	);
	pipeline.read_objects(
		LoadedDatabase::GovernmentReforms,
		objects.reforms,
		&mut heap,
	);
	pipeline.read_wargoals(objects.wargoals, &mut heap);
	for (key, value) in defines.defines() {
		pipeline.note_define(key, value);
	}
	if defines.defines_applied() {
		pipeline.note_defines_applied();
	}
	if !pipeline.ready() {
		*policy = pipeline.into_policy();
		return 0;
	}
	let identity = pipeline.rules_identity();
	let mut updated = 0;
	for episode in episodes {
		if episode.rules != identity {
			continue;
		}
		let ideas: Vec<&[u8]> = episode.ideas.iter().map(Vec::as_slice).collect();
		let Some(army) = pipeline.track(
			&TrackedObjects {
				army: &episode.army,
				personality: &episode.personality,
				ideas: &ideas,
				reform: episode.reform.as_deref(),
				wargoal: &episode.wargoal,
			},
			&mut heap,
		) else {
			continue;
		};
		for (order, score) in ORDERS.into_iter().zip(episode.scores) {
			if pipeline.begin_order(army, order) {
				pipeline.add_effective(score.delta);
				pipeline.end_order(score.manpower_lost, score.gold_spent);
			}
		}
		if pipeline.commit(army).trained {
			updated += 1;
		}
	}
	*policy = pipeline.into_policy();
	updated
}

/// The sampler's draw for one episode index: indexes into the object
/// lists. Publishing this lets an exporter replay the exact stream the
/// trainer samples.
#[derive(Clone, Debug, PartialEq)]
pub struct Draw {
	pub personality: usize,
	pub ideas: Vec<usize>,
	pub reform: Option<usize>,
	pub wargoal: usize,
}

/// The deterministic draw for one episode index over object lists of these
/// lengths.
pub fn draw_for(
	personalities: usize,
	ideas: usize,
	reforms: usize,
	wargoals: usize,
	index: u32,
) -> Draw {
	sample(personalities, ideas, reforms, wargoals, index)
}

/// One name per object, read off the object bytes. An unreadable name is
/// empty, and the pipeline will refuse to track that object anyway.
fn aligned_names<H>(heap: &mut H, objects: &[&[u8]], offset: u32) -> Vec<String>
where
	H: FnMut(u64, usize) -> Option<Vec<u8>>,
{
	let mut names = Vec::with_capacity(objects.len());
	for object in objects {
		names.push(read_entry_name(object, offset, &mut *heap).unwrap_or_default());
	}
	names
}

fn sample(personalities: usize, ideas: usize, reforms: usize, wargoals: usize, index: u32) -> Draw {
	let draw = mix(u64::from(index));
	Draw {
		personality: (draw as usize) % personalities.max(1),
		ideas: sample_idea_indexes(ideas, draw >> 32),
		reform: (reforms > 0).then_some(((draw >> 8) as usize) % reforms),
		wargoal: ((draw >> 16) as usize) % wargoals.max(1),
	}
}

fn sample_idea_indexes(len: usize, draw: u64) -> Vec<usize> {
	if len == 0 {
		return Vec::new();
	}
	let count = (draw as usize % len.min(3)) + 1;
	let mut chosen = Vec::new();
	let mut cursor = draw;
	while chosen.len() < count {
		cursor = mix(cursor);
		let index = (cursor as usize) % len;
		if !chosen.contains(&index) {
			chosen.push(index);
		}
	}
	chosen
}

fn mix(mut value: u64) -> u64 {
	value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
	value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
	value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
	value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::general::{PlayerArmy, WarGoal, WarGoalKind, player_general_order};
	use crate::loaded::LoadedDatabase;
	use crate::pipeline::OrderScore;
	use crate::rules::ScoreRules;

	use crate::sites::{EntryHeap, WARGOAL_ENTRY_NAME, entry_offset};

	struct Fixture {
		heap: EntryHeap,
		ideas: Vec<Vec<u8>>,
		leaders: Vec<Vec<u8>>,
		reforms: Vec<Vec<u8>>,
		wargoals: Vec<Vec<u8>>,
	}

	fn fixture() -> Fixture {
		let mut heap = EntryHeap::default();
		let ideas = vec![heap.store(
			entry_offset(LoadedDatabase::Ideas).unwrap(),
			"offensive_ideas",
		)];
		let leaders = vec![heap.store(
			entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
			"glory_seeker_personality",
		)];
		let reforms = vec![heap.store(
			entry_offset(LoadedDatabase::GovernmentReforms).unwrap(),
			"merchant_republic",
		)];
		let wargoals = vec![heap.store(WARGOAL_ENTRY_NAME, "superiority")];
		Fixture {
			heap,
			ideas,
			leaders,
			reforms,
			wargoals,
		}
	}

	/// The merged objects of one fixture: ideas, leaders, reforms, wargoals.
	type Merged<'a> = (Vec<&'a [u8]>, Vec<&'a [u8]>, Vec<&'a [u8]>, Vec<&'a [u8]>);

	fn merged(fixture: &Fixture) -> Merged<'_> {
		(
			fixture.ideas.iter().map(Vec::as_slice).collect(),
			fixture.leaders.iter().map(Vec::as_slice).collect(),
			fixture.reforms.iter().map(Vec::as_slice).collect(),
			fixture.wargoals.iter().map(Vec::as_slice).collect(),
		)
	}

	fn defines() -> ScoreRules {
		let mut rules = ScoreRules::default();
		rules.note_defines_applied();
		rules
	}

	/// Integers the booking call returned. Siege spent less manpower.
	fn book(order: GeneralOrder) -> OrderScore {
		let (delta, manpower_lost) = match order {
			GeneralOrder::Siege => (10, 1_000.0),
			GeneralOrder::Engage => (10, 8_000.0),
			GeneralOrder::Hold | GeneralOrder::MarchFast | GeneralOrder::MarchSafe => (0, 0.0),
		};
		OrderScore {
			delta,
			manpower_lost,
			gold_spent: 0.0,
		}
	}

	fn rollout() -> Rollout {
		Rollout {
			first: 0,
			steps: 32,
			learning_rate: 0.2,
		}
	}

	#[test]
	fn generated_groups_move_the_row_for_the_cheaper_order() {
		let fixture = fixture();
		let (ideas, leaders, reforms, wargoals) = merged(&fixture);
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		let table = NameTable::default();
		let mut policy = GeneralPolicy::zeros();
		let updated = train(
			&mut policy,
			&table,
			&objects,
			&defines(),
			rollout(),
			|pointer, size| fixture.heap.read(pointer, size),
			|_army, order| book(order),
		);
		assert_eq!(updated, 32);
		let decision = player_general_order(
			&policy,
			&table,
			&PlayerArmy {
				player: true,
				army: 1,
				personality: "glory_seeker_personality",
				ideas: &["offensive_ideas"],
				attributes: &["merchant_republic"],
				war_goal: WarGoal {
					kind: WarGoalKind::Superiority,
					province: None,
				},
				wargoal_name: "superiority",
			},
		);
		assert_eq!(
			decision.map(|instruction| instruction.order),
			Some(GeneralOrder::Siege)
		);
	}

	#[test]
	fn no_leader_personality_generates_no_episode() {
		let mut policy = GeneralPolicy::zeros();
		let objects = MergedObjects {
			ideas: &[],
			leaders: &[],
			reforms: &[],
			wargoals: &[],
		};
		let updated = train(
			&mut policy,
			&NameTable::default(),
			&objects,
			&defines(),
			rollout(),
			|_pointer, _size| None,
			|_army, order| book(order),
		);
		assert_eq!(updated, 0);
		assert_eq!(policy, GeneralPolicy::zeros());
	}

	#[test]
	fn rules_the_game_has_not_loaded_generate_nothing() {
		let mut policy = GeneralPolicy::zeros();
		let fixture = fixture();
		let (ideas, leaders, reforms, wargoals) = merged(&fixture);
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		let updated = train(
			&mut policy,
			&NameTable::default(),
			&objects,
			&ScoreRules::default(),
			rollout(),
			|pointer, size| fixture.heap.read(pointer, size),
			|_army, order| book(order),
		);
		assert_eq!(updated, 0);
		assert_eq!(policy, GeneralPolicy::zeros());
	}

	#[test]
	fn a_tied_group_does_not_move_the_rows() {
		let fixture = fixture();
		let (ideas, leaders, reforms, wargoals) = merged(&fixture);
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		let mut policy = GeneralPolicy::zeros();
		let updated = train(
			&mut policy,
			&NameTable::default(),
			&objects,
			&defines(),
			Rollout {
				first: 0,
				steps: 4,
				learning_rate: 0.2,
			},
			|pointer, size| fixture.heap.read(pointer, size),
			|_army, _order| OrderScore {
				delta: 3,
				manpower_lost: 0.0,
				gold_spent: 0.0,
			},
		);
		assert_eq!(updated, 0);
		assert_eq!(policy, GeneralPolicy::zeros());
	}

	fn booked(fixture: &Fixture, rules: u64) -> Episode {
		let mut army = vec![0u8; 0x28];
		army[0x24..0x28].copy_from_slice(&1i32.to_le_bytes());
		Episode {
			army,
			personality: fixture.leaders[0].clone(),
			ideas: fixture.ideas.clone(),
			reform: Some(fixture.reforms[0].clone()),
			wargoal: fixture.wargoals[0].clone(),
			scores: [
				OrderScore {
					delta: 10,
					manpower_lost: 8_000.0,
					gold_spent: 0.0,
				},
				OrderScore {
					delta: 10,
					manpower_lost: 1_000.0,
					gold_spent: 0.0,
				},
				OrderScore {
					delta: 0,
					manpower_lost: 0.0,
					gold_spent: 0.0,
				},
				OrderScore {
					delta: 0,
					manpower_lost: 0.0,
					gold_spent: 0.0,
				},
				OrderScore {
					delta: 0,
					manpower_lost: 0.0,
					gold_spent: 0.0,
				},
			],
			rules,
		}
	}

	fn rules_identity(fixture: &Fixture, rules: &ScoreRules) -> u64 {
		let (ideas, leaders, reforms, wargoals) = merged(fixture);
		let mut pipeline = Pipeline::new(GeneralPolicy::zeros(), NameTable::default(), 0.2);
		pipeline.read_objects(LoadedDatabase::Ideas, &ideas, |pointer, size| {
			fixture.heap.read(pointer, size)
		});
		pipeline.read_objects(
			LoadedDatabase::LeaderPersonalities,
			&leaders,
			|pointer, size| fixture.heap.read(pointer, size),
		);
		pipeline.read_objects(
			LoadedDatabase::GovernmentReforms,
			&reforms,
			|pointer, size| fixture.heap.read(pointer, size),
		);
		pipeline.read_wargoals(&wargoals, |pointer, size| fixture.heap.read(pointer, size));
		for (key, value) in rules.defines() {
			pipeline.note_define(key, value);
		}
		if rules.defines_applied() {
			pipeline.note_defines_applied();
		}
		pipeline.rules_identity()
	}

	#[test]
	fn offline_fit_replays_booked_scores() {
		let fixture = fixture();
		let (ideas, leaders, reforms, wargoals) = merged(&fixture);
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		let rules = defines();
		let episode = booked(&fixture, rules_identity(&fixture, &rules));
		let table = NameTable::default();
		let mut policy = GeneralPolicy::zeros();
		let updated = fit(
			&mut policy,
			&table,
			&objects,
			&rules,
			&[episode],
			0.2,
			|pointer, size| fixture.heap.read(pointer, size),
		);
		assert_eq!(updated, 1);
		let decision = player_general_order(
			&policy,
			&table,
			&PlayerArmy {
				player: true,
				army: 1,
				personality: "glory_seeker_personality",
				ideas: &["offensive_ideas"],
				attributes: &["merchant_republic"],
				war_goal: WarGoal {
					kind: WarGoalKind::Superiority,
					province: None,
				},
				wargoal_name: "superiority",
			},
		);
		assert_eq!(
			decision.map(|instruction| instruction.order),
			Some(GeneralOrder::Siege)
		);
	}

	#[test]
	fn offline_fit_drops_a_group_booked_under_replaced_rules() {
		let fixture = fixture();
		let (ideas, leaders, reforms, wargoals) = merged(&fixture);
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		let mut rules = defines();
		rules.note_define("WARSCORE_MAX_FROM_BATTLES", 40.0);
		let episode = booked(&fixture, rules_identity(&fixture, &rules));
		rules.note_define("WARSCORE_MAX_FROM_BATTLES", 80.0);
		let mut policy = GeneralPolicy::zeros();
		let updated = fit(
			&mut policy,
			&NameTable::default(),
			&objects,
			&rules,
			&[episode],
			0.2,
			|pointer, size| fixture.heap.read(pointer, size),
		);
		assert_eq!(updated, 0);
		assert_eq!(policy, GeneralPolicy::zeros());
	}
}
