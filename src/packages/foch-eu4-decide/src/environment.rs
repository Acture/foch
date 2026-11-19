//! The offline booking environment: a three-month pocket war.
//!
//! The engine-true parts are the score scales. One battle is worth
//! `BATTLE_BASE + regiments_lost / BATTLE_REGIMENTS_PER_POINT` points,
//! negated for a defeat, and the war's battles together are capped by
//! `BATTLES_CAP`; both come off the decompiled `FUN_1401dd600` and
//! `WAR_SCORE_TOTAL`. An occupied province pays its share of the enemy
//! side's total through the offline war-score model, and a held war goal
//! ticks `WARGOAL_TICKING_PER_MONTH` after a `WARGOAL_BONUS` once its
//! threshold is met.
//!
//! Everything else is a labeled stand-in for the un-decompiled combat
//! simulation: the dice shapes, the siege odds, and the interception
//! chances. The scenario - enemy strength, enemy position, relief timing -
//! is drawn per episode and stays hidden from the policy, so an order is
//! only ever better in expectation.

use crate::general::{GeneralOrder, GeneralPolicy, ORDER_COUNT};
use crate::ideas::{NameTable, hadamard_row};
use crate::loaded::LoadedDatabase;
use crate::pipeline::OrderScore;
use crate::rollout::{MergedObjects, Rollout, SampledArmy, train};
use crate::rules::ScoreRules;
use crate::score::{ProvinceCost, WarScoreInput, WarScoreRules, war_score};
use crate::sites::{EntryHeap, WARGOAL_ENTRY_NAME, entry_offset};
use crate::war::WarObjects;
use foch_eu4_state::candidates::province::OCCUPIED_MASK;

/// The flat part of one battle's score, in points.
pub const BATTLE_BASE: f32 = 0.1;
/// A battle pays one point per twenty regiments destroyed.
pub const BATTLE_REGIMENTS_PER_POINT: f32 = 20.0;
/// `WARSCORE_MAX_FROM_BATTLES`.
pub const BATTLES_CAP: i32 = 40;
/// `DEFAULT_WARGOAL_WARSCORE_BONUS`.
pub const WARGOAL_BONUS: f32 = 5.0;
/// `DEFAULT_WARGOAL_TICKINGWARSCORE_BONUS`, per month.
pub const WARGOAL_TICKING_PER_MONTH: f32 = 0.4;
/// `SUPERIORITY_WARGOAL_WARSCORE_THRESHOLD`.
pub const SUPERIORITY_THRESHOLD: f32 = 10.0;
/// Chance the fast evasion road runs into the enemy army anyway.
const FAST_INTERCEPT: f64 = 0.35;
/// Chance the direct siege approach is intercepted.
const SIEGE_INTERCEPT: f64 = 0.3;
/// Monthly chance a standing siege takes the province.
const SIEGE_SUCCESS: f64 = 0.35;
/// Chance a holding army is brought to battle by the relief force.
const HOLD_ENGAGED: f64 = 0.4;
/// Months one episode runs.
const MONTHS: u32 = 3;
/// Our army, in regiments.
const OUR_REGIMENTS: f32 = 12.0;
/// Attrition the safe road bleeds per month, in regiments.
const SAFE_ATTRITION: f32 = 0.5;

/// One episode's hidden scenario, common to every order branch.
#[derive(Clone, Copy, Debug)]
pub struct Scenario {
	/// The enemy army, in regiments.
	pub enemy_regiments: f32,
	/// The enemy army starts on the war-goal province instead of in the field.
	pub enemy_at_province: bool,
	/// The month a field army comes to relieve the province.
	pub relief_month: u32,
	/// Enemy province developments; the first province is the capital target.
	pub developments: [f32; 4],
	pub provinces: usize,
}

impl Scenario {
	/// The world one army fights in. Every order branch sees this same world.
	pub fn of(army: i32) -> Self {
		let mut rng = Rng::seed(u64::from(army.unsigned_abs()) ^ 0x9e37_79b9_7f4a_7c15);
		let mut developments = [0.0; 4];
		for value in developments.iter_mut() {
			*value = 5.0 + rng.below(30) as f32;
		}
		Self {
			enemy_regiments: 8.0 + rng.below(22) as f32,
			enemy_at_province: rng.below(2) == 0,
			relief_month: 2 + u32::from(rng.below(2) == 0),
			developments,
			provinces: 2 + rng.below(3) as usize,
		}
	}

	/// The doctrine bonus a leader personality gives its battles, in
	/// regiments. A stand-in for the un-decompiled leader pip math.
	fn battle_bonus(personality: &str) -> f32 {
		match personality {
			"glory_seeker_personality" | "bold_fighter_personality" => 2.0,
			"careful_personality" => -1.0,
			_ => 0.0,
		}
	}
}

/// A small deterministic RNG for the environment's dice.
struct Rng {
	state: u64,
}

impl Rng {
	fn seed(state: u64) -> Self {
		Self { state }
	}

	fn next(&mut self) -> u64 {
		self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
		let mut z = self.state;
		z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
		z ^ (z >> 31)
	}

	fn below(&mut self, bound: u64) -> u64 {
		self.next() % bound.max(1)
	}

	fn chance(&mut self, p: f64) -> bool {
		(self.next() % 10_000) as f64 / 10_000.0 < p
	}
}

/// One month of battle: `(points, enemy regiments destroyed, our losses)`.
fn battle(rng: &mut Rng, our_effective: f32, enemy: f32) -> (f32, f32, f32) {
	let edge = (our_effective - enemy) / 8.0;
	let win_p = (0.5 + edge / 2.0).clamp(0.05, 0.95);
	if rng.chance(f64::from(win_p)) {
		let destroyed = 6.0 + rng.below(9) as f32;
		let losses = 1.0 + rng.below(4) as f32;
		(
			BATTLE_BASE + destroyed / BATTLE_REGIMENTS_PER_POINT,
			destroyed,
			losses,
		)
	} else {
		let losses = 3.0 + rng.below(8) as f32;
		let destroyed = 1.0 + rng.below(3) as f32;
		(
			-(BATTLE_BASE + losses / BATTLE_REGIMENTS_PER_POINT),
			destroyed,
			losses,
		)
	}
}

/// The war state of one scenario, with occupation paid by the score model.
struct PocketWar {
	war: WarObjects,
	costs: Vec<ProvinceCost>,
	rules: WarScoreRules,
}

impl PocketWar {
	fn build(scenario: &Scenario) -> Self {
		let mut war = WarObjects::new(1, 2);
		let mut costs = Vec::new();
		for index in 0..scenario.provinces {
			war.add_province(2, 2, 0);
			costs.push(ProvinceCost {
				development: scenario.developments[index] as i32,
				capital: index == 0,
				..ProvinceCost::default()
			});
		}
		Self {
			war,
			costs,
			rules: WarScoreRules::VANILLA,
		}
	}

	fn score(&self) -> i32 {
		let input = WarScoreInput {
			war: &self.war,
			costs: &self.costs,
			battles: 0,
		};
		war_score(&input, &self.rules)
	}

	/// Flip the war-goal province to our control and pay the share the
	/// offline score model assigns, including the enemy's quarter loss.
	fn occupy(&mut self) -> f32 {
		let before = self.score();
		self.war.reoccupy(0, 1, OCCUPIED_MASK);
		let after = self.score();
		(after - before) as f32
	}
}

/// The score one order of one army collects over the whole episode. The
/// world dice come from the army; each order branch has its own stream.
pub fn book(army: &SampledArmy, order: GeneralOrder) -> OrderScore {
	let scenario = Scenario::of(army.army);
	let bonus = Scenario::battle_bonus(&army.personality);
	let mut rng =
		Rng::seed((u64::from(army.army.unsigned_abs()) << 32) ^ u64::from(index_of(order)));
	let mut war = PocketWar::build(&scenario);
	let mut points = 0.0f32;
	let mut losses = 0.0f32;
	let mut enemy = scenario.enemy_regiments;
	let enemy_in_field = !scenario.enemy_at_province;
	let mut at_province = false;
	let mut held = false;
	let mut battle_points = 0.0f32;

	for month in 1..=MONTHS {
		let relief = enemy_in_field && month >= scenario.relief_month;
		match order {
			GeneralOrder::Engage if enemy > 0.0 => {
				let (delta, destroyed, our) = battle(&mut rng, OUR_REGIMENTS + bonus, enemy);
				enemy -= destroyed;
				battle_points += delta;
				losses += our;
			}
			GeneralOrder::Siege => {
				if !at_province {
					if enemy_in_field && enemy > 0.0 && rng.chance(SIEGE_INTERCEPT) {
						let (delta, destroyed, our) =
							battle(&mut rng, OUR_REGIMENTS + bonus - 2.0, enemy);
						enemy -= destroyed;
						battle_points += delta;
						losses += our;
					} else {
						at_province = true;
					}
				} else if !held {
					if relief && enemy > 0.0 {
						let (delta, destroyed, our) =
							battle(&mut rng, OUR_REGIMENTS + bonus - 2.0, enemy);
						enemy -= destroyed;
						battle_points += delta;
						losses += our;
						if delta < 0.0 {
							// The siege breaks and the province stays enemy.
							at_province = false;
						}
					} else if rng.chance(SIEGE_SUCCESS) {
						points += war.occupy();
						held = true;
					}
				}
			}
			GeneralOrder::MarchFast => {
				if enemy_in_field && enemy > 0.0 && rng.chance(FAST_INTERCEPT) {
					let (delta, destroyed, our) =
						battle(&mut rng, OUR_REGIMENTS + bonus - 2.0, enemy);
					enemy -= destroyed;
					battle_points += delta;
					losses += our;
				}
			}
			GeneralOrder::MarchSafe => {
				losses += SAFE_ATTRITION;
			}
			GeneralOrder::Hold => {
				if relief && enemy > 0.0 && rng.chance(HOLD_ENGAGED) {
					let (delta, destroyed, our) = battle(&mut rng, OUR_REGIMENTS + bonus, enemy);
					enemy -= destroyed;
					battle_points += delta;
					losses += our;
				}
			}
			GeneralOrder::Engage => {}
		}
		if held && army.wargoal != "superiority" {
			points += WARGOAL_TICKING_PER_MONTH;
		}
	}
	if held && army.wargoal != "superiority" {
		points += WARGOAL_BONUS;
	}
	if army.wargoal == "superiority" && battle_points >= SUPERIORITY_THRESHOLD {
		points += WARGOAL_BONUS + WARGOAL_TICKING_PER_MONTH * MONTHS as f32;
	}
	points += battle_points.clamp(-(BATTLES_CAP as f32), BATTLES_CAP as f32);
	OrderScore {
		delta: points as i32,
		manpower_lost: losses * 1000.0,
		gold_spent: 0.0,
	}
}

/// One order's reward in this environment: the war score it collected
/// minus ten thousand men per point. The manpower price is an environment
/// choice; the score scales it prices are the engine's.
pub fn rewards_of(army: &SampledArmy) -> [f32; ORDER_COUNT] {
	let mut rewards = [0.0f32; ORDER_COUNT];
	for (slot, order) in order_list().into_iter().enumerate() {
		let score = book(army, order);
		rewards[slot] = score.delta as f32 - score.manpower_lost / 10_000.0;
	}
	rewards
}

/// The order this environment pays most for this army.
pub fn paid_order(army: &SampledArmy) -> GeneralOrder {
	let rewards = rewards_of(army);
	let mut best = GeneralOrder::Hold;
	let mut top = f32::NEG_INFINITY;
	for (order, reward) in order_list().into_iter().zip(rewards) {
		if reward > top {
			top = reward;
			best = order;
		}
	}
	best
}

pub fn order_list() -> [GeneralOrder; ORDER_COUNT] {
	[
		GeneralOrder::Engage,
		GeneralOrder::Siege,
		GeneralOrder::Hold,
		GeneralOrder::MarchFast,
		GeneralOrder::MarchSafe,
	]
}

fn index_of(order: GeneralOrder) -> u32 {
	match order {
		GeneralOrder::Engage => 0,
		GeneralOrder::Siege => 1,
		GeneralOrder::Hold => 2,
		GeneralOrder::MarchFast => 3,
		GeneralOrder::MarchSafe => 4,
	}
}

/// One training pool: the objects a load left behind, plus the rules and
/// the name table they embed into.
pub struct TrainingPool {
	pub objects: MergedObjectsOwned,
	pub rules: ScoreRules,
	pub table: NameTable,
	pub heap: EntryHeap,
}

/// The merged database objects the trainer samples armies from.
pub struct MergedObjectsOwned {
	pub ideas: Vec<Vec<u8>>,
	pub leaders: Vec<Vec<u8>>,
	pub reforms: Vec<Vec<u8>>,
	pub wargoals: Vec<Vec<u8>>,
}

impl TrainingPool {
	/// Build the pool the way the loaders would have: one object per name,
	/// with the name at the offset its database keeps it.
	pub fn build(ideas: &[&str], leaders: &[&str], reforms: &[&str], wargoals: &[&str]) -> Self {
		fn store(heap: &mut EntryHeap, names: &[&str], offset: u32) -> Vec<Vec<u8>> {
			names.iter().map(|name| heap.store(offset, name)).collect()
		}
		let mut heap = EntryHeap::default();
		let objects = MergedObjectsOwned {
			ideas: store(
				&mut heap,
				ideas,
				entry_offset(LoadedDatabase::Ideas).expect("ideas offset"),
			),
			leaders: store(
				&mut heap,
				leaders,
				entry_offset(LoadedDatabase::LeaderPersonalities).expect("leader offset"),
			),
			reforms: store(
				&mut heap,
				reforms,
				entry_offset(LoadedDatabase::GovernmentReforms).expect("reform offset"),
			),
			wargoals: store(&mut heap, wargoals, WARGOAL_ENTRY_NAME),
		};
		let mut rules = ScoreRules::default();
		rules.note_defines_applied();
		let mut table = NameTable::default();
		// Seed each database's names with Hadamard rows: hash embeddings
		// collide badly on short name sets, and a pool that must separate
		// its names cannot learn through a collision. Blocks are separate
		// features, so reusing a row across databases is harmless.
		for names in [ideas, leaders, reforms, wargoals] {
			for (index, name) in names.iter().enumerate() {
				table.insert(*name, hadamard_row(index));
			}
		}
		Self {
			objects,
			rules,
			table,
			heap,
		}
	}

	/// Continue the deterministic stream at `first` for `steps` episodes and
	/// return how many groups moved the policy rows.
	pub fn train_chunk(
		&self,
		policy: &mut GeneralPolicy,
		first: u32,
		steps: u32,
		learning_rate: f32,
	) -> u32 {
		let ideas: Vec<&[u8]> = self.objects.ideas.iter().map(Vec::as_slice).collect();
		let leaders: Vec<&[u8]> = self.objects.leaders.iter().map(Vec::as_slice).collect();
		let reforms: Vec<&[u8]> = self.objects.reforms.iter().map(Vec::as_slice).collect();
		let wargoals: Vec<&[u8]> = self.objects.wargoals.iter().map(Vec::as_slice).collect();
		let objects = MergedObjects {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
		};
		train(
			policy,
			&self.table,
			&objects,
			&self.rules,
			Rollout {
				first,
				steps,
				learning_rate,
			},
			|pointer, size| self.heap.read(pointer, size),
			book,
		)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const IDEAS: &[&str] = &["offensive_ideas", "defensive_ideas", "economic_ideas"];
	const LEADERS: &[&str] = &[
		"glory_seeker_personality",
		"bold_fighter_personality",
		"careful_personality",
	];
	const REFORMS: &[&str] = &["merchant_republic", "steppe_horde"];
	const WARGOALS: &[&str] = &[
		"superiority",
		"superiority",
		"superiority",
		"take_province",
		"take_capital",
		"take_core",
	];

	fn army(id: i32, wargoal: &str, personality: &str) -> SampledArmy {
		SampledArmy {
			army: id,
			personality: personality.to_string(),
			ideas: vec!["offensive_ideas".to_string()],
			reform: String::new(),
			wargoal: wargoal.to_string(),
		}
	}

	fn value(army: &SampledArmy, order: GeneralOrder) -> f32 {
		for (candidate, reward) in order_list().into_iter().zip(rewards_of(army)) {
			if candidate == order {
				return reward;
			}
		}
		0.0
	}

	#[test]
	fn a_battle_pays_one_point_per_twenty_regiments() {
		assert_eq!(BATTLE_BASE + 20.0 / BATTLE_REGIMENTS_PER_POINT, 1.1);
	}

	#[test]
	fn every_scenario_has_an_enemy_army() {
		for id in 1..=64 {
			let scenario = Scenario::of(id);
			assert!((8.0..30.0).contains(&scenario.enemy_regiments));
			assert!((2..=4).contains(&scenario.provinces));
			assert!(matches!(scenario.relief_month, 2 | 3));
		}
	}

	#[test]
	fn engaging_a_strong_enemy_loses_more_than_a_weak_one() {
		let (mut strong, mut strong_n) = (0.0, 0.0);
		let (mut weak, mut weak_n) = (0.0, 0.0);
		for id in 1..=300 {
			let scenario = Scenario::of(id);
			let engaged = value(
				&army(id, "superiority", "glory_seeker_personality"),
				GeneralOrder::Engage,
			);
			if scenario.enemy_regiments >= 20.0 {
				strong += engaged;
				strong_n += 1.0;
			} else if scenario.enemy_regiments <= 14.0 {
				weak += engaged;
				weak_n += 1.0;
			}
		}
		assert!(strong_n > 20.0 && weak_n > 20.0, "both bands must occur");
		assert!(
			strong / strong_n < weak / weak_n,
			"a strong enemy must make engaging worse on average"
		);
	}

	#[test]
	fn the_safe_road_never_fights_and_the_fast_road_sometimes_does() {
		let mut fast_fought = 0;
		for id in 1..=300 {
			let fast = book(
				&army(id, "take_province", "glory_seeker_personality"),
				GeneralOrder::MarchFast,
			);
			let safe = book(
				&army(id, "take_province", "glory_seeker_personality"),
				GeneralOrder::MarchSafe,
			);
			assert_eq!(
				safe.manpower_lost,
				SAFE_ATTRITION * MONTHS as f32 * 1000.0,
				"the safe road only bleeds attrition"
			);
			if fast.manpower_lost > 0.0 {
				fast_fought += 1;
			}
		}
		assert!(fast_fought > 0, "the fast road must risk interception");
	}

	#[test]
	fn training_raises_the_collected_reward() {
		let pool = TrainingPool::build(IDEAS, LEADERS, REFORMS, WARGOALS);
		let mut policy = GeneralPolicy::zeros();
		let before = value_score(&pool, &policy);
		let mut rate = 0.2f32;
		for chunk in 0..24u32 {
			rate *= 0.9;
			pool.train_chunk(&mut policy, chunk * 16, 16, rate);
		}
		let after = value_score(&pool, &policy);
		assert!(
			after > before,
			"training must raise the collected reward: {after} vs {before}"
		);
	}

	fn value_score(pool: &TrainingPool, policy: &GeneralPolicy) -> f32 {
		use crate::general::{PlayerArmy, WarGoal, WarGoalKind, player_general_order};
		let mut collected = 0.0;
		let mut best = 0.0;
		for personality in LEADERS {
			for wargoal in ["superiority", "take_province", "take_capital", "take_core"] {
				for id in (901..961).step_by(4) {
					let sampled = army(id, wargoal, personality);
					let rewards = rewards_of(&sampled);
					best += rewards.iter().copied().fold(f32::NEG_INFINITY, f32::max);
					let view = PlayerArmy {
						player: true,
						army: id,
						personality,
						ideas: &["offensive_ideas"],
						attributes: &["merchant_republic"],
						war_goal: WarGoal {
							kind: WarGoalKind::Superiority,
							province: None,
						},
						wargoal_name: wargoal,
					};
					let index = player_general_order(policy, &pool.table, &view)
						.map(|instruction| instruction.order)
						.map(|order| {
							order_list()
								.into_iter()
								.position(|candidate| candidate == order)
								.unwrap_or(2)
						})
						.unwrap_or(2);
					collected += rewards[index];
				}
			}
		}
		collected / best
	}
}
