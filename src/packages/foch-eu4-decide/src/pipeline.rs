//! One pipeline from the game's load and booking calls to one army instruction.
//!
//! Names, wargoal definitions, and define values enter through the note methods.
//! A score enters only through [`Pipeline::add_effective`] while an order is
//! open on an army whose leader was assigned, and only after the wargoal
//! database and the define walk have both reported. The generation is the one
//! loaded at [`Pipeline::begin_order`]. A later override drops that group.

use std::collections::BTreeMap;

use crate::general::{
	ActionCost, ArmyInstruction, GeneralOrder, GeneralPolicy, ORDER_COUNT, PlayerArmy, WarGoal,
	WarGoalKind, WarScoreLog, action_reward, army_situation, cheapest_order, player_general_order,
	warscore_per_cost,
};
use crate::ideas::NameTable;
use crate::loaded::{LoadedDatabase, LoadedSnapshot};
use crate::policy::encode;
use crate::rules::ScoreRules;
use crate::sites::{WARGOAL_ENTRY_NAME, army_id, entry_offset, read_entry_name};

const ORDERS: [GeneralOrder; ORDER_COUNT] = [
	GeneralOrder::Engage,
	GeneralOrder::Siege,
	GeneralOrder::Hold,
	GeneralOrder::MarchFast,
	GeneralOrder::MarchSafe,
];

/// Weight decay per trained group. Constant features such as the government
/// one-hot absorb an arbitrary offset otherwise, because the booking stream
/// never gives them a gradient; that offset then decides ties between the
/// informative features. Decay pulls those weights back to zero.
pub const WEIGHT_DECAY: f32 = 0.002;

/// One define value stored on the defines object.
pub struct DefineNote<'a> {
	pub key: &'a str,
	pub value: f32,
}

/// One assign call. The army id is the dword at [`foch_eu4_state::candidates::army::ID`].
/// Personality, ideas, reform, and wargoal are objects whose names were already read.
pub struct TrackedObjects<'a> {
	pub army: &'a [u8],
	pub personality: &'a [u8],
	pub ideas: &'a [&'a [u8]],
	pub reform: Option<&'a [u8]>,
	pub wargoal: &'a [u8],
}

/// The integer `AddEffectiveWarScore` wrote, plus what that order's copy spent.
/// The pipeline stamps the rule generation itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderScore {
	pub delta: i32,
	pub manpower_lost: f32,
	pub gold_spent: f32,
}

pub struct PipelineInput<'a> {
	pub ideas: &'a [&'a [u8]],
	pub leaders: &'a [&'a [u8]],
	pub reforms: &'a [&'a [u8]],
	pub wargoals: &'a [&'a [u8]],
	pub defines: &'a [DefineNote<'a>],
	pub defines_applied: bool,
	pub tracked: TrackedObjects<'a>,
	/// Engage, siege, hold, march. Each one is booked while that order is open.
	pub scores: [OrderScore; ORDER_COUNT],
	pub learning_rate: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineOutput {
	/// Command for this army. Absent when the group was not accepted, or the head ties.
	pub instruction: Option<ArmyInstruction>,
	/// Cheapest of the four booked orders. Absent when the group was dropped.
	pub label: Option<GeneralOrder>,
	pub efficiency: f32,
	/// The four orders moved the rows.
	pub trained: bool,
	pub ideas: Vec<String>,
	pub personality: Option<String>,
	pub attributes: Vec<String>,
}

#[derive(Clone)]
struct TrackedArmy {
	personality: String,
	ideas: Vec<String>,
	reform: String,
	wargoal: String,
}

struct OpenOrder {
	army: i32,
	index: usize,
	score: WarScoreLog,
	generation: u64,
}

#[derive(Clone, Copy)]
struct Stamped {
	army: i32,
	cost: ActionCost,
	rules: u64,
}

/// Hook calls in the order the game makes them, then one commit.
pub struct Pipeline {
	loaded: LoadedSnapshot,
	rules: ScoreRules,
	tracked: BTreeMap<i32, TrackedArmy>,
	policy: GeneralPolicy,
	table: NameTable,
	open: Option<OpenOrder>,
	booked: [Option<Stamped>; ORDER_COUNT],
	learning_rate: f32,
	/// Running mean advantage per order. Without it, an uneven mix of
	/// booking streams tilts the shared feature rows toward whatever order
	/// is usually cheapest, and the wargoal signal drowns.
	baseline: [f32; ORDER_COUNT],
	groups: u32,
}

impl Pipeline {
	pub fn new(policy: GeneralPolicy, table: NameTable, learning_rate: f32) -> Self {
		Self {
			loaded: LoadedSnapshot::default(),
			rules: ScoreRules::default(),
			tracked: BTreeMap::new(),
			policy,
			table,
			open: None,
			booked: [None; ORDER_COUNT],
			learning_rate,
			baseline: [0.0; ORDER_COUNT],
			groups: 0,
		}
	}

	pub fn into_policy(self) -> GeneralPolicy {
		self.policy
	}

	/// Copy each object's name from [`crate::EntryName::offset`]. Returns how many names were stored.
	pub fn read_objects<F>(
		&mut self,
		database: LoadedDatabase,
		objects: &[&[u8]],
		mut heap: F,
	) -> usize
	where
		F: FnMut(u64, usize) -> Option<Vec<u8>>,
	{
		let Some(offset) = entry_offset(database) else {
			return 0;
		};
		let mut stored = 0;
		for object in objects {
			let Some(name) = read_entry_name(object, offset, &mut heap) else {
				continue;
			};
			self.loaded.note_definition(database, &name);
			stored += 1;
		}
		stored
	}

	/// Copy each wargoal object's name from [`WARGOAL_ENTRY_NAME`].
	pub fn read_wargoals<F>(&mut self, objects: &[&[u8]], mut heap: F) -> usize
	where
		F: FnMut(u64, usize) -> Option<Vec<u8>>,
	{
		let mut stored = 0;
		for object in objects {
			let Some(name) = read_entry_name(object, WARGOAL_ENTRY_NAME, &mut heap) else {
				continue;
			};
			self.rules.note_wargoal(&name);
			stored += 1;
		}
		stored
	}

	pub(crate) fn ready(&self) -> bool {
		self.rules.ready()
	}

	pub fn generation(&self) -> u64 {
		self.rules.generation()
	}

	pub fn rules_identity(&self) -> u64 {
		self.rules.identity()
	}

	pub(crate) fn leader_names(&self) -> Vec<String> {
		self.loaded
			.leader_personalities()
			.map(str::to_string)
			.collect()
	}

	pub fn note_define(&mut self, key: &str, value: f32) {
		self.rules.note_define(key, value);
	}

	pub fn note_defines_applied(&mut self) {
		self.rules.note_defines_applied();
	}

	/// Read the army id at [`foch_eu4_state::candidates::army::ID`] and the names on the other objects.
	/// Every name must already have been read from the database. Returns the army id.
	pub fn track<F>(&mut self, tracked: &TrackedObjects<'_>, mut heap: F) -> Option<i32>
	where
		F: FnMut(u64, usize) -> Option<Vec<u8>>,
	{
		let army = army_id(tracked.army)?;
		let personality_offset = entry_offset(LoadedDatabase::LeaderPersonalities)?;
		let personality = read_entry_name(tracked.personality, personality_offset, &mut heap)?;
		if !self
			.loaded
			.leader_personalities()
			.any(|known| known == personality)
		{
			return None;
		}
		let idea_offset = entry_offset(LoadedDatabase::Ideas)?;
		let mut ideas = Vec::new();
		for object in tracked.ideas {
			let name = read_entry_name(object, idea_offset, &mut heap)?;
			if !self.loaded.idea_groups().any(|known| known == name) {
				return None;
			}
			if !ideas.contains(&name) {
				ideas.push(name);
			}
		}
		let reform = match tracked.reform {
			Some(object) => {
				let offset = entry_offset(LoadedDatabase::GovernmentReforms)?;
				let name = read_entry_name(object, offset, &mut heap)?;
				if !self.loaded.reform_names().any(|known| known == name) {
					return None;
				}
				name
			}
			None => String::new(),
		};
		let wargoal = read_entry_name(tracked.wargoal, WARGOAL_ENTRY_NAME, &mut heap)?;
		if !self.rules.wargoals().any(|known| known == wargoal) {
			return None;
		}
		self.tracked.insert(
			army,
			TrackedArmy {
				personality,
				ideas,
				reform,
				wargoal,
			},
		);
		Some(army)
	}

	/// Start one order on an assigned army. The score that follows belongs to this generation.
	pub fn begin_order(&mut self, army: i32, order: GeneralOrder) -> bool {
		if self.open.is_some() || !self.rules.ready() || !self.tracked.contains_key(&army) {
			return false;
		}
		self.open = Some(OpenOrder {
			army,
			index: order_index(order),
			score: WarScoreLog::default(),
			generation: self.rules.generation(),
		});
		true
	}

	/// The integer at `r8` in `AddEffectiveWarScore`. Ignored unless an order is open.
	pub fn add_effective(&mut self, delta: i32) -> bool {
		let Some(open) = self.open.as_mut() else {
			return false;
		};
		open.score.add_effective(delta);
		true
	}

	/// Close the open order. A define or wargoal override since [`Self::begin_order`] drops the group.
	pub fn end_order(&mut self, manpower_lost: f32, gold_spent: f32) -> bool {
		let Some(open) = self.open.take() else {
			return false;
		};
		if self.rules.generation() != open.generation {
			self.clear_booked();
			return false;
		}
		self.booked[open.index] = Some(Stamped {
			army: open.army,
			cost: ActionCost::from_hook(&open.score, manpower_lost, gold_spent),
			rules: open.generation,
		});
		true
	}

	pub fn commit(&mut self, army: i32) -> PipelineOutput {
		let Some(tracked) = self.tracked.get(&army).cloned() else {
			return idle();
		};
		let idea_refs: Vec<&str> = tracked.ideas.iter().map(String::as_str).collect();
		let reform_slot = [tracked.reform.as_str()];
		let attributes: &[&str] = if tracked.reform.is_empty() {
			&[]
		} else {
			&reform_slot
		};
		let view = PlayerArmy {
			player: true,
			army,
			personality: &tracked.personality,
			ideas: &idea_refs,
			attributes,
			war_goal: WarGoal {
				kind: WarGoalKind::Superiority,
				province: None,
			},
			wargoal_name: &tracked.wargoal,
		};
		let accepted = self.group_accepted(&view);
		let costs = self.costs();
		let label = accepted.then_some(cheapest_order(costs));
		let trained = accepted && self.apply_group(&view, costs);
		let instruction = if accepted {
			player_general_order(&self.policy, &self.table, &view)
		} else {
			None
		};
		let efficiency = label
			.map(|order| warscore_per_cost(costs[order_index(order)]))
			.unwrap_or(0.0);
		self.clear_booked();
		PipelineOutput {
			instruction,
			label,
			efficiency,
			trained,
			ideas: tracked.ideas,
			personality: Some(tracked.personality),
			attributes: attributes.iter().map(|name| (*name).to_string()).collect(),
		}
	}

	fn group_accepted(&self, army: &PlayerArmy<'_>) -> bool {
		if self.open.is_some() {
			return false;
		}
		let Some(tracked) = self.tracked.get(&army.army) else {
			return false;
		};
		if tracked.personality != army.personality || tracked.wargoal != army.wargoal_name {
			return false;
		}
		let generation = self.rules.generation();
		self.booked.iter().all(|slot| {
			slot.as_ref()
				.is_some_and(|stamped| stamped.army == army.army && stamped.rules == generation)
		})
	}

	fn costs(&self) -> [ActionCost; ORDER_COUNT] {
		self.booked.each_ref().map(|slot| {
			slot.as_ref()
				.map(|stamped| stamped.cost)
				.unwrap_or(ActionCost {
					warscore: 0,
					manpower_lost: 0.0,
					gold_spent: 0.0,
				})
		})
	}

	fn apply_group(&mut self, army: &PlayerArmy<'_>, costs: [ActionCost; ORDER_COUNT]) -> bool {
		let Some(raw) = advantages(costs.map(action_reward)) else {
			return false;
		};
		let mut centered = [0.0; ORDER_COUNT];
		for index in 0..ORDER_COUNT {
			centered[index] = raw[index] - self.baseline[index];
		}
		let rate = 1.0 / (self.groups + 1) as f32;
		for (mean, advantage) in self.baseline.iter_mut().zip(raw) {
			*mean += rate * (advantage - *mean);
		}
		self.groups += 1;
		let features = encode(&army_situation(&self.table, army));
		for (row, advantage) in self.policy.rows.iter_mut().zip(centered) {
			for weight in row.iter_mut() {
				*weight *= 1.0 - WEIGHT_DECAY;
			}
			for (weight, feature) in row.iter_mut().zip(features) {
				*weight += self.learning_rate * advantage * feature;
			}
		}
		true
	}

	fn clear_booked(&mut self) {
		self.booked = [None; ORDER_COUNT];
		self.open = None;
	}
}

/// Replay one batch of database objects and booking calls through [`Pipeline`].
pub fn run<F>(
	policy: &mut GeneralPolicy,
	table: &NameTable,
	input: &PipelineInput<'_>,
	mut heap: F,
) -> PipelineOutput
where
	F: FnMut(u64, usize) -> Option<Vec<u8>>,
{
	let mut pipeline = Pipeline::new(policy.clone(), table.clone(), input.learning_rate);
	pipeline.read_objects(LoadedDatabase::Ideas, input.ideas, &mut heap);
	pipeline.read_objects(
		LoadedDatabase::LeaderPersonalities,
		input.leaders,
		&mut heap,
	);
	pipeline.read_objects(LoadedDatabase::GovernmentReforms, input.reforms, &mut heap);
	pipeline.read_wargoals(input.wargoals, &mut heap);
	for define in input.defines {
		pipeline.note_define(define.key, define.value);
	}
	if input.defines_applied {
		pipeline.note_defines_applied();
	}
	let army = pipeline.track(&input.tracked, &mut heap);
	if let Some(army) = army {
		for (order, score) in ORDERS.into_iter().zip(input.scores) {
			if pipeline.begin_order(army, order) {
				pipeline.add_effective(score.delta);
				pipeline.end_order(score.manpower_lost, score.gold_spent);
			}
		}
	}
	let output = match army {
		Some(army) => pipeline.commit(army),
		None => idle(),
	};
	*policy = pipeline.into_policy();
	output
}

fn idle() -> PipelineOutput {
	PipelineOutput {
		instruction: None,
		label: None,
		efficiency: 0.0,
		trained: false,
		ideas: Vec::new(),
		personality: None,
		attributes: Vec::new(),
	}
}

fn order_index(order: GeneralOrder) -> usize {
	match order {
		GeneralOrder::Engage => 0,
		GeneralOrder::Siege => 1,
		GeneralOrder::Hold => 2,
		GeneralOrder::MarchFast => 3,
		GeneralOrder::MarchSafe => 4,
	}
}

fn advantages(rewards: [f32; ORDER_COUNT]) -> Option<[f32; ORDER_COUNT]> {
	let mean = rewards.iter().copied().sum::<f32>() / ORDER_COUNT as f32;
	let variance = rewards
		.iter()
		.map(|reward| {
			let gap = reward - mean;
			gap * gap
		})
		.sum::<f32>()
		/ ORDER_COUNT as f32;
	let std = variance.sqrt();
	if std <= f32::EPSILON {
		None
	} else {
		Some(rewards.map(|reward| (reward - mean) / std))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ideas::EMBED;
	use crate::sites::{EntryHeap, WARGOAL_ENTRY_NAME, entry_offset};

	fn army_bytes(id: i32) -> Vec<u8> {
		let mut bytes = vec![0u8; 0x28];
		bytes[0x24..0x28].copy_from_slice(&id.to_le_bytes());
		bytes
	}

	fn load_army(pipeline: &mut Pipeline, heap: &mut EntryHeap) -> i32 {
		let defensive = heap.store(
			entry_offset(LoadedDatabase::Ideas).unwrap(),
			"defensive_ideas",
		);
		let offensive = heap.store(
			entry_offset(LoadedDatabase::Ideas).unwrap(),
			"offensive_ideas",
		);
		let leader = heap.store(
			entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
			"glory_seeker_personality",
		);
		let reform = heap.store(
			entry_offset(LoadedDatabase::GovernmentReforms).unwrap(),
			"merchant_republic",
		);
		let wargoal = heap.store(WARGOAL_ENTRY_NAME, "take_province");
		let idea_refs = [defensive.as_slice(), offensive.as_slice()];
		pipeline.read_objects(
			LoadedDatabase::Ideas,
			&[&defensive, &offensive],
			|pointer, size| heap.read(pointer, size),
		);
		pipeline.read_objects(
			LoadedDatabase::LeaderPersonalities,
			&[&leader],
			|pointer, size| heap.read(pointer, size),
		);
		pipeline.read_objects(
			LoadedDatabase::GovernmentReforms,
			&[&reform],
			|pointer, size| heap.read(pointer, size),
		);
		pipeline.read_wargoals(&[&wargoal], |pointer, size| heap.read(pointer, size));
		pipeline.note_defines_applied();
		let army = army_bytes(7);
		pipeline
			.track(
				&TrackedObjects {
					army: &army,
					personality: &leader,
					ideas: &idea_refs,
					reform: Some(&reform),
					wargoal: &wargoal,
				},
				|pointer, size| heap.read(pointer, size),
			)
			.expect("tracked army")
	}

	fn book_group(pipeline: &mut Pipeline) {
		let scores = [
			// The costly engagement collects little; the siege pays well.
			(GeneralOrder::Engage, 2, 8_000.0),
			(GeneralOrder::Siege, 10, 1_000.0),
			(GeneralOrder::Hold, 0, 0.0),
			(GeneralOrder::MarchFast, 0, 0.0),
			(GeneralOrder::MarchSafe, 0, 0.0),
		];
		for (order, delta, manpower) in scores {
			assert!(pipeline.begin_order(7, order));
			assert!(pipeline.add_effective(delta));
			assert!(pipeline.end_order(manpower, 0.0));
		}
	}

	#[test]
	fn names_read_from_the_database_objects_become_one_army_instruction() {
		let mut heap = EntryHeap::default();
		let defensive = heap.store(
			entry_offset(LoadedDatabase::Ideas).unwrap(),
			"defensive_ideas",
		);
		let offensive = heap.store(
			entry_offset(LoadedDatabase::Ideas).unwrap(),
			"offensive_ideas",
		);
		let leader = heap.store(
			entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
			"glory_seeker_personality",
		);
		let reform = heap.store(
			entry_offset(LoadedDatabase::GovernmentReforms).unwrap(),
			"merchant_republic",
		);
		let wargoal = heap.store(WARGOAL_ENTRY_NAME, "take_province");
		let ideas = [
			defensive.as_slice(),
			offensive.as_slice(),
			defensive.as_slice(),
		];
		let leaders = [leader.as_slice()];
		let reforms = [reform.as_slice()];
		let wargoals = [wargoal.as_slice()];
		let army = army_bytes(7);
		let tracked_ideas = [defensive.as_slice(), offensive.as_slice()];
		let scores = [
			OrderScore {
				delta: 2,
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
		];
		let input = PipelineInput {
			ideas: &ideas,
			leaders: &leaders,
			reforms: &reforms,
			wargoals: &wargoals,
			defines: &[],
			defines_applied: true,
			tracked: TrackedObjects {
				army: &army,
				personality: &leader,
				ideas: &tracked_ideas,
				reform: Some(&reform),
				wargoal: &wargoal,
			},
			scores,
			learning_rate: 0.2,
		};
		let table = NameTable::default();
		let mut policy = GeneralPolicy::zeros();
		let probe = crate::policy::Situation::from_names(
			&table,
			&["offensive_ideas"],
			&["glory_seeker_personality"],
			&["merchant_republic"],
		);
		for column in 0..EMBED {
			policy.rows[0][EMBED + column] = probe.personalities[column];
		}
		let output = run(&mut policy, &table, &input, |pointer, size| {
			heap.read(pointer, size)
		});
		assert_eq!(
			output.ideas,
			vec!["defensive_ideas".to_string(), "offensive_ideas".to_string()]
		);
		assert_eq!(
			output.personality.as_deref(),
			Some("glory_seeker_personality")
		);
		assert_eq!(output.attributes, vec!["merchant_republic".to_string()]);
		assert_eq!(output.label, Some(GeneralOrder::Siege));
		assert!(output.trained);
		assert_eq!(
			output.instruction,
			Some(ArmyInstruction {
				army: 7,
				order: GeneralOrder::Siege,
			})
		);
	}

	#[test]
	fn a_score_before_the_rules_and_the_assignment_is_dropped() {
		let mut pipeline = Pipeline::new(GeneralPolicy::zeros(), NameTable::default(), 0.2);
		let mut heap = EntryHeap::default();
		let wargoal = heap.store(WARGOAL_ENTRY_NAME, "take_province");
		let leader = heap.store(
			entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
			"glory_seeker_personality",
		);
		pipeline.read_wargoals(&[&wargoal], |pointer, size| heap.read(pointer, size));
		pipeline.read_objects(
			LoadedDatabase::LeaderPersonalities,
			&[&leader],
			|pointer, size| heap.read(pointer, size),
		);
		assert!(!pipeline.begin_order(7, GeneralOrder::Siege));
		assert!(!pipeline.add_effective(10));
		pipeline.note_defines_applied();
		assert!(!pipeline.begin_order(7, GeneralOrder::Siege));
		let army = army_bytes(7);
		let unknown = heap.store(
			entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
			"not_in_the_database",
		);
		assert!(
			pipeline
				.track(
					&TrackedObjects {
						army: &army,
						personality: &unknown,
						ideas: &[],
						reform: None,
						wargoal: &wargoal,
					},
					|pointer, size| heap.read(pointer, size),
				)
				.is_none()
		);
		assert_eq!(
			pipeline.track(
				&TrackedObjects {
					army: &army,
					personality: &leader,
					ideas: &[],
					reform: None,
					wargoal: &wargoal,
				},
				|pointer, size| heap.read(pointer, size),
			),
			Some(7)
		);
		assert!(pipeline.begin_order(7, GeneralOrder::Siege));
		assert!(pipeline.add_effective(10));
		assert_eq!(pipeline.commit(99).instruction, None);
	}

	#[test]
	fn a_define_override_after_booking_drops_the_group() {
		let mut pipeline = Pipeline::new(GeneralPolicy::zeros(), NameTable::default(), 0.2);
		let mut heap = EntryHeap::default();
		let army = load_army(&mut pipeline, &mut heap);
		pipeline.note_define("WARSCORE_MAX_FROM_BATTLES", 40.0);
		book_group(&mut pipeline);
		pipeline.note_define("WARSCORE_MAX_FROM_BATTLES", 80.0);
		let dropped = pipeline.commit(army);
		assert!(!dropped.trained);
		assert_eq!(dropped.instruction, None);
		assert_eq!(dropped.label, None);
		assert_eq!(pipeline.into_policy(), GeneralPolicy::zeros());
	}

	#[test]
	fn the_same_calls_after_the_override_train_one_group() {
		let mut pipeline = Pipeline::new(GeneralPolicy::zeros(), NameTable::default(), 0.2);
		let mut heap = EntryHeap::default();
		let army = load_army(&mut pipeline, &mut heap);
		pipeline.note_define("WARSCORE_MAX_FROM_BATTLES", 80.0);
		book_group(&mut pipeline);
		let output = pipeline.commit(army);
		assert!(output.trained);
		assert_eq!(output.label, Some(GeneralOrder::Siege));
		assert!(output.instruction.is_some());
	}
}
