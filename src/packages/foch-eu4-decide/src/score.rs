//! The war score the engine composes, computed offline.
//!
//! `PROVINCE_WARSCORE_COST` builds one province's cost in thousandths of a
//! point. The tooltip keys `WARSCORE_COST_CAPITAL`, `_TRADE_POWER`,
//! `_BUILDINGS`, `_GREAT_PROJECTS`, `_AUTONOMY` and `WARSCORE_NO_FORT` are
//! localization names; the numbers come from the defines
//! `PEACE_COST_DEMAND_CAPITAL_MULT`, `AE_PROVINCE_CAP`,
//! `GREAT_PROJECT_WARSCORE_COST_MODIFIER` and
//! `AUTONOMY_WARSCORE_COST_MODIFIER`, plus the literals 5000 and the two
//! `/5000` scales below.
//!
//! `WAR_SCORE_SIDE` turns occupation into points: a province this side
//! occupies adds its share of the enemy side's total, and an own province the
//! enemy occupies subtracts a quarter of its share of this side's total. A
//! side total below one point makes one share a whole point.
//!
//! `WAR_SCORE_TOTAL` adds the battle term, clamped by
//! `WARSCORE_MAX_FROM_BATTLES`, clamps the running sum inside
//! `MAX_WARSCORE`, adds the occupation shares, and clamps the result inside
//! `MAX_WARSCORE - 1`. Ticking war score and unconditional surrender need war
//! state this crate does not model and stay with the caller's deltas.

use crate::rules::ScoreRules;
use crate::war::{WarObjects, WarSide};

/// The literal base of one province's cost, in thousandths of a point.
pub const PROVINCE_COST_BASE: i32 = 5000;
/// The clamp [`PROVINCE_WARSCORE_COST`] applies after every term.
pub const PROVINCE_COST_MIN: i32 = 1000;
/// The upper end of that clamp. One province can never cost a full war.
pub const PROVINCE_COST_MAX: i32 = 100_000;
/// Trade power and each building add a fifth of their scale to the cost.
const FIFTH_DENOMINATOR: i64 = 5000;
/// An occupied enemy province counts at a quarter weight in reverse.
const REVERSE_QUARTER_DENOMINATOR: i64 = 4000;

/// The defines the war-score functions read. Vanilla values for EU4 1.37.5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarScoreRules {
	/// `PEACE_COST_DEMAND_CAPITAL_MULT`, stored in the binary as thousandths.
	pub capital_mult: f32,
	/// `AE_PROVINCE_CAP`. Development above this does not raise the cost.
	pub dev_cap: i32,
	/// `GREAT_PROJECT_WARSCORE_COST_MODIFIER`, in thousandths when loaded.
	pub great_project: f32,
	/// `AUTONOMY_WARSCORE_COST_MODIFIER`, in thousandths when loaded.
	pub autonomy: f32,
	/// `WARSCORE_MAX_FROM_BATTLES`, in points.
	pub battles_cap: i32,
	/// `MAX_WARSCORE`, in points.
	pub max_war_score: i32,
}

impl WarScoreRules {
	pub const VANILLA: Self = Self {
		capital_mult: 1.2,
		dev_cap: 30,
		great_project: 0.33,
		autonomy: 0.33,
		battles_cap: 40,
		max_war_score: 100,
	};

	/// Read the six defines out of the loaded rules. A missing key keeps the
	/// vanilla value, which is also what an unmodified game runs on.
	pub fn from_rules(rules: &ScoreRules) -> Self {
		let mut out = Self::VANILLA;
		if let Some(value) = rules.define("PEACE_COST_DEMAND_CAPITAL_MULT") {
			out.capital_mult = value;
		}
		if let Some(value) = rules.define("AE_PROVINCE_CAP") {
			out.dev_cap = value as i32;
		}
		if let Some(value) = rules.define("GREAT_PROJECT_WARSCORE_COST_MODIFIER") {
			out.great_project = value;
		}
		if let Some(value) = rules.define("AUTONOMY_WARSCORE_COST_MODIFIER") {
			out.autonomy = value;
		}
		if let Some(value) = rules.define("WARSCORE_MAX_FROM_BATTLES") {
			out.battles_cap = value as i32;
		}
		if let Some(value) = rules.define("MAX_WARSCORE") {
			out.max_war_score = value as i32;
		}
		out
	}
}

/// The inputs of one province's cost. Costs are whole numbers; autonomy is
/// thousandths of a percent, so half autonomy is `50_000`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProvinceCost {
	/// Base tax, production and manpower added together, before the cap.
	pub development: i32,
	/// The owner's mark names this province.
	pub capital: bool,
	/// Flat trade power in the province's home node.
	pub trade_power: i32,
	/// Built buildings the engine counts.
	pub buildings: i32,
	/// Great projects on the province.
	pub great_projects: i32,
	/// Local autonomy in thousandths of a percent.
	pub autonomy: i32,
	/// The fort condition of the `WARSCORE_NO_FORT` term.
	pub no_fort: bool,
}

/// One province's war-score cost in thousandths of a point, in the order the
/// engine applies the terms: base, development, capital on the running total,
/// trade power, buildings and great projects on the running total, then
/// autonomy and the no-fort share off of it.
pub fn province_cost(province: &ProvinceCost, rules: &WarScoreRules) -> i32 {
	let mut cost = PROVINCE_COST_BASE;
	cost += province.development.min(rules.dev_cap) * 1000;
	if province.capital {
		let bonus = (i64::from(cost) * (thousandths(rules.capital_mult) - 1000) / 1000) as i32;
		cost += bonus;
	}
	cost += (i64::from(province.trade_power) * 1000 / FIFTH_DENOMINATOR) as i32;
	let running = i64::from(cost);
	let buildings = i64::from(province.buildings) * 1000 * running / 1000;
	cost += (buildings * 1000 / FIFTH_DENOMINATOR) as i32;
	let running = i64::from(cost);
	let projects = i64::from(province.great_projects) * 1000 * running / 1000;
	cost += (thousandths(rules.great_project) * projects / 1000) as i32;
	let running = i64::from(cost);
	let autonomy = running * i64::from(province.autonomy) / 1000;
	let autonomy = autonomy * 1000 / 100_000;
	cost -= (autonomy * thousandths(rules.autonomy) / 1000) as i32;
	if province.no_fort {
		cost -= (i64::from(cost) * 750 / 100_000) as i32;
	}
	cost.clamp(PROVINCE_COST_MIN, PROVINCE_COST_MAX)
}

fn thousandths(value: f32) -> i64 {
	i64::from((value * 1000.0) as i32)
}

/// The war and the costs of its provinces, aligned with
/// [`WarObjects::province`] indexes.
pub struct WarScoreInput<'a> {
	pub war: &'a WarObjects,
	pub costs: &'a [ProvinceCost],
	/// The battle points booked from `AddEffectiveWarScore` deltas.
	pub battles: i32,
}

/// The total war score from side A's perspective, in points, following
/// `WAR_SCORE_TOTAL`: the battle term under its cap, one clamp at
/// `MAX_WARSCORE`, the occupation shares, and the final clamp one inside it.
pub fn war_score(input: &WarScoreInput<'_>, rules: &WarScoreRules) -> i32 {
	let battles = input.battles.clamp(-rules.battles_cap, rules.battles_cap);
	let partial = battles.clamp(-rules.max_war_score, rules.max_war_score);
	let occupation = occupation_points(input, rules);
	(partial + occupation).clamp(-(rules.max_war_score - 1), rules.max_war_score - 1)
}

/// Side A's occupation points: what A holds of B at full share of B's total,
/// and what B holds of A at full share of A's total against A, while each
/// victim also loses a quarter of its own share of the same province. Each
/// participant's shares truncate to whole points separately, the way one
/// `WAR_SCORE_SIDE` call per country does. Which participant list a province
/// sits on when it changes controller is the one reading this model assumes;
/// the share scales themselves are the loaded division's.
fn occupation_points(input: &WarScoreInput<'_>, rules: &WarScoreRules) -> i32 {
	let a_total = side_total_points(input, WarSide::A, rules);
	let b_total = side_total_points(input, WarSide::B, rules);
	let mut points = 0i64;
	for country in input.war.participants(WarSide::A) {
		let index = crate::war::country_index(*country);
		points += country_shares(input, rules, index, WarSide::B, b_total, a_total) / 1000;
	}
	for country in input.war.participants(WarSide::B) {
		let index = crate::war::country_index(*country);
		points -= country_shares(input, rules, index, WarSide::A, a_total, b_total) / 1000;
	}
	points as i32
}

/// One participant's occupation net in thousandths: provinces of the opposite
/// side this country holds, minus a quarter of the shares of its own
/// provinces the opposite side holds.
fn country_shares(
	input: &WarScoreInput<'_>,
	rules: &WarScoreRules,
	index: u16,
	opposite: WarSide,
	opposite_total: i64,
	own_total: i64,
) -> i64 {
	let mut held = 0i64;
	let mut lost = 0i64;
	for (province, cost) in input.costs.iter().enumerate() {
		if !input.war.occupied(province) {
			continue;
		}
		let owner = input.war.owner(province);
		let controller = input.war.controller(province);
		if controller == index && input.war.on_side(owner, opposite) {
			held += share_thousandths(province_cost(cost, rules), opposite_total);
		}
		if owner == index && input.war.on_side(controller, opposite) {
			lost += share_thousandths(province_cost(cost, rules), own_total) * 1000
				/ REVERSE_QUARTER_DENOMINATOR;
		}
	}
	held - lost
}

/// One occupied province's share of a side's total, in thousandths of a point.
/// The ratio truncates before the hundred scale, as the loaded division does.
fn share_thousandths(cost: i32, side_total_points: i64) -> i64 {
	if side_total_points < 1 {
		return 1000;
	}
	(i64::from(cost) * 1000 / (side_total_points * 1000)) * 100
}

/// One side's total province cost in whole points: every province a
/// participant or its subject owns, each province truncated on its own.
pub fn side_total_points(input: &WarScoreInput<'_>, side: WarSide, rules: &WarScoreRules) -> i64 {
	let mut total = 0i64;
	for (province, cost) in input.costs.iter().enumerate() {
		let owner = input.war.owner(province);
		if input.war.on_side(owner, side) {
			total += i64::from(province_cost(cost, rules)) / 1000;
		}
	}
	total
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::war::WarObjects;
	use foch_eu4_state::candidates::province::OCCUPIED_MASK;

	#[test]
	fn the_engine_order_of_terms_is_preserved() {
		// Hand-computed against the decompiled PROVINCE_WARSCORE_COST:
		// 5000 + 10000 dev = 15000; capital +20% = 18000; trade 10 -> +2 =
		// 18002; one building +3600 = 21602; one great project +7128 = 28730;
		// half autonomy -4740 = 23990.
		let province = ProvinceCost {
			development: 10,
			capital: true,
			trade_power: 10,
			buildings: 1,
			great_projects: 1,
			autonomy: 50_000,
			no_fort: false,
		};
		assert_eq!(
			province_cost(&province, &WarScoreRules::VANILLA),
			23_990,
			"capital multiplies the running total, not the base"
		);
	}

	#[test]
	fn development_above_the_cap_costs_no_more() {
		let capped = ProvinceCost {
			development: 30,
			..ProvinceCost::default()
		};
		let over = ProvinceCost {
			development: 40,
			..ProvinceCost::default()
		};
		assert_eq!(
			province_cost(&capped, &WarScoreRules::VANILLA),
			province_cost(&over, &WarScoreRules::VANILLA)
		);
		assert_eq!(
			province_cost(&capped, &WarScoreRules::VANILLA),
			PROVINCE_COST_BASE + 30_000
		);
	}

	#[test]
	fn a_compound_province_clamps_at_one_hundred_points() {
		let province = ProvinceCost {
			development: 30,
			trade_power: 100,
			buildings: 4,
			great_projects: 2,
			autonomy: 0,
			..ProvinceCost::default()
		};
		assert_eq!(
			province_cost(&province, &WarScoreRules::VANILLA),
			PROVINCE_COST_MAX
		);
	}

	#[test]
	fn a_bigger_capital_multiplier_comes_from_the_rules() {
		let mut rules = WarScoreRules::VANILLA;
		rules.capital_mult = 2.0;
		let province = ProvinceCost {
			development: 10,
			capital: true,
			..ProvinceCost::default()
		};
		assert_eq!(
			province_cost(&province, &rules),
			(5000 + 10_000) + (5000 + 10_000)
		);
		let mut noted = ScoreRules::default();
		noted.note_define("PEACE_COST_DEMAND_CAPITAL_MULT", 2.0);
		assert_eq!(WarScoreRules::from_rules(&noted), rules);
	}

	#[test]
	fn an_occupied_province_scores_its_share_of_the_enemy() {
		// B owns a 10-point and a 30-point province; A occupies the 30-point
		// one: 75 points, plus B's own quarter loss of 18. A's single
		// 20-point province stays unoccupied.
		let mut war = WarObjects::new(1, 2);
		let small = war.add_province(2, 2, 0);
		let big = war.add_province(2, 1, OCCUPIED_MASK);
		let home = war.add_province(1, 1, 0);
		let costs = vec![
			ProvinceCost {
				development: 5,
				..ProvinceCost::default()
			},
			ProvinceCost {
				development: 25,
				..ProvinceCost::default()
			},
			ProvinceCost {
				development: 15,
				..ProvinceCost::default()
			},
		];
		let input = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: 0,
		};
		assert_eq!(
			war_score(&input, &WarScoreRules::VANILLA),
			share_of(30_000, 40) + 18 // A's 75-point share, plus B's quarter loss of 18
		);
		assert!(war.occupied(big));
		assert!(!war.occupied(small));
		assert!(!war.occupied(home));
		assert!(war.on_side(1, WarSide::A));
		assert!(!war.on_side(2, WarSide::A));
	}

	fn share_of(cost: i64, total: i64) -> i32 {
		((cost * 1000 / (total * 1000)) * 100 / 1000) as i32
	}

	#[test]
	fn the_ratio_truncates_before_the_hundred_scale() {
		assert_eq!(share_thousandths(30_000, 40), 75_000);
		// 29.999 of 40 points is 74.999..%, but the ratio step keeps 749.
		assert_eq!(share_thousandths(29_999, 40), 74_900);
	}

	#[test]
	fn the_enemy_occupied_share_counts_at_a_quarter() {
		// A owns two 10-point provinces; B occupies one of them and nothing
		// else. B gains the full 50-point share of A's total, and A loses a
		// quarter of it: -50 - 12.
		let mut war = WarObjects::new(1, 2);
		war.add_province(1, 2, OCCUPIED_MASK);
		war.add_province(1, 1, 0);
		war.add_province(2, 2, 0);
		let costs = vec![
			ProvinceCost {
				development: 10,
				..ProvinceCost::default()
			},
			ProvinceCost {
				development: 10,
				..ProvinceCost::default()
			},
			ProvinceCost {
				development: 30,
				..ProvinceCost::default()
			},
		];
		let input = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: 0,
		};
		assert_eq!(
			share_thousandths(10_000, 20),
			50_000,
			"the occupied province is half of A's total"
		);
		assert_eq!(war_score(&input, &WarScoreRules::VANILLA), -62);
	}

	#[test]
	fn battles_clamp_at_forty_and_the_total_at_ninety_nine() {
		let mut war = WarObjects::new(1, 2);
		war.add_province(1, 1, 0);
		war.add_province(2, 2, 0);
		let costs = vec![ProvinceCost::default(), ProvinceCost::default()];
		let input = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: 500,
		};
		assert_eq!(war_score(&input, &WarScoreRules::VANILLA), 40);
		let losing = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: -500,
		};
		assert_eq!(war_score(&losing, &WarScoreRules::VANILLA), -40);
		let overrun = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: 40 + 100,
		};
		// 140 clamps to 40 at the battle cap, not to 99.
		assert_eq!(war_score(&overrun, &WarScoreRules::VANILLA), 40);
	}

	#[test]
	fn a_side_without_provinces_grants_one_point_per_share() {
		// B has no provinces; occupying nothing still books the default share.
		let war = WarObjects::new(1, 2);
		let costs: [ProvinceCost; 0] = [];
		let input = WarScoreInput {
			war: &war,
			costs: &costs,
			battles: 0,
		};
		assert_eq!(
			share_thousandths(10_000, 0),
			1000,
			"a total below one point is one whole point"
		);
		assert_eq!(
			side_total_points(&input, WarSide::B, &WarScoreRules::VANILLA),
			0
		);
		assert_eq!(war_score(&input, &WarScoreRules::VANILLA), 0);
	}
}
