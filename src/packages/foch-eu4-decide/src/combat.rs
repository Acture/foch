//! Slot-based line combat: per-regiment front, back, and reserve rows.
//!
//! The campaign hands both armies in as aggregate regiment counts; the
//! engine splits them into regiments of one strength each (one partial
//! tail regiment per arm allowed) and deploys them the way the game
//! does. The front row holds `width` slots - infantry in the center,
//! cavalry on the flank seats - the back row holds artillery behind
//! occupied slots, and every further regiment waits in reserve. Days
//! run in three-day fire and shock phases (`DAYS_PER_PHASE`), each
//! phase rolling one die per side plus the general's pips, with the
//! attacker eating the ground's penalty. A front regiment strikes the
//! slot directly across; when that slot is empty it strikes the nearest
//! enemy within its flanking range - cavalry reaches `flank_range`
//! slots (the cavalry `maneuver` stat), other arms one. Back-row
//! artillery fires straight ahead at reduced strength
//! (`BACK_LINE_MODIFIER`) during fire phases and soaks a fraction of
//! the morale damage its column's front regiment takes
//! (`BACK_LINE_MORALE_DAMAGE_TAKEN_MODIFIER`). Deployed regiments bleed
//! `BATTLE_DAILY_MORALE_DAMAGE` a day and reserves
//! `RESERVE_DAILY_MORALE_DAMAGE`. A regiment out of strength is
//! destroyed; one out of morale routs off the line and a reserve fills
//! the slot. The battle ends when a side's deployed line is empty, its
//! living regiments' average morale falls to `LOW_MORALE_THRESHOLD`, or
//! sixty days pass. As in [`crate::world::resolve_battle_full`], the
//! per-day casualty coefficients are a documented stand-in for the
//! un-decompiled damage tables; every other number is the game's.

use crate::world::{
	Army, COMBAT_DICE_SIDE, DAYS_PER_PHASE, LOW_MORALE_THRESHOLD, Pips, Rng, Terrain,
};

/// Damage a back-row regiment deals, as a fraction of front-row output
/// (`BACK_LINE_MODIFIER`).
pub const BACK_LINE_MODIFIER: f32 = 0.5;
/// Morale damage a back-row regiment takes when its column is struck
/// (`BACK_LINE_MORALE_DAMAGE_TAKEN_MODIFIER`).
pub const BACK_LINE_MORALE_DAMAGE_TAKEN_MODIFIER: f32 = 0.4;
/// Strength damage a back-row regiment takes
/// (`BACK_LINE_STRENGTH_DAMAGE_TAKEN_MODIFIER`).
pub const BACK_LINE_STRENGTH_DAMAGE_TAKEN_MODIFIER: f32 = 0.0;
/// Fraction of maximum morale a deployed regiment bleeds each day
/// (`BATTLE_DAILY_MORALE_DAMAGE`).
pub const BATTLE_DAILY_MORALE_DAMAGE: f32 = 0.01;
/// Fraction of maximum morale a reserve regiment bleeds each day
/// (`RESERVE_DAILY_MORALE_DAMAGE`).
pub const RESERVE_DAILY_MORALE_DAMAGE: f32 = 0.02;
/// Days a battle may run before both sides disengage.
const BATTLE_DAY_LIMIT: u32 = 60;
/// Flanking reach of a non-cavalry regiment: the infantry `maneuver`
/// stat in `common/units`.
const FOOT_FLANK_RANGE: usize = 1;

/// One regiment's arm of service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Infantry,
	Cavalry,
	Artillery,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regiment {
	pub kind: Kind,
	/// Men standing, as a fraction of a full regiment.
	pub strength: f32,
	/// Fighting spirit, as a fraction of maximum.
	pub morale: f32,
}

impl Regiment {
	/// Weight in the army-power ratio: `Army::strength`'s unit table.
	fn power(&self) -> f32 {
		self.strength
			* match self.kind {
				Kind::Infantry => 1.0,
				Kind::Cavalry => 1.5,
				Kind::Artillery => 2.0,
			}
	}

	/// How far past an empty facing slot this regiment strikes: cavalry
	/// reach the drawn flank range, other arms a single slot.
	fn flank_range(&self, flank: usize) -> usize {
		if self.kind == Kind::Cavalry {
			flank.max(FOOT_FLANK_RANGE)
		} else {
			FOOT_FLANK_RANGE
		}
	}
}

/// Split an army's aggregate counts into regiments of one strength
/// each - one partial tail regiment per arm - all carrying the army's
/// morale. Infantry first, then cavalry, then artillery: the reserve
/// queue reinforces fronts with foot before horse.
pub fn split(army: Army) -> Vec<Regiment> {
	let morale = army.morale.clamp(0.0, 1.0);
	let mut out = Vec::new();
	fn push(out: &mut Vec<Regiment>, count: f32, kind: Kind, morale: f32) {
		let full = count.max(0.0).floor() as usize;
		for _ in 0..full {
			out.push(Regiment {
				kind,
				strength: 1.0,
				morale,
			});
		}
		let tail = count.max(0.0) - full as f32;
		if tail > 1e-6 {
			out.push(Regiment {
				kind,
				strength: tail,
				morale,
			});
		}
	}
	push(&mut out, army.infantry, Kind::Infantry, morale);
	push(&mut out, army.cavalry, Kind::Cavalry, morale);
	push(&mut out, army.artillery, Kind::Artillery, morale);
	out
}

/// One side's deployment: front and back rows of `width` slots, the
/// reserve behind them, and the regiments routed off the line.
#[derive(Clone, Debug)]
pub struct Line {
	pub front: Vec<Option<Regiment>>,
	pub back: Vec<Option<Regiment>>,
	pub reserve: Vec<Regiment>,
	pub routed: Vec<Regiment>,
}

impl Line {
	/// Deploy into `width` slots, both lines centered so a wider front
	/// faces empty ground on the flanks: infantry fills the center,
	/// cavalry takes the outermost seats of the line, artillery stands
	/// behind occupied front slots left to right, the rest waits in
	/// reserve.
	pub fn deploy(regiments: &[Regiment], width: usize) -> Self {
		let width = width.max(1);
		let mut front = vec![None; width];
		let mut back = vec![None; width];
		let of_kind = |kind| {
			regiments
				.iter()
				.filter(|r| r.kind == kind)
				.copied()
				.collect()
		};
		let infantry: Vec<Regiment> = of_kind(Kind::Infantry);
		let cavalry: Vec<Regiment> = of_kind(Kind::Cavalry);
		let artillery: Vec<Regiment> = of_kind(Kind::Artillery);
		// Cavalry seats are what the front row has left after infantry
		// takes the center.
		let inf_front = infantry.len().min(width);
		let cav_front = cavalry.len().min(width - inf_front);
		let span = inf_front + cav_front;
		let start = (width - span) / 2;
		// Cavalry splits between the two flanks, outermost seats first.
		let on_left = cav_front / 2;
		for index in 0..on_left {
			front[start + index] = Some(cavalry[index]);
		}
		for index in on_left..cav_front {
			front[start + span - 1 - (index - on_left)] = Some(cavalry[index]);
		}
		for index in 0..inf_front {
			front[start + on_left + index] = Some(infantry[index]);
		}
		// Artillery only stands where a front regiment covers it.
		let mut spare = artillery.into_iter();
		for slot in 0..width {
			if front[slot].is_some()
				&& let Some(regiment) = spare.next()
			{
				back[slot] = Some(regiment);
			}
		}
		let reserve = infantry
			.into_iter()
			.skip(inf_front)
			.chain(cavalry.into_iter().skip(cav_front))
			.chain(spare)
			.collect();
		Self {
			front,
			back,
			reserve,
			routed: Vec::new(),
		}
	}

	/// Every regiment still with the army: deployed, in reserve, routed.
	pub fn living(&self) -> impl Iterator<Item = &Regiment> {
		self.front
			.iter()
			.flatten()
			.chain(self.back.iter().flatten())
			.chain(&self.reserve)
			.chain(&self.routed)
	}

	/// Total strength left, in regiments.
	pub fn strength_sum(&self) -> f32 {
		self.living().map(|regiment| regiment.strength).sum()
	}

	/// Average morale over living regiments; a destroyed army has none.
	pub fn average_morale(&self) -> f32 {
		let mut total = 0.0;
		let mut count = 0usize;
		for regiment in self.living() {
			total += regiment.morale;
			count += 1;
		}
		if count == 0 {
			0.0
		} else {
			total / count as f32
		}
	}

	/// Army power for the tactics ratio (`Army::strength`'s table).
	pub fn power(&self) -> f32 {
		self.living().map(Regiment::power).sum()
	}

	/// No regiment left standing in either row.
	pub fn deployed_empty(&self) -> bool {
		self.front.iter().all(Option::is_none) && self.back.iter().all(Option::is_none)
	}

	/// The daily morale bleed: deployed regiments lose
	/// `BATTLE_DAILY_MORALE_DAMAGE`, reserves
	/// `RESERVE_DAILY_MORALE_DAMAGE`, both scaled like all morale damage.
	pub fn daily_drain(&mut self, scale: f32) {
		for regiment in self.front.iter_mut().chain(self.back.iter_mut()).flatten() {
			regiment.morale -= BATTLE_DAILY_MORALE_DAMAGE * scale;
		}
		for regiment in self.reserve.iter_mut() {
			regiment.morale -= RESERVE_DAILY_MORALE_DAMAGE * scale;
		}
	}

	/// Clear the destroyed (no strength) and pull the broken (no morale)
	/// off the line and out of the reserve queue.
	pub fn sweep(&mut self) {
		let Line {
			front,
			back,
			reserve,
			routed,
		} = self;
		for slot in front.iter_mut().chain(back.iter_mut()) {
			if let Some(regiment) = slot.as_mut() {
				if regiment.strength <= 0.0 {
					*slot = None;
				} else if regiment.morale <= 0.0 {
					routed.push(*regiment);
					*slot = None;
				}
			}
		}
		let queue = std::mem::take(reserve);
		for regiment in queue {
			if regiment.strength <= 0.0 {
				// Destroyed in the queue; counted by strength difference.
			} else if regiment.morale <= 0.0 {
				routed.push(regiment);
			} else {
				reserve.push(regiment);
			}
		}
	}

	/// Fill empty front slots from the reserve - foot and horse only -
	/// and empty back slots behind an occupied front with artillery.
	pub fn reinforce(&mut self) {
		for slot in 0..self.front.len() {
			if self.front[slot].is_none()
				&& let Some(index) = self
					.reserve
					.iter()
					.position(|regiment| regiment.kind != Kind::Artillery)
			{
				self.front[slot] = Some(self.reserve.remove(index));
			}
		}
		for slot in 0..self.back.len() {
			if self.back[slot].is_none()
				&& self.front[slot].is_some()
				&& let Some(index) = self
					.reserve
					.iter()
					.position(|regiment| regiment.kind == Kind::Artillery)
			{
				self.back[slot] = Some(self.reserve.remove(index));
			}
		}
	}
}

/// The nearest enemy within `range` slots of an empty facing slot,
/// distance by distance with the left slot first. `None` when nothing
/// is in reach: a regiment facing empty ground far from the enemy's
/// line stands idle.
fn flank_target(front: &[Option<Regiment>], slot: usize, range: usize) -> Option<usize> {
	for distance in 1..=range {
		if let Some(left) = slot.checked_sub(distance)
			&& front[left].is_some()
		{
			return Some(left);
		}
		let right = slot + distance;
		if right < front.len() && front[right].is_some() {
			return Some(right);
		}
	}
	None
}

/// Apply one hit to a column: strength casualties to the front
/// regiment, morale damage to it and to the back-row regiment behind
/// it at the game's fractions.
fn strike(defender: &mut Line, slot: usize, strength_hit: f32, morale_hit: f32) {
	if let Some(regiment) = defender.front[slot].as_mut() {
		regiment.strength = (regiment.strength - strength_hit).max(0.0);
		regiment.morale -= morale_hit;
	}
	if let Some(regiment) = defender.back[slot].as_mut() {
		regiment.strength -= strength_hit * BACK_LINE_STRENGTH_DAMAGE_TAKEN_MODIFIER;
		regiment.morale -= morale_hit * BACK_LINE_MORALE_DAMAGE_TAKEN_MODIFIER;
	}
}

/// One day of one side's attacks. Every front regiment strikes the slot
/// directly across or - when that slot is empty - the nearest enemy
/// within its flanking range; back-row artillery adds reduced fire
/// straight ahead during fire phases. Our side strikes first each day.
fn exchange(
	defender: &mut Line,
	attacker: &Line,
	edge: f32,
	power: f32,
	fire: bool,
	flank: usize,
	defender_morale_scale: f32,
) {
	// Only a favorable die edge adds damage; a losing roll just hits
	// the base rates, like the lumped model.
	let advantage = edge.max(0.0);
	for slot in 0..attacker.front.len() {
		let Some(striker) = attacker.front[slot] else {
			continue;
		};
		let target = if defender.front[slot].is_some() {
			Some(slot)
		} else {
			flank_target(&defender.front, slot, striker.flank_range(flank))
		};
		if let Some(target) = target {
			// The stand-in daily damage table: base casualties plus a
			// die-edge term, scaled by the tactics power ratio and the
			// striker's remaining men.
			let strength_hit = (0.02 + 0.004 * advantage) * power * striker.strength;
			let morale_hit = (0.01 + 0.002 * advantage) * defender_morale_scale;
			strike(defender, target, strength_hit, morale_hit);
		}
		if fire
			&& let Some(gun) = attacker.back[slot]
			&& defender.front[slot].is_some()
		{
			let strength_hit =
				(0.02 + 0.004 * advantage) * power * BACK_LINE_MODIFIER * gun.strength;
			let morale_hit =
				(0.01 + 0.002 * advantage) * defender_morale_scale * BACK_LINE_MODIFIER;
			strike(defender, slot, strength_hit, morale_hit);
		}
	}
}

/// Resolve one whole battle on the slot lines, day by day, in
/// three-day fire and shock phases. Same contract and return shape as
/// [`crate::world::resolve_battle_full`]: war-score points (negated for
/// a defeat), enemy regiments destroyed, our losses, and the morale
/// fraction each army leaves the field with.
#[allow(clippy::too_many_arguments)]
pub fn resolve_battle_slots(
	rng: &mut Rng,
	ours: Army,
	enemy: Army,
	our_pips: Pips,
	their_pips: Pips,
	terrain: Terrain,
	we_attacked: bool,
	morale_scales: (f32, f32),
	width: usize,
	flank: usize,
) -> (f32, f32, f32, f32, f32) {
	let flank = flank.max(1);
	let our_start = ours.regiments();
	let their_start = enemy.regiments();
	// Per-side morale-damage scales from the modifier draws.
	let (our_morale_scale, their_morale_scale) = morale_scales;
	let mut our_line = Line::deploy(&split(ours), width);
	let mut their_line = Line::deploy(&split(enemy), width);

	let mut ours_won;
	let (mut our_die, mut their_die) = (0, 0);
	let mut day = 0u32;
	loop {
		ours_won =
			their_line.deployed_empty() || their_line.average_morale() <= LOW_MORALE_THRESHOLD;
		if ours_won
			|| our_line.deployed_empty()
			|| our_line.average_morale() <= LOW_MORALE_THRESHOLD
			|| day >= BATTLE_DAY_LIMIT
		{
			break;
		}
		// One die per side per phase, like the game.
		if day.is_multiple_of(DAYS_PER_PHASE) {
			our_die = rng.below(u64::from(COMBAT_DICE_SIDE)) as i32;
			their_die = rng.below(u64::from(COMBAT_DICE_SIDE)) as i32;
		}
		let fire = (day / DAYS_PER_PHASE).is_multiple_of(2);
		// The side that marched into the province is the attacker and
		// eats the ground's penalty; standing on hills and being
		// attacked is the point of choosing a battlefield.
		let (our_bonus, their_bonus) = if we_attacked {
			(terrain.attacker_pips, 0)
		} else {
			(0, terrain.attacker_pips)
		};
		let phase_pips = |pips: Pips| if fire { pips.fire } else { pips.shock };
		let our_total = our_die + phase_pips(our_pips) + our_bonus;
		let their_total = their_die + phase_pips(their_pips) + their_bonus;
		let edge = our_total - their_total;
		// The tactics ratio: the side with more army power behind its
		// line deals proportionally more, as in the lumped model.
		let our_power = our_line.power().max(0.1);
		let their_power = their_line.power().max(0.1);
		exchange(
			&mut their_line,
			&our_line,
			edge as f32,
			1.0 + 0.1 * our_power / their_power,
			fire,
			flank,
			their_morale_scale,
		);
		exchange(
			&mut our_line,
			&their_line,
			-edge as f32,
			1.0 + 0.1 * their_power / our_power,
			fire,
			flank,
			our_morale_scale,
		);
		our_line.daily_drain(our_morale_scale);
		their_line.daily_drain(their_morale_scale);
		our_line.sweep();
		our_line.reinforce();
		their_line.sweep();
		their_line.reinforce();
		day += 1;
	}
	let destroyed = (their_start - their_line.strength_sum()).max(0.0);
	let losses = (our_start - our_line.strength_sum()).max(0.0);
	let points = if ours_won {
		0.1 + destroyed / 20.0
	} else {
		-(0.1 + losses / 20.0)
	};
	(
		points,
		destroyed,
		losses,
		our_line.average_morale().max(0.0),
		their_line.average_morale().max(0.0),
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::world::PLAINS;

	fn army(infantry: f32, cavalry: f32, artillery: f32) -> Army {
		Army {
			infantry,
			cavalry,
			artillery,
			morale: 1.0,
		}
	}

	#[test]
	fn deployment_respects_width() {
		// Thirty regiments in a width-15 line: fifteen front slots, a
		// full back row of artillery behind them, the rest in reserve.
		let line = Line::deploy(&split(army(20.0, 4.0, 6.0)), 15);
		assert_eq!(line.front.iter().flatten().count(), 15);
		assert!(
			line.front
				.iter()
				.flatten()
				.all(|regiment| regiment.kind == Kind::Infantry)
		);
		assert_eq!(line.back.iter().flatten().count(), 6);
		assert!(
			line.back
				.iter()
				.flatten()
				.all(|regiment| regiment.kind == Kind::Artillery)
		);
		assert_eq!(line.reserve.len(), 9);
		assert_eq!(
			line.front.len() + line.back.len() + line.reserve.len(),
			15 + 15 + 9
		);
	}

	#[test]
	fn cavalry_sits_on_flanks_and_artillery_behind() {
		let line = Line::deploy(&split(army(10.0, 2.0, 2.0)), 15);
		// Twelve front regiments centered: seats 1 through 12.
		assert_eq!(line.front[1].map(|r| r.kind), Some(Kind::Cavalry));
		assert_eq!(line.front[12].map(|r| r.kind), Some(Kind::Cavalry));
		for slot in 2..12 {
			assert_eq!(line.front[slot].map(|r| r.kind), Some(Kind::Infantry));
		}
		assert!(line.front[0].is_none() && line.front[13].is_none() && line.front[14].is_none());
		// Artillery stands behind occupied front slots, left to right.
		assert_eq!(line.back[1].map(|r| r.kind), Some(Kind::Artillery));
		assert_eq!(line.back[2].map(|r| r.kind), Some(Kind::Artillery));
		assert_eq!(line.back.iter().flatten().count(), 2);
	}

	#[test]
	fn an_empty_facing_slot_lets_cavalry_flank_inward() {
		// A narrow enemy line centered in a width-15 field: seats 5-8.
		let mut narrow = vec![None; 15];
		narrow[5..9].fill(Some(Regiment {
			kind: Kind::Infantry,
			strength: 1.0,
			morale: 1.0,
		}));
		let infantry = Regiment {
			kind: Kind::Infantry,
			strength: 1.0,
			morale: 1.0,
		};
		// Infantry reaches one slot: the units bordering the line hit it.
		assert_eq!(flank_target(&narrow, 4, infantry.flank_range(2)), Some(5));
		assert_eq!(flank_target(&narrow, 9, infantry.flank_range(2)), Some(8));
		// Two slots out, infantry stands idle; cavalry reaches in.
		assert_eq!(flank_target(&narrow, 3, infantry.flank_range(2)), None);
		let cavalry = Regiment {
			kind: Kind::Cavalry,
			strength: 1.0,
			morale: 1.0,
		};
		assert_eq!(flank_target(&narrow, 3, cavalry.flank_range(2)), Some(5));
		assert_eq!(flank_target(&narrow, 10, cavalry.flank_range(2)), Some(8));
		// Far on the wing, even cavalry finds nothing in range.
		assert_eq!(flank_target(&narrow, 0, cavalry.flank_range(2)), None);
	}

	#[test]
	fn a_wider_line_bleeds_a_narrower_one() {
		// Flanking turns empty facing slots into extra attacks: a wide
		// line with cavalry destroys more of a narrow enemy than a
		// line the same width as the enemy does.
		let mut wide_total = 0.0;
		let mut even_total = 0.0;
		for seed in 0..64u64 {
			let mut rng = Rng::seed(seed);
			let (_, destroyed, _, _, _) = resolve_battle_slots(
				&mut rng,
				army(10.0, 4.0, 0.0),
				army(4.0, 0.0, 0.0),
				Pips { fire: 1, shock: 1 },
				Pips { fire: 1, shock: 1 },
				PLAINS,
				false,
				(1.0, 1.0),
				15,
				2,
			);
			wide_total += destroyed;
			let mut rng = Rng::seed(seed);
			let (_, destroyed, _, _, _) = resolve_battle_slots(
				&mut rng,
				army(4.0, 0.0, 0.0),
				army(4.0, 0.0, 0.0),
				Pips { fire: 1, shock: 1 },
				Pips { fire: 1, shock: 1 },
				PLAINS,
				false,
				(1.0, 1.0),
				15,
				2,
			);
			even_total += destroyed;
		}
		assert!(
			wide_total > even_total,
			"flanking must bleed the narrow line harder: {wide_total} vs {even_total}"
		);
	}

	#[test]
	fn battles_end_broken_and_finite() {
		for seed in 0..64u64 {
			for we_attacked in [false, true] {
				let mut rng = Rng::seed(seed);
				let (points, destroyed, losses, our_morale, their_morale) = resolve_battle_slots(
					&mut rng,
					army(10.0, 2.0, 2.0),
					army(8.0, 2.0, 1.0),
					Pips { fire: 2, shock: 2 },
					Pips { fire: 1, shock: 1 },
					PLAINS,
					we_attacked,
					(1.0, 1.0),
					15,
					2,
				);
				assert!(points.is_finite() && destroyed.is_finite() && losses.is_finite());
				assert!(our_morale.is_finite() && their_morale.is_finite());
				assert!(destroyed >= 0.0 && losses >= 0.0);
				assert!((0.0..=1.0).contains(&our_morale));
				assert!((0.0..=1.0).contains(&their_morale));
				// The beaten side leaves at or below the shatter
				// threshold - an emptied line reports no morale at all.
				if points > 0.0 {
					assert!(
						their_morale <= LOW_MORALE_THRESHOLD,
						"seed {seed}: a victory must leave the enemy broken"
					);
					assert!((points - (0.1 + destroyed / 20.0)).abs() < 1e-4);
				} else {
					assert!(
						our_morale <= LOW_MORALE_THRESHOLD,
						"seed {seed}: a defeat must leave us broken"
					);
					assert!((points + (0.1 + losses / 20.0)).abs() < 1e-4);
				}
			}
		}
	}
}
