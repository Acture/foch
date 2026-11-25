//! Orders for one general on a player army.
//!
//! A leader has one personality from `common/leader_personalities`. Admirals
//! are out of scope. Idea groups and government attributes are names only.
//! AI countries are not given an order.

use crate::ideas::NameTable;
use crate::policy::{FEATURES, Situation};

/// What the player's general does with the army. The two marches are the
/// route choice: the fast road risks interception, the safe road costs a
/// month.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneralOrder {
	Engage,
	Siege,
	Hold,
	MarchFast,
	MarchSafe,
}

/// How many orders a policy scores. Every order array has this length.
pub const ORDER_COUNT: usize = 5;

/// The instruction issued to the one army this general leads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmyInstruction {
	pub army: i32,
	pub order: GeneralOrder,
}

/// `type` on the wargoal object in the database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarGoalKind {
	Superiority,
	TakeProvince,
	TakeCapital,
	TakeCore,
	TakeBorder,
	TakeRegion,
	TakeColony,
	DefendCapital,
	DefendCountry,
	BlockadePorts,
	NavalSuperiority,
}

impl WarGoalKind {
	pub const fn name(self) -> &'static str {
		match self {
			Self::Superiority => "superiority",
			Self::TakeProvince => "take_province",
			Self::TakeCapital => "take_capital",
			Self::TakeCore => "take_core",
			Self::TakeBorder => "take_border",
			Self::TakeRegion => "take_region",
			Self::TakeColony => "take_colony",
			Self::DefendCapital => "defend_capital",
			Self::DefendCountry => "defend_country",
			Self::BlockadePorts => "blockade_ports",
			Self::NavalSuperiority => "naval_superiority",
		}
	}
}

/// The war goal the player's general is trying to complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WarGoal {
	pub kind: WarGoalKind,
	/// Set when the goal is a province (`take_province`, `take_capital`, and the other `take_*` / `defend_capital` goals).
	pub province: Option<i32>,
}

/// Score deltas the game passed to `AddEffectiveWarScore`. This crate does not compute them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WarScoreLog {
	deltas: Vec<i32>,
}

impl WarScoreLog {
	pub fn add_effective(&mut self, delta: i32) {
		self.deltas.push(delta);
	}

	pub fn total(&self) -> i32 {
		self.deltas.iter().sum()
	}
}

/// One order's observed result. `warscore` is [`WarScoreLog::total`], not a formula.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionCost {
	pub warscore: i32,
	pub manpower_lost: f32,
	pub gold_spent: f32,
}

impl ActionCost {
	pub fn from_hook(score: &WarScoreLog, manpower_lost: f32, gold_spent: f32) -> Self {
		Self {
			warscore: score.total(),
			manpower_lost,
			gold_spent,
		}
	}
}

/// One order's reward: the war score it collected, minus ten thousand men
/// and a hundred ducats per point. This is the objective the environment
/// pays and the trainer groups on.
pub fn action_reward(action: ActionCost) -> f32 {
	action.warscore as f32 - action.manpower_lost / 10_000.0 - action.gold_spent / 100.0
}

/// War score gained per unit of cost. Higher is cheaper progress.
pub fn warscore_per_cost(action: ActionCost) -> f32 {
	let cost = action.manpower_lost / 1000.0 + action.gold_spent / 100.0;
	let score = action.warscore as f32;
	if cost <= f32::EPSILON {
		score
	} else {
		score / cost
	}
}

/// The order that raised war score for the least cost. Ties keep the earlier order.
pub fn cheapest_order(costs: [ActionCost; ORDER_COUNT]) -> GeneralOrder {
	let orders = [
		GeneralOrder::Engage,
		GeneralOrder::Siege,
		GeneralOrder::Hold,
		GeneralOrder::MarchFast,
		GeneralOrder::MarchSafe,
	];
	let mut best = f32::NEG_INFINITY;
	let mut chosen = GeneralOrder::Engage;
	for (cost, order) in costs.into_iter().zip(orders) {
		let efficiency = warscore_per_cost(cost);
		if efficiency > best {
			best = efficiency;
			chosen = order;
		}
	}
	chosen
}

/// One row per order.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneralPolicy {
	pub rows: [[f32; FEATURES]; ORDER_COUNT],
}

impl GeneralPolicy {
	pub const fn zeros() -> Self {
		Self {
			rows: [[0.0; FEATURES]; ORDER_COUNT],
		}
	}

	pub fn order(&self, situation: &Situation) -> Option<GeneralOrder> {
		let features = crate::policy::encode(situation);
		let orders = [
			GeneralOrder::Engage,
			GeneralOrder::Siege,
			GeneralOrder::Hold,
			GeneralOrder::MarchFast,
			GeneralOrder::MarchSafe,
		];
		let mut best = f32::NEG_INFINITY;
		let mut winners = 0usize;
		let mut chosen = GeneralOrder::Engage;
		for (row, order) in self.rows.iter().zip(orders) {
			let score: f32 = row
				.iter()
				.zip(features)
				.map(|(weight, value)| weight * value)
				.sum();
			if score > best {
				best = score;
				winners = 1;
				chosen = order;
			} else if score == best {
				winners += 1;
			}
		}
		if winners == 1 { Some(chosen) } else { None }
	}
}

/// The single army a player general is attached to.
#[derive(Clone, Copy, Debug)]
pub struct PlayerArmy<'a> {
	pub player: bool,
	/// The one army this leader commands. A general is not given a second army.
	pub army: i32,
	pub personality: &'a str,
	pub ideas: &'a [&'a str],
	pub attributes: &'a [&'a str],
	pub war_goal: WarGoal,
	/// Name on the wargoal object after [`crate::WARGOAL_LOAD`] returns.
	pub wargoal_name: &'a str,
}

/// The situation one army is scored in: name features plus its wargoal.
pub fn army_situation(table: &NameTable, army: &PlayerArmy<'_>) -> Situation {
	let mut situation =
		Situation::from_names(table, army.ideas, &[army.personality], army.attributes);
	situation.target = table.embed(army.wargoal_name);
	situation
}

/// `None` when this is not the player, or the head does not pick a single order.
pub fn player_general_order(
	policy: &GeneralPolicy,
	table: &NameTable,
	army: &PlayerArmy<'_>,
) -> Option<ArmyInstruction> {
	if !army.player {
		return None;
	}
	let order = policy.order(&army_situation(table, army))?;
	Some(ArmyInstruction {
		army: army.army,
		order,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ideas::EMBED;
	use crate::loaded::{LoadedDatabase, LoadedSnapshot};

	#[test]
	fn only_the_player_general_gets_an_order() {
		let mut loaded = LoadedSnapshot::default();
		loaded.note_definition(
			LoadedDatabase::LeaderPersonalities,
			"glory_seeker_personality",
		);
		loaded.note_definition(LoadedDatabase::Ideas, "offensive_ideas");
		loaded.note_definition(LoadedDatabase::GovernmentReforms, "steppe_horde");
		let ideas: Vec<&str> = loaded.idea_groups().collect();
		let attributes: Vec<&str> = loaded.reform_names().collect();
		let table = NameTable::default();
		let situation =
			Situation::from_names(&table, &ideas, &["glory_seeker_personality"], &attributes);
		let mut policy = GeneralPolicy::zeros();
		for column in 0..EMBED {
			policy.rows[0][EMBED + column] = situation.personalities[column];
		}
		let army = PlayerArmy {
			player: true,
			army: 7,
			personality: "glory_seeker_personality",
			ideas: &ideas,
			attributes: &attributes,
			war_goal: WarGoal {
				kind: WarGoalKind::TakeProvince,
				province: Some(183),
			},
			wargoal_name: "take_province",
		};
		assert_eq!(
			player_general_order(&policy, &table, &army),
			Some(ArmyInstruction {
				army: 7,
				order: GeneralOrder::Engage,
			})
		);
		let mut other = army;
		other.player = false;
		assert_eq!(player_general_order(&policy, &table, &other), None);
	}

	#[test]
	fn the_label_is_the_order_that_buys_the_most_war_score() {
		let mut battle_score = WarScoreLog::default();
		battle_score.add_effective(6);
		battle_score.add_effective(4);
		let mut siege_score = WarScoreLog::default();
		siege_score.add_effective(10);
		let costly_battle = ActionCost::from_hook(&battle_score, 8_000.0, 0.0);
		let cheap_siege = ActionCost::from_hook(&siege_score, 1_000.0, 0.0);
		let nothing = ActionCost::from_hook(&WarScoreLog::default(), 0.0, 0.0);
		assert_eq!(
			cheapest_order([costly_battle, cheap_siege, nothing, nothing, nothing]),
			GeneralOrder::Siege
		);
		let mut battle_only = WarScoreLog::default();
		battle_only.add_effective(15);
		let mut siege_only = WarScoreLog::default();
		siege_only.add_effective(2);
		let battle = ActionCost::from_hook(&battle_only, 2_000.0, 0.0);
		let siege = ActionCost::from_hook(&siege_only, 1_000.0, 0.0);
		assert_eq!(
			cheapest_order([battle, siege, nothing, nothing, nothing]),
			GeneralOrder::Engage
		);
	}
}
