//! Monthly diplomatic interests for EU4.
//!
//! Rewards are adversarial against the vanilla AI: the same horizon scored for
//! the model minus the same horizon scored for the game's own behavior.
//! Personalities, government attributes, and idea groups enter as names.
//! Hostile attitude and rival slots are not written by this crate.

mod attitude;
mod combat;
mod environment;
mod general;
mod ideas;
mod loaded;
mod offline;
mod pipeline;
mod policy;
pub use foch_eu4_gnn::{GeneralGnn, HEADS, NODE, RULES};
pub mod policy_net;
mod residual;
mod reward;
mod rollout;
mod rules;
mod score;
mod sites;
pub mod territory;
mod war;
pub mod world;

pub use attitude::{Choice, INTEREST_LIMIT, Interest, interest_window};
pub use combat::resolve_battle_slots;
pub use environment::{
	BATTLE_BASE, BATTLE_REGIMENTS_PER_POINT, BATTLES_CAP, MergedObjectsOwned,
	SUPERIORITY_THRESHOLD, Scenario, TrainingPool, WARGOAL_BONUS, WARGOAL_TICKING_PER_MONTH, book,
	order_list, paid_order, rewards_of,
};
pub use general::{
	ActionCost, ArmyInstruction, GeneralOrder, GeneralPolicy, PlayerArmy, WarGoal, WarGoalKind,
	WarScoreLog, action_reward, army_situation, cheapest_order, player_general_order,
	warscore_per_cost,
};
pub use ideas::{EMBED, HashEmbed, NameTable, hadamard_row};
pub use loaded::{LoadedDatabase, LoadedSnapshot, ResidualHeads};
pub use offline::{InterestPrediction, Month, TargetOutcomes, TargetScore, monthly};
pub use pipeline::{
	DefineNote, OrderScore, Pipeline, PipelineInput, PipelineOutput, TrackedObjects, WEIGHT_DECAY,
	run,
};
pub use policy::{
	FEATURES, GovernmentKind, Policy, Situation, active_channels, category_score, channel_of_name,
	encode,
};
pub use residual::{DecisionForward, ResidualCategory};
pub use reward::{Channel, Horizons, Yields, adversarial_reward};
pub use rollout::{Draw, Episode, MergedObjects, Rollout, SampledArmy, draw_for, fit, train};
pub use rules::ScoreRules;
pub use score::{
	PROVINCE_COST_BASE, PROVINCE_COST_MAX, PROVINCE_COST_MIN, ProvinceCost, WarScoreInput,
	WarScoreRules, category_decision, province_cost, side_total_points, war_score,
};
pub use sites::{
	ADD_EFFECTIVE_WAR_SCORE, ASSIGN_LEADER, BATTLE_RESULTS, CALC_MANPOWER_INCREASE, DEFINES_LOAD,
	ENTRY_NAMES, EntryHeap, EntryName, LoadSite, OCCUPATION_WARSCORE, PROVINCE_WARSCORE_COST,
	SET_MANPOWER, SET_TREASURY, SIEGE_COMBAT, SITES, UNIT_LOSSES, WAR_SCORE_SIDE, WAR_SCORE_TOTAL,
	WARGOAL_ENTRY_NAME, WARGOAL_LOAD, entry_offset, read_entry_name,
};
pub use war::{WarObjects, WarSide, country_id, country_index};
