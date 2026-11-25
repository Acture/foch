//! A generalized pocket war: random terrain, daily time, province choice.
//!
//! The battlefield is a random connected province graph with terrain drawn
//! from the real categories - each carries the vanilla movement cost and
//! the attacker dice penalty out of `map/terrain.txt`. Time runs one day
//! per tick, and a decision point only comes when the standing command
//! completes: a march arrives, a battle resolves, a siege phase ends, or a
//! hold expires. The action at each decision point is a province: the
//! general picks any province on the map and the army marches there along
//! the cheapest path. Arriving at the war goal starts the siege; meeting
//! the enemy army starts a battle; neither is a separate order.
//!
//! Battles roll a `COMBAT_DICE_SIDE`-sided die per side per three-day fire
//! or shock phase, add the general's pips and the terrain's attacker
//! penalty, drain morale daily, and end in a shattered retreat below
//! `LOW_MORALE_THRESHOLD`. A siege accumulates progress in thirty-day
//! phases toward `SIEGE_WIN`. The war-score scales - battle value, ticking,
//! goal bonus, caps - come from the episode's own define draw, so a mod's
//! numbers move both the world and the features the policy sees.

use crate::score::{ProvinceCost, WarScoreInput, WarScoreRules, war_score};
use crate::war::WarObjects;

/// Days each fire or shock phase lasts (`DAYS_PER_PHASE`).
pub const DAYS_PER_PHASE: u32 = 3;
/// Days each siege phase lasts (`DAYS_PER_SIEGE_PHASE`).
pub const DAYS_PER_SIEGE_PHASE: u32 = 30;
/// Sides on the combat die (`COMBAT_DICE_SIDE`).
pub const COMBAT_DICE_SIDE: u32 = 10;
/// Sides on the siege die.
pub const SIEGE_DICE_SIDE: u32 = 14;
/// A battle ends in a shattered retreat below this morale fraction
/// (`LOW_MORALE_THRESHOLD`).
pub const LOW_MORALE_THRESHOLD: f32 = 0.5;
/// Siege progress that captures the province (`SIEGE_WIN`).
pub const SIEGE_WIN: i32 = 20;
/// Baseline days to cross one province before terrain.
pub const DAYS_PER_PROVINCE: u32 = 12;
/// How many provinces one decision point may choose between.
pub const CANDIDATE_LIMIT: usize = 8;

/// One terrain category: vanilla movement cost and the die pips an
/// attacker loses fighting on it, out of `map/terrain.txt`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Terrain {
	pub name: &'static str,
	pub movement_cost: f32,
	pub attacker_pips: i32,
	/// Monthly manpower attrition a standing or marching army takes.
	pub attrition: f32,
}

pub const PLAINS: Terrain = Terrain {
	name: "plains",
	movement_cost: 1.0,
	attacker_pips: 0,
	attrition: 0.01,
};
pub const FOREST: Terrain = Terrain {
	name: "forest",
	movement_cost: 1.25,
	attacker_pips: -1,
	attrition: 0.02,
};
pub const HILLS: Terrain = Terrain {
	name: "hills",
	movement_cost: 1.5,
	attacker_pips: -1,
	attrition: 0.02,
};
pub const MOUNTAINS: Terrain = Terrain {
	name: "mountains",
	movement_cost: 2.0,
	attacker_pips: -2,
	attrition: 0.03,
};
pub const MARSH: Terrain = Terrain {
	name: "marsh",
	movement_cost: 1.5,
	attacker_pips: -1,
	attrition: 0.03,
};

const TERRAINS: [Terrain; 5] = [PLAINS, FOREST, HILLS, MOUNTAINS, MARSH];

/// One province on the generated map. The grid coordinates place it on
/// the raw board tensor; nothing strategic is precomputed.
#[derive(Clone, Copy, Debug)]
pub struct Province {
	pub terrain: Terrain,
	/// Enemy development, feeding the war-score cost model.
	pub development: i32,
	pub capital: bool,
	pub x: u32,
	pub y: u32,
}

/// The board fits the provinces plus a one-cell border on every side.
pub const BOARD_WIDTH: usize = 6;
pub const BOARD_HEIGHT: usize = 6;
/// Raw channels per board cell: terrain one-hot, development, capital,
/// fort, our regiments, enemy regiments, goal flag, our flag.
pub const BOARD_CHANNELS: usize = 10;

/// The generated battlefield: provinces and their day-weighted adjacency.
#[derive(Clone, Debug)]
pub struct Map {
	pub provinces: Vec<Province>,
	/// `edges[a]` lists `(b, days)` neighbors; symmetric.
	pub edges: Vec<Vec<(u32, u32)>>,
}

impl Map {
	/// A random connected map: a ring backbone plus chords, terrain spread
	/// over the real categories.
	pub fn generate(seed: u64) -> Self {
		let mut rng = Rng::seed(seed);
		let count = 9 + rng.below(8) as usize;
		let mut provinces = Vec::with_capacity(count);
		for index in 0..count {
			// Raw grid coordinates, distinct cells, no layout meaning.
			let cell = index as u64;
			provinces.push(Province {
				terrain: TERRAINS[rng.below(TERRAINS.len() as u64) as usize],
				development: 5 + rng.below(30) as i32,
				capital: index == 0,
				x: (cell % 4) as u32,
				y: (cell / 4) as u32,
			});
		}
		let mut edges = vec![Vec::new(); count];
		fn link(edges: &mut [Vec<(u32, u32)>], provinces: &[Province], a: usize, b: usize) {
			if a == b || edges[a].iter().any(|(to, _)| *to as usize == b) {
				return;
			}
			let days = march_days(provinces[a].terrain).max(march_days(provinces[b].terrain));
			edges[a].push((b as u32, days));
			edges[b].push((a as u32, days));
		}
		for index in 0..count {
			link(&mut edges, &provinces, index, (index + 1) % count);
		}
		for _ in 0..count {
			let a = rng.below(count as u64) as usize;
			let b = rng.below(count as u64) as usize;
			link(&mut edges, &provinces, a, b);
		}
		Self { provinces, edges }
	}

	/// Dijkstra days between two provinces.
	pub fn travel_days(&self, from: u32, to: u32) -> u32 {
		let mut best = vec![u32::MAX; self.provinces.len()];
		best[from as usize] = 0;
		let mut frontier = vec![from];
		while let Some(current) = frontier.pop() {
			let days = best[current as usize];
			for (next, cost) in &self.edges[current as usize] {
				let total = days.saturating_add(*cost);
				if total < best[*next as usize] {
					best[*next as usize] = total;
					frontier.push(*next);
				}
			}
		}
		best[to as usize]
	}

	/// The full cheapest path, including the destination, excluding `from`.
	pub fn path(&self, from: u32, to: u32) -> Vec<u32> {
		if from == to {
			return Vec::new();
		}
		let mut best = vec![u32::MAX; self.provinces.len()];
		let mut came = vec![None; self.provinces.len()];
		best[from as usize] = 0;
		let mut frontier = vec![from];
		while let Some(current) = frontier.pop() {
			let days = best[current as usize];
			for (next, cost) in &self.edges[current as usize] {
				let total = days.saturating_add(*cost);
				if total < best[*next as usize] {
					best[*next as usize] = total;
					came[*next as usize] = Some(current);
					frontier.push(*next);
				}
			}
		}
		let mut path = Vec::new();
		let mut cursor = to;
		while let Some(previous) = came[cursor as usize] {
			path.push(cursor);
			cursor = previous;
		}
		path.reverse();
		path
	}
}

pub fn march_days(terrain: Terrain) -> u32 {
	(DAYS_PER_PROVINCE as f32 * terrain.movement_cost).ceil() as u32
}

/// One side's modifier draw: morale and general-pip modifiers, the kind
/// of thing ideas, advisors, and policies stack in the real game. Mostly
/// vanilla; a varied draw appears one time in four, and the numbers are
/// raw policy inputs and battle inputs at once, so a modded or doctrinal
/// game is read, not memorized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Modifiers {
	pub morale: f32,
	pub fire_pips: i32,
	pub shock_pips: i32,
	pub maneuver_pips: i32,
	pub siege_pips: i32,
}

impl Modifiers {
	pub const VANILLA: Self = Self {
		morale: 0.0,
		fire_pips: 0,
		shock_pips: 0,
		maneuver_pips: 0,
		siege_pips: 0,
	};

	pub fn draw(seed: u64) -> Self {
		let mut rng = Rng::seed(seed);
		if rng.below(4) != 0 {
			return Self::VANILLA;
		}
		Self {
			morale: (rng.below(51) as f32) / 100.0 - 0.25,
			fire_pips: rng.below(3) as i32 - 1,
			shock_pips: rng.below(3) as i32 - 1,
			maneuver_pips: rng.below(3) as i32 - 1,
			siege_pips: rng.below(3) as i32 - 1,
		}
	}
}

/// The episode's define draw. A mod's changed numbers move the world and
/// the features together; the trainer randomizes them so the policy reads
/// them instead of memorizing vanilla.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DefineDraw {
	pub battles_cap: f32,
	pub wargoal_bonus: f32,
	pub ticking_per_month: f32,
	pub superiority_threshold: f32,
	pub capital_mult: f32,
	pub autonomy: f32,
	/// Monthly morale recovery (`MORALE_RECOVERY_SPEED`).
	pub morale_recovery: f32,
	/// Extra recovery in controlled territory
	/// (`MORALE_RECOVERY_SPEED_OWN_TERRITORY`).
	pub morale_recovery_own: f32,
	/// Regiment strength reinforced each month (`MONTHLY_REINFORCE`).
	pub monthly_reinforce: f32,
	/// Front-row deployment slots each side fields (`BASE_COMBAT_WIDTH`).
	pub combat_width: f32,
	/// Slots a cavalry regiment strikes past an empty facing one: the
	/// cavalry `maneuver` stat in `common/units`, where defines.lua
	/// keeps no `FLANK_RANGE` entry.
	pub flank_range: f32,
}

impl DefineDraw {
	pub const VANILLA: Self = Self {
		battles_cap: 40.0,
		wargoal_bonus: 5.0,
		ticking_per_month: 0.4,
		superiority_threshold: 10.0,
		capital_mult: 1.2,
		autonomy: 0.33,
		morale_recovery: 0.15,
		morale_recovery_own: 0.05,
		monthly_reinforce: 0.1,
		combat_width: 15.0,
		flank_range: 2.0,
	};

	/// Most games run vanilla; one scenario in four draws a modded set with
	/// each number scaled independently between half and double. Only a
	/// changed game needs the policy to read the numbers at all.
	pub fn draw(seed: u64) -> Self {
		let mut rng = Rng::seed(seed);
		if rng.below(4) != 0 {
			return Self::VANILLA;
		}
		let scale = |rng: &mut Rng, value: f32| value * (0.5 + rng.below(151) as f32 / 100.0);
		Self {
			battles_cap: scale(&mut rng, Self::VANILLA.battles_cap),
			wargoal_bonus: scale(&mut rng, Self::VANILLA.wargoal_bonus),
			ticking_per_month: scale(&mut rng, Self::VANILLA.ticking_per_month),
			superiority_threshold: scale(&mut rng, Self::VANILLA.superiority_threshold),
			capital_mult: scale(&mut rng, Self::VANILLA.capital_mult),
			autonomy: scale(&mut rng, Self::VANILLA.autonomy),
			morale_recovery: scale(&mut rng, Self::VANILLA.morale_recovery),
			morale_recovery_own: scale(&mut rng, Self::VANILLA.morale_recovery_own),
			monthly_reinforce: scale(&mut rng, Self::VANILLA.monthly_reinforce),
			combat_width: scale(&mut rng, Self::VANILLA.combat_width),
			flank_range: scale(&mut rng, Self::VANILLA.flank_range),
		}
	}
}

/// One side's battle line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Army {
	pub infantry: f32,
	pub cavalry: f32,
	pub artillery: f32,
	pub morale: f32,
}

impl Army {
	/// Regiments still standing.
	pub fn regiments(&self) -> f32 {
		self.infantry + self.cavalry + self.artillery
	}

	fn strength(&self) -> f32 {
		self.infantry + self.cavalry * 1.5 + self.artillery * 2.0
	}
}

/// The general's pips on one phase roll.
#[derive(Clone, Copy, Debug)]
pub struct Pips {
	pub fire: i32,
	pub shock: i32,
}

impl Pips {
	pub fn of_personality(name: &str) -> Self {
		match name {
			"glory_seeker_personality" | "bold_fighter_personality" => Pips { fire: 1, shock: 3 },
			"careful_personality" => Pips { fire: 2, shock: 0 },
			_ => Pips { fire: 1, shock: 1 },
		}
	}
}

/// Resolve one whole battle, day by day, in three-day phases. Returns the
/// war-score points it pays (negated for a defeat), enemy regiments
/// destroyed, and our losses. The battle-score scale - `0.1 + destroyed /
/// 20` - is the decompiled engine formula; the daily casualty split is a
/// documented stand-in for the un-decompiled damage tables.
#[allow(clippy::too_many_arguments)]
pub fn resolve_battle(
	rng: &mut Rng,
	ours: Army,
	enemy: Army,
	our_pips: Pips,
	their_pips: Pips,
	terrain: Terrain,
	we_attacked: bool,
	morale_scales: (f32, f32),
) -> (f32, f32, f32) {
	let (points, destroyed, losses, _, _) = resolve_battle_full(
		rng,
		ours,
		enemy,
		our_pips,
		their_pips,
		terrain,
		we_attacked,
		morale_scales,
	);
	(points, destroyed, losses)
}

/// The same battle, also reporting the morale both armies end the
/// engagement with — the campaign persists it, so a beaten army stays
/// brittle until it recovers on friendly ground.
#[allow(clippy::too_many_arguments)]
pub fn resolve_battle_full(
	rng: &mut Rng,
	mut ours: Army,
	mut enemy: Army,
	our_pips: Pips,
	their_pips: Pips,
	terrain: Terrain,
	we_attacked: bool,
	morale_scales: (f32, f32),
) -> (f32, f32, f32, f32, f32) {
	let our_start = ours.regiments();
	let their_start = enemy.regiments();
	// Per-side morale-damage scales from the modifier draws.
	let (our_morale_scale, their_morale_scale) = morale_scales;
	let mut day = 0;
	let mut ours_won;
	loop {
		let fire = (day / DAYS_PER_PHASE).is_multiple_of(2);
		let our_die = rng.below(u64::from(COMBAT_DICE_SIDE)) as i32;
		let their_die = rng.below(u64::from(COMBAT_DICE_SIDE)) as i32;
		// The side that marched into the province is the attacker and eats
		// the ground's penalty; standing on hills and being attacked is the
		// point of choosing a battlefield.
		let (our_bonus, their_bonus) = if we_attacked {
			(terrain.attacker_pips, 0)
		} else {
			(0, terrain.attacker_pips)
		};
		let (our_total, their_total) = if fire {
			(
				our_die + our_pips.fire + our_bonus,
				their_die + their_pips.fire + their_bonus,
			)
		} else {
			(
				our_die + our_pips.shock + our_bonus,
				their_die + their_pips.shock + their_bonus,
			)
		};
		let edge = (our_total - their_total) as f32;
		// The game's tactics stat multiplies casualties: higher tactics
		// means you deal more and take less.
		let ratio = ours.strength() / enemy.strength().max(0.1);
		enemy.infantry -= (0.06 + 0.012 * edge.max(0.0)) * (1.0 + 0.1 * ratio);
		ours.infantry -= (0.06 + 0.012 * (-edge).max(0.0)) * (1.0 + 0.1 / ratio.max(0.1));
		ours.morale -= (0.01 + 0.002 * (-edge).max(0.0)) * DAYS_PER_PHASE as f32 * our_morale_scale;
		enemy.morale -= (0.01 + 0.002 * edge.max(0.0)) * DAYS_PER_PHASE as f32 * their_morale_scale;
		ours.infantry = ours.infantry.max(0.0);
		enemy.infantry = enemy.infantry.max(0.0);
		day += DAYS_PER_PHASE;
		ours_won = enemy.morale <= LOW_MORALE_THRESHOLD || enemy.regiments() < 1.0;
		if ours_won || ours.morale <= LOW_MORALE_THRESHOLD || ours.regiments() < 1.0 || day > 60 {
			break;
		}
	}
	let destroyed = (their_start - enemy.regiments()).max(0.0);
	let losses = (our_start - ours.regiments()).max(0.0);
	let points = if ours_won {
		0.1 + destroyed / 20.0
	} else {
		-(0.1 + losses / 20.0)
	};
	(
		points,
		destroyed,
		losses,
		ours.morale.max(0.0),
		enemy.morale.max(0.0),
	)
}

/// One thirty-day siege phase: a roll of the `SIEGE_DICE_SIDE` die plus
/// the general's siege pips against the fort. Progress reaching
/// `SIEGE_WIN` captures the province.
pub fn siege_phase(rng: &mut Rng, siege_pips: i32, fort_level: i32) -> i32 {
	let roll = 1 + rng.below(u64::from(SIEGE_DICE_SIDE)) as i32;
	(roll + siege_pips - fort_level * 2 - 4).max(0)
}

/// Cumulative military stats per tech level, parsed from the game's
/// `technologies/mil.txt`. These are the game's own computed values:
/// tactics (casualty multiplier), max morale, and combat width.
pub const MIL_TACTICS: [f32; 32] = [
	0.50, 0.50, 0.50, 0.50, 0.75, 0.75, 0.75, 0.75, 0.75, 1.00, 1.50, 1.50, 1.50, 1.50, 1.75, 1.75,
	1.75, 2.00, 2.00, 2.00, 2.25, 2.25, 2.25, 2.50, 2.50, 3.00, 3.00, 3.00, 3.00, 3.00, 3.00, 3.25,
];
pub const MIL_MORALE: [f32; 32] = [
	2.00, 2.00, 2.00, 4.00, 4.00, 4.00, 5.00, 5.00, 5.00, 5.00, 5.00, 5.00, 5.00, 5.00, 5.00, 5.00,
	5.00, 5.00, 5.00, 5.00, 6.00, 6.00, 6.00, 6.00, 6.00, 6.00, 6.00, 6.00, 6.00, 7.00, 8.00, 8.00,
];
pub const MIL_WIDTH: [f32; 32] = [
	15.0, 15.0, 15.0, 20.0, 20.0, 20.0, 22.0, 22.0, 22.0, 25.0, 25.0, 25.0, 25.0, 27.0, 29.0, 29.0,
	29.0, 29.0, 29.0, 30.0, 32.0, 32.0, 32.0, 32.0, 32.0, 38.0, 38.0, 38.0, 38.0, 40.0, 40.0, 40.0,
];

/// The military tech that unlocks artillery.
pub const ARTILLERY_TECH: u32 = 7;

/// Per-province features the scoring policy sees: terrain one-hot,
/// movement cost, reach from us in days, the goal flag, the enemy flag,
/// enemy proximity, development, the capital flag, and the candidate's own
/// distance to the war goal - without that last one no walk can head for
/// the goal until it is two hops away.
pub const PROVINCE_FEATURES: usize = 12;

/// Broadcast features every candidate shares: our shape, the enemy, the
/// clock, both sides' military tech, and the drawn defines. Mod numbers
/// live here, so a changed game changes the input.
pub const GLOBAL_FEATURES: usize = 16;

/// Which side a scenario favors, for the advantage curriculum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
	Ours,
	Even,
	Enemy,
}

/// The whole episode state machine. One `step_day` per day; a decision is
/// owed exactly when `decision_pending` turns true.
#[derive(Clone)]
pub struct Campaign {
	pub map: Map,
	pub defines: DefineDraw,
	pub goal: u32,
	pub ours: Army,
	pub enemy: Army,
	pub our_position: u32,
	pub enemy_position: u32,
	pub enemy_strength: u32,
	pub our_tech: u32,
	pub enemy_tech: u32,
	pub our_modifiers: Modifiers,
	pub enemy_modifiers: Modifiers,
	/// The enemy army's own march: path and days left on the hop.
	enemy_path: Vec<u32>,
	enemy_hop_days: u32,
	/// Whether our last arrival in the contact province was our own march
	/// in (we attack) or a hold (the enemy attacked us on our ground).
	we_marched_in: bool,
	/// When set, the built-in enemy general stands down and an outside
	/// policy drives the enemy through [`Campaign::enemy_order`].
	pub enemy_manual: bool,
	/// Base vision: every owned or captured province and the army's own
	/// position see one hop. Not a draw; a consequence of territory.
	our_pips: Pips,
	their_pips: Pips,
	siege_pips: i32,
	fort_level: i32,
	pub wargoal: String,
	pub personality: String,
	/// Enemy provinces we occupy, paid through the score model.
	pockets: Vec<u32>,
	siege_progress: i32,
	pub day: u32,
	pub horizon: u32,
	/// The provinces still to walk, in order, and the days left on the
	/// current hop.
	path: Vec<u32>,
	hop_days_left: u32,
	/// Days left on a hold or a siege phase.
	wait_days: u32,
	pub score_points: f32,
	/// Reward breakdown for diagnosis: how much came from each source.
	pub exploration_points: f32,
	pub occupation_points: f32,
	pub ticking_points: f32,
	pub battle_count: u32,
	pub battles_won: u32,
	pub sieges_won: u32,
	/// Provinces our army has stood on; first visits earn exploration.
	visited: Vec<u32>,
	pub manpower_lost: f32,
	pub decision_pending: bool,
	/// Second army: position and composition. When present, decision
	/// points alternate between the primary and the second army.
	pub second_army: Option<(u32, Army)>,
	/// When true, the second army is an AI ally the policy cannot order.
	pub second_army_ai: bool,
	/// Which army owes the decision: 0 = primary, 1 = second.
	pub deciding: usize,
	battle_points: f32,
}

impl Campaign {
	/// One scenario class in the advantage curriculum: a manpower-and-pips
	/// edge for one side, or an even draw. The board and rules still carry
	/// only raw numbers, so the policy must read the advantage, not a tag.
	pub fn generate_with_edge(seed: u64, personality: &str, wargoal: &str, edge: Edge) -> Self {
		let mut campaign = Self::generate(seed, personality, wargoal);
		// Defend scenarios: we already hold the goal; the enemy tries to take it.
		if wargoal.starts_with("defend") {
			campaign.pockets.push(campaign.goal);
			campaign.enemy_position = campaign.goal; // enemy starts on our goal
		}
		match edge {
			Edge::Even => {}
			Edge::Ours => {
				campaign.ours.infantry += 4.0;
				campaign.our_modifiers.fire_pips += 1;
				campaign.our_modifiers.shock_pips += 1;
				campaign.our_modifiers.morale += 0.15;
			}
			Edge::Enemy => {
				campaign.enemy.infantry += 4.0;
				campaign.enemy_modifiers.fire_pips += 1;
				campaign.enemy_modifiers.shock_pips += 1;
				campaign.enemy_modifiers.morale += 0.15;
			}
		}
		campaign
	}

	/// One scenario: a random map, a define draw, armies, and positions.
	pub fn generate(seed: u64, personality: &str, wargoal: &str) -> Self {
		let mut rng = Rng::seed(seed);
		let map = Map::generate(seed ^ 0x51ed_270b);
		// The enemy army defends the war goal; taking it means beating the
		// garrison army first.
		let enemy_position = 0;
		let our_position = 1 + rng.below(map.provinces.len() as u64 - 1) as u32;
		let enemy_strength = 8 + rng.below(18) as u32;
		let our_tech = 3 + rng.below(30) as u32;
		let enemy_tech = 3 + rng.below(30) as u32;
		Self {
			map,
			defines: DefineDraw::draw(seed ^ 0x2545_f491),
			goal: 0,
			ours: Army {
				infantry: 10.0,
				cavalry: 2.0,
				artillery: f32::from(our_tech >= ARTILLERY_TECH) * 2.0,
				morale: 1.0,
			},
			enemy: Army {
				infantry: (enemy_strength - 2) as f32,
				cavalry: 2.0,
				artillery: f32::from(enemy_tech >= ARTILLERY_TECH) * 2.0,
				morale: 1.0,
			},
			our_position,
			enemy_position,
			enemy_strength,
			our_tech,
			enemy_tech,
			our_modifiers: Modifiers::draw(seed ^ 0x7a11_ce5e),
			enemy_modifiers: Modifiers::draw(seed ^ 0x1b57_f3a7),
			enemy_path: Vec::new(),
			enemy_hop_days: 0,
			we_marched_in: false,
			enemy_manual: false,

			our_pips: Pips::of_personality(personality),
			their_pips: Pips { fire: 1, shock: 1 },
			siege_pips: if personality == "careful_personality" {
				2
			} else {
				1
			},
			fort_level: 1,
			wargoal: wargoal.to_string(),
			personality: personality.to_string(),
			pockets: Vec::new(),
			siege_progress: 0,
			day: 0,
			horizon: 360,
			path: Vec::new(),
			hop_days_left: 0,
			wait_days: 0,
			score_points: 0.0,
			second_army: None,
			second_army_ai: false,
			deciding: 0,
			exploration_points: 0.0,
			occupation_points: 0.0,
			ticking_points: 0.0,
			battle_count: 0,
			battles_won: 0,
			sieges_won: 0,
			visited: vec![our_position],
			manpower_lost: 0.0,
			decision_pending: true,
			battle_points: 0.0,
		}
	}

	/// Issue the next command: march at a chosen province. Picking the
	/// current province holds for one siege phase worth of days; picking
	/// the goal starts or resumes its siege once the army is there.
	pub fn order(&mut self, destination: u32) {
		self.decision_pending = false;
		if destination == self.our_position {
			// Standing in place for one phase; the wait branch below turns
			// it into a siege when this is the unheld goal.
			self.wait_days = DAYS_PER_SIEGE_PHASE;
			self.path.clear();
			self.hop_days_left = 0;
			return;
		}
		self.path = self.map.path(self.our_position, destination);
		if let Some(&first) = self.path.first() {
			self.hop_days_left = self.edge_days(self.our_position, first);
		}
	}

	/// The provinces the general may pick this decision point: the current
	/// province, the war goal, and the enemy's province always lead - a
	/// general can always order a march on either anchor - then the nearest
	/// neighbors up to the cap.
	pub fn candidates(&self) -> Vec<u32> {
		let mut picked = vec![self.deciding_position()];
		for anchor in [self.goal, self.enemy_position] {
			if !picked.contains(&anchor) {
				picked.push(anchor);
			}
		}
		let mut reachable: Vec<(u32, u32)> = (0..self.map.provinces.len() as u32)
			.map(|index| (self.map.travel_days(self.our_position, index), index))
			.filter(|(days, index)| *days <= 2 * DAYS_PER_PROVINCE && !picked.contains(index))
			.collect();
		reachable.sort();
		picked.extend(
			reachable
				.into_iter()
				.take(CANDIDATE_LIMIT - picked.len())
				.map(|(_, index)| index),
		);
		picked
	}

	/// One day. Contact brings a battle wherever it happens; a completed
	/// hop advances the march; a finished wait owes a decision unless the
	/// army is besieging the goal, which runs phase after phase.
	pub fn step_day(&mut self, rng: &mut Rng) {
		if self.decision_pending || self.finished() {
			return;
		}
		self.day += 1;
		if !self.enemy_manual {
			self.enemy_ai();
			self.enemy_step();
		}
		if self.our_position == self.enemy_position && self.enemy.regiments() >= 1.0 {
			self.fight(rng);
		}
		// Monthly attrition from the ground we stand on, and the siege's
		// own bleed, at the defines' `SIEGE_ATTRITION` scale.
		if self.day.is_multiple_of(DAYS_PER_SIEGE_PHASE) {
			let terrain = self.map.provinces[self.our_position as usize].terrain;
			let besieging = self.our_position == self.goal
				&& !self.pockets.contains(&self.goal)
				&& self.wait_days > 0;
			let rate = terrain.attrition + f32::from(besieging) * 0.01;
			// Supply: a province supports development-scaled regiments;
			// the excess starves monthly. Big army on poor ground bleeds.
			let supply = 4.0 + self.map.provinces[self.our_position as usize].development as f32;
			let excess = (self.ours.regiments() - supply).max(0.0);
			let supply_rate = excess / self.ours.regiments().max(1.0) * 0.05;
			let losses = self.ours.regiments() * (rate + supply_rate);
			self.ours.infantry = (self.ours.infantry - losses).max(0.0);
			self.manpower_lost += losses * 1000.0;
		}
		if self.hop_days_left > 0 {
			self.hop_days_left -= 1;
			if self.hop_days_left == 0
				&& let Some(next) = self.path.first().copied()
			{
				self.path.remove(0);
				self.our_position = next;
				self.we_marched_in = true;
				if !self.visited.contains(&next) {
					self.visited.push(next);
					// First visit pays like a month of ticking: modest, but it
					// rewards moving over standing still.
					self.score_points += self.defines.ticking_per_month / 5.0;
					self.exploration_points += self.defines.ticking_per_month / 5.0;
				}
				if self.path.is_empty() && next == self.goal {
					// Arriving at an unheld goal starts the siege.
					self.wait_days = DAYS_PER_SIEGE_PHASE;
				} else if self.path.is_empty() {
					self.decision_pending = true;
				} else {
					self.hop_days_left = self.edge_days(self.our_position, self.path[0]);
				}
			}
		} else if self.wait_days > 0 {
			self.wait_days -= 1;
			if self.wait_days == 0 {
				let besieging = self.our_position == self.goal
					&& !self.pockets.contains(&self.goal)
					&& (self.enemy_position != self.goal || self.enemy.regiments() < 1.0);
				if besieging {
					self.siege_progress += siege_phase(rng, self.siege_pips, self.fort_level);
					if self.siege_progress >= SIEGE_WIN {
						self.occupy_goal();
						self.siege_progress = 0;
						self.decision_pending = true;
					} else {
						self.wait_days = DAYS_PER_SIEGE_PHASE;
					}
				} else {
					self.decision_pending = true;
				}
			}
		}
		// Ticking score every thirty days while the goal is held.
		if self.day.is_multiple_of(DAYS_PER_SIEGE_PHASE) && self.pockets.contains(&self.goal) {
			let superior = self.wargoal == "superiority"
				&& self.battle_points >= self.defines.superiority_threshold;
			if self.wargoal != "superiority" || superior {
				self.score_points += self.defines.ticking_per_month;
			}
		}
	}

	fn edge_days(&self, from: u32, to: u32) -> u32 {
		self.map.edges[from as usize]
			.iter()
			.find(|(next, _)| *next == to)
			.map(|(_, days)| *days)
			.unwrap_or(DAYS_PER_PROVINCE)
	}

	/// The pips our general brings to a battle, tech included: early tech
	/// fights on shock, late tech on fire. The exact per-tech unit table is
	/// a moddable file; this scaling is a documented stand-in.
	fn tech_pips(&self, ours: bool) -> Pips {
		let tech = if ours { self.our_tech } else { self.enemy_tech };
		let base = if ours { self.our_pips } else { self.their_pips };
		let modifiers = if ours {
			self.our_modifiers
		} else {
			self.enemy_modifiers
		};
		Pips {
			fire: (base.fire + (tech / 10) as i32 + modifiers.fire_pips).max(0),
			shock: (base.shock + ((32 - tech) / 16) as i32 + modifiers.shock_pips).max(0),
		}
	}

	/// The morale-damage scale a side takes, from its modifier draw.
	fn morale_scale(&self, ours: bool) -> f32 {
		let modifiers = if ours {
			self.our_modifiers
		} else {
			self.enemy_modifiers
		};
		(1.0 - modifiers.morale).clamp(0.5, 1.5)
	}

	/// The enemy general, in the vanilla spirit: relieve a siege of the
	/// goal first, seek battle when clearly stronger, fall back when
	/// weaker, hold otherwise.
	fn enemy_ai(&mut self) {
		if self.enemy.regiments() < 1.0 {
			return;
		}
		let relieving = self.our_position == self.goal
			&& !self.pockets.contains(&self.goal)
			&& self.wait_days > 0;
		let stronger = self.enemy.strength() > self.ours.strength() * 1.1;
		let weaker = self.enemy.strength() < self.ours.strength() * 0.9;
		let near_us =
			self.map.travel_days(self.enemy_position, self.our_position) <= 2 * DAYS_PER_PROVINCE;
		let target = if relieving {
			Some(self.goal)
		} else if stronger && self.our_position != self.enemy_position {
			Some(self.our_position)
		} else if weaker && near_us {
			self.map.edges[self.enemy_position as usize]
				.iter()
				.map(|(next, _)| *next)
				.max_by_key(|next| self.map.travel_days(*next, self.our_position))
		} else {
			None
		};
		if let Some(target) = target
			&& self.enemy_path.is_empty()
			&& target != self.enemy_position
		{
			self.enemy_path = self.map.path(self.enemy_position, target);
			if let Some(&first) = self.enemy_path.first() {
				self.enemy_hop_days = self.map.edges[self.enemy_position as usize]
					.iter()
					.find(|(next, _)| *next == first)
					.map(|(_, days)| *days)
					.unwrap_or(DAYS_PER_PROVINCE);
			}
		}
	}

	/// One day of the enemy's march. A beaten enemy re-plans next tick.
	fn enemy_step(&mut self) {
		if self.enemy_hop_days > 0 {
			self.enemy_hop_days -= 1;
			if self.enemy_hop_days == 0
				&& let Some(next) = self.enemy_path.first().copied()
			{
				self.enemy_path.remove(0);
				self.enemy_position = next;
				self.we_marched_in = false;
			}
		}
	}

	fn fight(&mut self, rng: &mut Rng) {
		let terrain = self.map.provinces[self.our_position as usize].terrain;
		let (points, destroyed, losses) = resolve_battle(
			rng,
			self.ours,
			self.enemy,
			self.tech_pips(true),
			self.tech_pips(false),
			terrain,
			self.we_marched_in,
			(self.morale_scale(true), self.morale_scale(false)),
		);
		self.battle_points += points;
		self.battle_count += 1;
		if points > 0.0 {
			self.battles_won += 1;
		}
		self.manpower_lost += losses * 1000.0;
		self.enemy.infantry = (self.enemy.infantry - destroyed).max(0.0);
		self.ours.infantry = (self.ours.infantry - losses).max(0.0);
		self.ours.morale = (self.ours.morale - 0.3).max(0.0);
		if points > 0.0 {
			// The beaten enemy falls back one province and re-plans.
			let away = self.map.edges[self.enemy_position as usize]
				.first()
				.map(|(next, _)| *next)
				.unwrap_or(self.enemy_position);
			self.enemy_position = away;
			self.enemy.morale = 1.0;
			self.enemy_path.clear();
			self.enemy_hop_days = 0;
		} else {
			// Our shattered army falls back and the march aborts.
			let away = self.map.edges[self.our_position as usize]
				.first()
				.map(|(next, _)| *next)
				.unwrap_or(self.our_position);
			self.our_position = away;
			self.path.clear();
			self.hop_days_left = 0;
			self.wait_days = 0;
			self.decision_pending = true;
			self.enemy_position = self.goal;
		}
	}

	fn occupy_goal(&mut self) {
		let mut objects = WarObjects::new(1, 2);
		let mut costs = Vec::new();
		for (index, province) in self.map.provinces.iter().enumerate() {
			objects.add_province(2, 2, 0);
			costs.push(ProvinceCost {
				development: province.development,
				capital: index == 0,
				..ProvinceCost::default()
			});
		}
		let rules = WarScoreRules {
			capital_mult: self.defines.capital_mult,
			autonomy: self.defines.autonomy,
			..WarScoreRules::VANILLA
		};
		let score = |objects: &WarObjects| -> i32 {
			let input = WarScoreInput {
				war: objects,
				costs: &costs,
				battles: 0,
			};
			war_score(&input, &rules)
		};
		let before = score(&objects);
		objects.reoccupy(0, 1, foch_eu4_state::candidates::province::OCCUPIED_MASK);
		self.score_points += (score(&objects) - before) as f32 + self.defines.wargoal_bonus;
		self.occupation_points += (score(&objects) - before) as f32 + self.defines.wargoal_bonus;
		self.sieges_won += 1;
		self.pockets.push(self.goal);
	}

	/// The episode's reward: collected score minus the manpower price, with
	/// the battle term under the drawn cap.
	pub fn reward(&self) -> f32 {
		let battles = self
			.battle_points
			.clamp(-self.defines.battles_cap, self.defines.battles_cap);
		self.score_points + battles - self.manpower_lost / 10_000.0
	}

	pub fn finished(&self) -> bool {
		self.day >= self.horizon
	}

	/// Per-province features for one candidate destination.
	/// Order the enemy army to march at a province, the outside-driven side
	/// of self-play.
	pub fn enemy_order(&mut self, destination: u32) {
		if destination == self.enemy_position {
			return;
		}
		self.enemy_path = self.map.path(self.enemy_position, destination);
		if self.enemy_hop_days == 0
			&& let Some(&first) = self.enemy_path.first()
		{
			self.enemy_hop_days = self.map.edges[self.enemy_position as usize]
				.iter()
				.find(|(next, _)| *next == first)
				.map(|(_, days)| *days)
				.unwrap_or(DAYS_PER_PROVINCE);
		}
	}

	/// The capped candidate set from any position, not just ours.
	pub fn candidates_from(&self, position: u32) -> Vec<u32> {
		let mut picked = vec![position];
		for anchor in [self.goal, self.our_position] {
			if !picked.contains(&anchor) {
				picked.push(anchor);
			}
		}
		let mut reachable: Vec<(u32, u32)> = (0..self.map.provinces.len() as u32)
			.map(|index| (self.map.travel_days(position, index), index))
			.filter(|(days, index)| *days <= 2 * DAYS_PER_PROVINCE && !picked.contains(index))
			.collect();
		reachable.sort();
		picked.extend(
			reachable
				.into_iter()
				.take(CANDIDATE_LIMIT - picked.len())
				.map(|(_, index)| index),
		);
		picked
	}

	/// The heuristic continuation used inside lookahead rollouts: take the
	/// war goal if it is a candidate, else step toward it. Only the oracle
	/// uses this; the policy never sees it.
	fn heuristic_pick(&self, candidates: &[u32]) -> usize {
		candidates
			.iter()
			.enumerate()
			.min_by_key(|(slot, index)| {
				let travel = self.map.travel_days(**index, self.goal);
				(travel, *slot)
			})
			.map(|(slot, _)| slot)
			.unwrap_or(0)
	}

	/// The expert label for the current decision point: roll each candidate
	/// out to the horizon under the heuristic continuation, average the
	/// final rewards over `seeds` dice draws, and return the best slot. This
	/// is one-step expert iteration ground truth, not a hand feature.
	pub fn lookahead_label<P>(&self, candidates: &[u32], seeds: u32, pick: P) -> usize
	where
		P: Fn(&Campaign, &[u32]) -> usize,
	{
		self.lookahead_values(candidates, seeds, pick)
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
			.map(|(i, _)| i)
			.unwrap_or(0)
	}

	/// The per-candidate average reward from rolling each to the horizon.
	/// These are the per-target advantage labels: how well choosing this
	/// candidate pays, against this enemy, on this ground.
	pub fn lookahead_values<P>(&self, candidates: &[u32], seeds: u32, pick: P) -> Vec<f32>
	where
		P: Fn(&Campaign, &[u32]) -> usize,
	{
		let mut values = Vec::with_capacity(candidates.len());
		// Common random numbers: every candidate rolls the same dice.
		for candidate in candidates {
			let mut total = 0.0f32;
			for seed in 0..seeds {
				let mut branch = self.clone();
				let mut rng = Rng::seed(
					(u64::from(branch.day) << 32) ^ (u64::from(seed + 1).wrapping_mul(0x9e37_79b9)),
				);
				branch.order(*candidate);
				while !branch.finished() {
					if branch.decision_pending {
						let next = branch.candidates();
						let slot = pick(&branch, &next);
						branch.order(next[slot]);
					}
					branch.step_day(&mut rng);
				}
				total += branch.reward();
			}
			values.push(total / seeds as f32);
		}
		values
	}

	/// The label under the built-in heuristic continuation.
	pub fn heuristic_label(&self, candidates: &[u32], seeds: u32) -> usize {
		self.lookahead_label(candidates, seeds, |branch, next| {
			branch.heuristic_pick(next)
		})
	}

	/// The position of the army that owes the decision.
	pub fn deciding_position(&self) -> u32 {
		match (self.deciding, &self.second_army) {
			(1, Some((pos, _))) => *pos,
			_ => self.our_position,
		}
	}

	/// The composition of the army that owes the decision.
	pub fn deciding_army(&self) -> Army {
		match (self.deciding, &self.second_army) {
			(1, Some((_, army))) => *army,
			_ => self.ours,
		}
	}

	/// Order the deciding army to a destination. For the second army,
	/// we track just the position (the march resolution is simplified).
	pub fn order_deciding(&mut self, destination: u32) {
		if self.deciding == 1 {
			if let Some((pos, _)) = &mut self.second_army {
				*pos = destination;
			}
			// Alternate back to the primary army.
			self.deciding = 0;
			self.decision_pending = false;
			return;
		}
		self.order(destination);
		if self.second_army.is_some() && !self.second_army_ai {
			// The second army is player-controlled: ask the policy.
			self.deciding = 1;
			self.decision_pending = true;
		} else if self.second_army.is_some() && self.second_army_ai {
			// The second army is an AI ally following the vanilla spirit:
			// besiege the goal, defend it, stack with the player when
			// nearby, seek battle only with a clear edge, and retreat
			// from hopeless engagements.
			let ally_pos = self
				.second_army
				.map(|(p, _)| p)
				.unwrap_or(self.our_position);
			let ally_strength = self.second_army.map(|(_, a)| a.regiments()).unwrap_or(0.0);
			let enemy_strength = self.enemy.regiments();
			let player_nearby =
				self.map.travel_days(self.our_position, ally_pos) <= 2 * DAYS_PER_PROVINCE;
			let combined = ally_strength
				+ if player_nearby {
					self.ours.regiments()
				} else {
					0.0
				};
			let enemy_dist = self.map.travel_days(ally_pos, self.enemy_position);
			let target = if ally_pos == self.goal && !self.pockets.contains(&self.goal) {
				// Already besieging: stay on it.
				ally_pos
			} else if self.enemy_position == self.goal {
				// The objective is threatened: go defend it.
				self.goal
			} else if combined > enemy_strength * 1.5 && enemy_dist <= 3 * DAYS_PER_PROVINCE {
				// Clear edge over a nearby enemy: seek the battle.
				self.enemy_position
			} else if ally_strength < enemy_strength * 0.5 && enemy_dist <= 2 * DAYS_PER_PROVINCE {
				// Hopeless engagement nearby: move away from the enemy.
				self.map.edges[ally_pos as usize]
					.iter()
					.map(|(next, _)| *next)
					.max_by_key(|next| self.map.travel_days(*next, self.enemy_position))
					.unwrap_or(ally_pos)
			} else {
				// Default: head for the war goal to siege it.
				self.goal
			};
			if target != ally_pos
				&& let Some((pos, _)) = &mut self.second_army
			{
				let hops = self.map.path(ally_pos, target);
				if let Some(&first) = hops.first() {
					*pos = first;
				}
			}
			// Stay on the primary army's decision cycle.
		}
	}

	/// The raw rule numbers the world runs on and the policy reads: the
	/// drawn defines, both techs, and the clock. Raw game values, nothing
	/// strategic.
	pub fn rules_vector(&self) -> [f32; 15] {
		// The game's own computed stats, straight from the tech table and
		// the current state. No hand-crafted aggregation: if the game UI
		// shows it to a player, it is here.
		let tech = |t: u32| (t as usize).min(31);
		[
			// The war's payscale.
			self.defines.battles_cap / 40.0,
			self.defines.wargoal_bonus / 5.0,
			self.defines.ticking_per_month / 0.4,
			self.day as f32 / self.horizon as f32,
			self.goal_type(),
			// The game's military stats, from the tech table.
			MIL_TACTICS[tech(self.our_tech)],
			MIL_TACTICS[tech(self.enemy_tech)],
			MIL_MORALE[tech(self.our_tech)] / 8.0,
			MIL_MORALE[tech(self.enemy_tech)] / 8.0,
			MIL_WIDTH[tech(self.our_tech.max(self.enemy_tech))] / 40.0,
			// Current morale as a fraction of max (what the bar shows).
			self.ours.morale,
			// Supply limit where we stand: development-scaled.
			(4.0 + self.map.provinces[self.our_position as usize].development as f32) / 35.0,
			// Army composition: infantry and artillery fractions.
			self.ours.infantry / self.ours.regiments().max(0.1),
			self.ours.artillery / self.ours.regiments().max(0.1),
			// How much of the map we have walked.
			self.visited.len() as f32 / self.map.provinces.len() as f32,
		]
	}

	/// What kind of war: 0 occupy, 1 battles, 2 defend.
	///
	/// This is the current simulator channel. An unseen or numerically
	/// edited casus belli is not a fourth class here; score it with
	/// [`crate::ResidualCategory`] so the id embedding is zero and the
	/// mechanic vector still contributes.
	pub fn goal_type(&self) -> f32 {
		match self.wargoal.as_str() {
			"superiority" => 1.0,
			"defend_capital" | "defend_country" => 2.0,
			_ => 0.0,
		}
	}

	/// Zero-shot casus-belli score. `modifiers` are the CB's own numbers.
	/// A name the exported table does not contain, or a known name whose
	/// numbers no longer match, uses `E_id = 0`.
	pub fn wargoal_score(&self, model: &crate::ResidualCategory, modifiers: [f32; 5]) -> Vec<f32> {
		model.score(&self.wargoal, modifiers)
	}

	/// One number for "can I win a fight". Positive means press.
	/// Per-province features for one candidate destination.
	pub fn province_features(&self, index: u32) -> [f32; PROVINCE_FEATURES] {
		let province = &self.map.provinces[index as usize];
		let mut out = [0.0; PROVINCE_FEATURES];
		// A category the five vanilla rows do not match — a mod name, or
		// a vanilla name whose mechanics were edited — stays an all-zero
		// one-hot. Movement cost below still carries the mechanic.
		if let Some(terrain_index) = TERRAINS
			.iter()
			.position(|terrain| *terrain == province.terrain)
		{
			out[terrain_index] = 1.0;
		}
		out[5] = province.terrain.movement_cost;
		out[6] = self.map.travel_days(self.our_position, index) as f32 / 48.0;
		out[7] = f32::from(index == self.goal);
		out[8] = f32::from(index == self.enemy_position) * self.enemy.regiments() / 14.0;
		out[9] = province.development as f32 / 35.0;
		out[10] = self.map.travel_days(index, self.goal) as f32 / 48.0;
		out[11] = f32::from(
			self.map
				.path(self.enemy_position, self.goal)
				.contains(&index)
				&& index != self.enemy_position,
		);
		out
	}

	/// The raw board: one entry per province, its padded-board cell and its
	/// raw channels.
	pub fn board(&self) -> (usize, usize, Vec<Vec<f32>>) {
		let mut cells = Vec::new();
		for (index, province) in self.map.provinces.iter().enumerate() {
			let index = index as u32;
			let mut row = vec![
				f32::from(index == 0),
				f32::from(index == 1),
				f32::from(index == 2),
				f32::from(index == 3),
				f32::from(index == 4),
			];
			row.push(province.development as f32 / 35.0);
			row.push(f32::from(province.capital));
			row.push(self.fort_level as f32 / 3.0);
			row.push(if index == self.our_position {
				self.ours.regiments() / 14.0
			} else {
				0.0
			});
			// Fog of war: the enemy is visible from the army's province,
			// its neighbors, and every captured province's neighbors.
			let seen_from_here = index == self.our_position
				|| self.map.edges[self.our_position as usize]
					.iter()
					.any(|(next, _)| *next == index);
			let seen_from_held = self.pockets.iter().any(|held| {
				*held == index
					|| self.map.edges[(*held) as usize]
						.iter()
						.any(|(next, _)| *next == index)
			});
			let enemy_visible = seen_from_here || seen_from_held;
			row.push(if index == self.enemy_position && enemy_visible {
				self.enemy.regiments() / 14.0
			} else {
				0.0
			});
			row.push(f32::from(index == self.goal));
			// Second army position marker (0 when no second army).
			row.push(match &self.second_army {
				Some((pos, _)) if *pos == index => 1.0,
				_ => 0.0,
			});
			cells.push(vec![province.x as f32 + 1.0, province.y as f32 + 1.0]);
			cells.last_mut().unwrap().extend(row);
		}
		(BOARD_WIDTH + 2, BOARD_HEIGHT + 2, cells)
	}

	/// The graph adjacency as triples of `[from, to, days]`, symmetric.
	pub fn adjacency(&self) -> Vec<[f32; 3]> {
		let mut out = Vec::new();
		for (from, neighbors) in self.map.edges.iter().enumerate() {
			for (to, days) in neighbors {
				if (*to as usize) > from {
					out.push([from as f32, *to as f32, *days as f32]);
				}
			}
		}
		out
	}

	/// A plain-language rendering of the decision point, for a language
	/// model policy: the world, both armies, and each candidate as a line.
	pub fn describe(&self, candidates: &[u32]) -> String {
		let terrain = self.map.provinces[self.our_position as usize].terrain;
		let mut out = format!(
			"Day {} of {}. You command {} regiments (morale {:.0}%) at military tech {}, standing on {} {}. ",
			self.day,
			self.horizon,
			self.ours.regiments(),
			self.ours.morale * 100.0,
			self.our_tech,
			terrain.name,
			if terrain == PLAINS {
				"open ground"
			} else {
				"bad ground"
			},
		);
		out.push_str(&format!(
			"The enemy field army, {} regiments at tech {}, ",
			self.enemy.regiments(),
			self.enemy_tech
		));
		if self.enemy_position == self.goal {
			out.push_str("stands on the war goal. ");
		} else {
			out.push_str(&format!(
				"is {} days away. ",
				self.map.travel_days(self.enemy_position, self.goal)
			));
		}
		if self.pockets.contains(&self.goal) {
			out.push_str("You hold the war goal. ");
		} else if self.our_position == self.goal {
			out.push_str("You are besieging the war goal. ");
		} else {
			out.push_str(&format!(
				"The war goal is {} days away. ",
				self.map.travel_days(self.our_position, self.goal)
			));
		}
		out.push_str(&format!(
			"This war pays battles up to {:.0} points; holding the goal ticks {:.1} a month after a {:.0}-point bonus. ",
			self.defines.battles_cap,
			self.defines.ticking_per_month,
			self.defines.wargoal_bonus
		));
		if terrain.attacker_pips < 0 {
			out.push_str(&format!(
				"Ground: an attacker on this {} loses {} pips. ",
				terrain.name, -terrain.attacker_pips
			));
		}
		out.push_str("Orders:");
		for (index, candidate) in candidates.iter().enumerate() {
			let province = &self.map.provinces[*candidate as usize];
			out.push_str(&format!(
				"
{}. province {} days away, {}, development {},{}{}",
				index + 1,
				self.map.travel_days(self.our_position, *candidate),
				province.terrain.name,
				province.development,
				if *candidate == self.goal {
					" the war goal,"
				} else {
					""
				},
				if *candidate == self.enemy_position {
					" the enemy is there"
				} else {
					""
				},
			));
		}
		out
	}

	/// Broadcast features: our shape, the enemy, the clock, the goal class,
	/// and the drawn defines.
	pub fn global_features(&self) -> [f32; GLOBAL_FEATURES] {
		[
			self.day as f32 / self.horizon as f32,
			self.ours.regiments() / 14.0,
			self.ours.morale,
			self.enemy.regiments() / 14.0,
			f32::from(self.pockets.contains(&self.goal)),
			f32::from(self.wargoal == "superiority"),
			f32::from(self.our_position == self.goal),
			f32::from(self.our_position == self.enemy_position),
			self.defines.battles_cap / 40.0,
			self.defines.wargoal_bonus / 5.0,
			self.defines.ticking_per_month / 0.4,
			self.defines.superiority_threshold / 10.0,
			self.defines.capital_mult / 1.2,
			self.defines.autonomy / 0.33,
			self.our_tech as f32 / 32.0,
			self.enemy_tech as f32 / 32.0,
		]
	}
}

/// The environment's own RNG.
pub struct Rng {
	state: u64,
}

impl Rng {
	pub fn seed(state: u64) -> Self {
		Self { state }
	}

	fn draw(&mut self) -> u64 {
		self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
		let mut z = self.state;
		z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
		z ^ (z >> 31)
	}

	pub fn below(&mut self, bound: u64) -> u64 {
		self.draw() % bound.max(1)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_generated_map_is_connected_with_real_terrain() {
		for seed in 0..32u64 {
			let map = Map::generate(seed);
			assert!((9..17).contains(&map.provinces.len()));
			for from in 0..map.provinces.len() as u32 {
				for to in 0..map.provinces.len() as u32 {
					assert!(
						map.travel_days(from, to) < u32::MAX,
						"every province must reach every other (seed {seed})"
					);
				}
			}
			assert!(map.provinces.iter().all(|p| TERRAINS.contains(&p.terrain)));
		}
	}

	#[test]
	fn mountains_cost_double_days_and_punish_attackers() {
		assert_eq!(march_days(MOUNTAINS), 2 * march_days(PLAINS));
		assert_eq!(MOUNTAINS.attacker_pips, -2);
		assert_eq!(PLAINS.attacker_pips, 0);
	}

	#[test]
	fn a_march_runs_on_its_own_clock_not_a_monthly_tick() {
		let mut rng = Rng::seed(3);
		let mut campaign = Campaign::generate(11, "glory_seeker_personality", "take_province");
		let target = campaign.map.provinces.len() as u32 - 1;
		campaign.order(target);
		let travel = campaign.map.travel_days(campaign.our_position, target);
		let mut stepped = 0;
		while !campaign.decision_pending && !campaign.finished() {
			campaign.step_day(&mut rng);
			stepped += 1;
		}
		assert!(
			stepped <= travel.max(1) + DAYS_PER_SIEGE_PHASE,
			"a march decides on arrival ({stepped} days vs {travel} travel)"
		);
		assert_eq!(campaign.our_position, target);
	}

	#[test]
	fn battles_terminate_and_pay_the_engine_loss_scale() {
		for seed in 0..64u64 {
			let mut rng = Rng::seed(seed);
			let (points, destroyed, losses) = resolve_battle(
				&mut rng,
				Army {
					infantry: 10.0,
					cavalry: 2.0,
					artillery: 2.0,
					morale: 1.0,
				},
				Army {
					infantry: 8.0,
					cavalry: 2.0,
					artillery: 0.0,
					morale: 1.0,
				},
				Pips { fire: 2, shock: 2 },
				Pips { fire: 1, shock: 1 },
				PLAINS,
				true,
				(1.0, 1.0),
			);
			assert!(destroyed >= 0.0 && losses >= 0.0);
			if points > 0.0 {
				assert!((points - (0.1 + destroyed / 20.0)).abs() < 1e-4);
			} else {
				assert!((points + (0.1 + losses / 20.0)).abs() < 1e-4);
			}
		}
	}

	#[test]
	fn unknown_terrain_and_an_unseen_casus_belli_keep_mechanics_without_a_vanilla_id() {
		let mut campaign = Campaign::generate(4, "careful_personality", "superiority");
		campaign.map.provinces[0].terrain = Terrain {
			name: "haunted_bog",
			movement_cost: 3.0,
			attacker_pips: -1,
			attrition: 0.05,
		};
		let features = campaign.province_features(0);
		assert_eq!(&features[..5], &[0.0; 5]);
		assert_eq!(features[5], 3.0);

		let model = crate::ResidualCategory::from_json(
			r#"{
			"kind": "residual-category",
			"names": ["superiority"],
			"modifiers": [[1.0, 0.4, 0.0, 1.0, 0.0]],
			"f0_weight": [[1, 0, 0, 0, 0], [0, 1, 0, 0, 0]],
			"f0_bias": [0, 0],
			"f2_weight": [[1, 0], [0, 1]],
			"f2_bias": [0, 0],
			"embedding": [[9, 9], [1, 0]]
		}"#,
		)
		.expect("cb head");
		let vanilla = [1.0, 0.4, 0.0, 1.0, 0.0];
		let edited = [9.0, 0.4, 0.0, 1.0, 0.0];
		let custom = [2.0, 0.1, 1.0, 0.0, 0.5];
		assert_eq!(campaign.wargoal_score(&model, vanilla), vec![2.0, 0.4]);
		campaign.wargoal = "my_mod_cb".to_string();
		assert_eq!(campaign.wargoal_score(&model, custom), vec![2.0, 0.1]);
		campaign.wargoal = "superiority".to_string();
		assert_eq!(model.resolve("superiority", edited), 0);
		assert_eq!(campaign.wargoal_score(&model, edited), vec![9.0, 0.4]);
	}

	#[test]
	fn a_modded_define_moves_both_world_and_features() {
		let mut campaign = Campaign::generate(21, "careful_personality", "superiority");
		let before = campaign.global_features()[8];
		campaign.defines.battles_cap *= 2.0;
		let after = campaign.global_features()[8];
		assert!(
			(after / before - 2.0).abs() < 1e-4,
			"a doubled define must double the feature: {before} -> {after}"
		);
	}

	#[test]
	fn candidates_are_capped_and_carry_both_anchors() {
		for seed in 0..32u64 {
			let campaign = Campaign::generate(seed, "glory_seeker_personality", "take_province");
			let candidates = campaign.candidates();
			assert!(!candidates.is_empty());
			assert!(candidates.len() <= CANDIDATE_LIMIT);
			assert_eq!(
				candidates[0], campaign.our_position,
				"holding is always first"
			);
			assert!(
				candidates.contains(&campaign.goal),
				"the war goal is always a candidate"
			);
			assert!(
				candidates.contains(&campaign.enemy_position),
				"the enemy's province is always a candidate"
			);
		}
	}

	#[test]
	fn tech_gates_artillery_and_scales_pips() {
		let low = Campaign::generate(41, "glory_seeker_personality", "take_province");
		let mut high = Campaign::generate(41, "glory_seeker_personality", "take_province");
		high.our_tech = 20;
		high.enemy_tech = 20;
		assert!(low.our_tech < ARTILLERY_TECH || low.ours.artillery > 0.0);
		assert!(high.our_tech >= ARTILLERY_TECH && high.ours.artillery > 0.0);
		assert!(high.enemy_tech >= ARTILLERY_TECH && high.enemy.artillery > 0.0);
		let pips = high.tech_pips(true);
		assert!(pips.fire >= 2, "late tech fights on fire");
	}

	#[test]
	fn defines_stay_vanilla_unless_the_game_is_modded() {
		let mut vanilla = 0;
		for seed in 0..64u64 {
			if DefineDraw::draw(seed) == DefineDraw::VANILLA {
				vanilla += 1;
			}
		}
		assert!(
			vanilla >= 40,
			"most scenarios must run vanilla, got {vanilla}/64"
		);
		assert!(vanilla < 64, "some scenarios must see a modded draw");
	}

	#[test]
	fn a_siege_draws_the_defender_to_relieve_it() {
		let mut campaign = Campaign::generate(51, "glory_seeker_personality", "take_province");
		// Put us on the goal, besieging, and the enemy one hop away.
		campaign.our_position = campaign.goal;
		campaign.enemy_position = campaign.map.edges[campaign.goal as usize]
			.first()
			.map(|(next, _)| *next)
			.unwrap_or(campaign.goal);
		campaign.enemy_path.clear();
		campaign.enemy_hop_days = 0;
		campaign.wait_days = DAYS_PER_SIEGE_PHASE;
		campaign.decision_pending = false;
		let mut rng = Rng::seed(9);
		for _ in 0..4 * DAYS_PER_PROVINCE {
			campaign.step_day(&mut rng);
			let met = campaign.our_position == campaign.enemy_position;
			assert!(
				!met || campaign.manpower_lost > 0.0,
				"meeting the defender must bring a battle"
			);
			if campaign.manpower_lost > 0.0 || met {
				return;
			}
		}
		panic!("the defender never came to relieve the siege");
	}

	#[test]
	fn bad_ground_bleeds_the_army() {
		let mut campaign = Campaign::generate(61, "careful_personality", "take_province");
		// Stand on marsh for a year of holds.
		for (index, province) in campaign.map.provinces.iter().enumerate() {
			if province.terrain == MARSH && index as u32 != campaign.goal {
				campaign.our_position = index as u32;
				break;
			}
		}
		campaign.decision_pending = false;
		campaign.hop_days_left = 0;
		campaign.wait_days = 360;
		let mut rng = Rng::seed(2);
		let mut days = 0;
		while days < 180 {
			campaign.step_day(&mut rng);
			days += 1;
		}
		// Marsh runs three percent a month: six months on a fourteen-regiment
		// army bleed about two regiments.
		assert!(
			campaign.manpower_lost >= 1_500.0,
			"six months of marsh attrition must bleed: {:.0}",
			campaign.manpower_lost
		);
	}

	#[test]
	fn defending_good_ground_wins_the_battles_attackers_lose() {
		let mut mountain_wins = 0;
		let mut plain_wins = 0;
		for seed in 0..200u64 {
			let mut rng = Rng::seed(seed);
			let ours = Army {
				infantry: 10.0,
				cavalry: 2.0,
				artillery: 2.0,
				morale: 1.0,
			};
			let enemy = Army {
				infantry: 10.0,
				cavalry: 2.0,
				artillery: 2.0,
				morale: 1.0,
			};
			let (points, _, _) = resolve_battle(
				&mut rng,
				ours,
				enemy,
				Pips { fire: 1, shock: 1 },
				Pips { fire: 1, shock: 1 },
				MOUNTAINS,
				false,
				(1.0, 1.0),
			);
			if points > 0.0 {
				mountain_wins += 1;
			}
			let mut rng = Rng::seed(seed);
			let (points, _, _) = resolve_battle(
				&mut rng,
				ours,
				enemy,
				Pips { fire: 1, shock: 1 },
				Pips { fire: 1, shock: 1 },
				PLAINS,
				false,
				(1.0, 1.0),
			);
			if points > 0.0 {
				plain_wins += 1;
			}
		}
		assert!(
			mountain_wins > plain_wins + 40,
			"defending mountains must clearly beat defending plains: {mountain_wins} vs {plain_wins}"
		);
	}

	#[test]
	fn every_candidate_carries_features() {
		let campaign = Campaign::generate(31, "glory_seeker_personality", "take_province");
		for index in 0..campaign.map.provinces.len() as u32 {
			let features = campaign.province_features(index);
			assert!(features.iter().all(|value| value.is_finite()));
		}
		assert_eq!(campaign.global_features().len(), GLOBAL_FEATURES);
	}
}

#[cfg(test)]
mod chain_tests {
	use super::*;

	#[test]
	fn marching_at_a_defended_goal_fights() {
		for seed in 1..16u64 {
			let mut rng = Rng::seed(seed);
			let mut campaign =
				Campaign::generate(seed, "glory_seeker_personality", "take_province");
			let start_enemy = campaign.enemy.regiments();
			campaign.order(campaign.goal);
			while !campaign.finished() && !campaign.decision_pending {
				campaign.step_day(&mut rng);
			}
			// The march either met the defender (a battle) or is still on
			// the road; both must happen within the horizon.
			let mut met = campaign.manpower_lost > 0.0 || campaign.enemy.regiments() < start_enemy;
			while !campaign.finished() && !met {
				if campaign.decision_pending {
					campaign.order(campaign.goal);
				}
				campaign.step_day(&mut rng);
				met = campaign.manpower_lost > 0.0 || campaign.enemy.regiments() < start_enemy;
			}
			assert!(
				met,
				"seed {seed}: the defender must be reachable and fightable"
			);
		}
	}
}
