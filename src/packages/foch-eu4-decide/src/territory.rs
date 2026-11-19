//! A territorial war: provinces owned, besieged, captured, and retaken.

use crate::combat::resolve_battle_slots;
use crate::world::{
	Army, CANDIDATE_LIMIT, DAYS_PER_PROVINCE, DAYS_PER_SIEGE_PHASE, DefineDraw, FOREST, HILLS,
	MARSH, MOUNTAINS, Modifiers, PLAINS, Pips, Rng, SIEGE_WIN, Terrain, march_days,
};

pub use crate::world::Edge;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EdgeKind {
	Land,
	Strait { width: u32 },
	Pass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
	Ours,
	Theirs,
	Neutral,
}

impl Side {
	pub fn flip(self) -> Self {
		match self {
			Self::Ours => Self::Theirs,
			Self::Theirs => Self::Ours,
			Self::Neutral => Self::Neutral,
		}
	}
}

#[derive(Clone, Debug)]
pub struct Province {
	pub terrain: Terrain,
	pub development: i32,
	pub fort_level: i32,
	pub original_owner: Side,
	pub controller: Side,
	pub x: f32,
	pub y: f32,
}

#[derive(Clone, Debug)]
pub struct TerritoryMap {
	pub provinces: Vec<Province>,
	pub edges: Vec<Vec<(u32, u32, EdgeKind)>>,
	/// Each side's capital, as an index into `provinces`. On real-map
	/// slices these are the nations' actual capitals (read from the
	/// game's country files) whenever they made the slice; on synthetic
	/// maps a uniformly random province of that side — a capital can sit
	/// anywhere in a nation's land, not only its geometric heart.
	pub our_capital: u32,
	pub their_capital: u32,
}

impl TerritoryMap {
	pub fn generate(seed: u64) -> Self {
		let mut rng = Rng::seed(seed);
		let count = 12 + (rng.below(6) as usize);
		// Points in pixel space, like the real map's province positions,
		// so node features share one scale with real slices.
		let points: Vec<(f32, f32)> = (0..count)
			.map(|_| {
				(
					1000.0 + rng.below(3600) as f32,
					1000.0 + rng.below(3600) as f32,
				)
			})
			.collect();
		let distance = |a: usize, b: usize| -> f32 {
			let (dx, dy) = (points[a].0 - points[b].0, points[a].1 - points[b].1);
			(dx * dx + dy * dy).sqrt()
		};
		// A mesh: each province links to 3-5 of its nearest neighbors,
		// the density of a real border region (avg degree ~6, no funnels).
		let mut edges: Vec<Vec<(u32, u32, EdgeKind)>> = vec![Vec::new(); count];
		fn link(edges: &mut [Vec<(u32, u32, EdgeKind)>], a: usize, b: usize, kind: EdgeKind) {
			if a == b || edges[a].iter().any(|(to, _, _)| *to as usize == b) {
				return;
			}
			edges[a].push((b as u32, DAYS_PER_PROVINCE, kind));
			edges[b].push((a as u32, DAYS_PER_PROVINCE, kind));
		}
		for i in 0..count {
			let mut near: Vec<(f32, usize)> = (0..count)
				.filter(|&j| j != i)
				.map(|j| (distance(i, j), j))
				.collect();
			near.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
			let k = 3 + rng.below(3) as usize;
			for (_, j) in near.iter().take(k) {
				link(&mut edges, i, *j, EdgeKind::Land);
			}
		}
		// Stitch any leftover components together by nearest pair.
		loop {
			let mut component = vec![u32::MAX; count];
			let mut components = 0;
			for start in 0..count {
				if component[start] != u32::MAX {
					continue;
				}
				let id = components;
				components += 1;
				component[start] = id;
				let mut frontier = vec![start];
				while let Some(current) = frontier.pop() {
					for (next, _, _) in &edges[current] {
						if component[*next as usize] == u32::MAX {
							component[*next as usize] = id;
							frontier.push(*next as usize);
						}
					}
				}
			}
			if components <= 1 {
				break;
			}
			let mut best: Option<(f32, usize, usize)> = None;
			for a in 0..count {
				for b in a + 1..count {
					if component[a] != component[b] {
						let d = distance(a, b);
						if best.is_none_or(|(bd, _, _)| d < bd) {
							best = Some((d, a, b));
						}
					}
				}
			}
			if let Some((_, a, b)) = best {
				link(&mut edges, a, b, EdgeKind::Land);
			}
		}
		// Ownership: the two farthest points are the poles of the two
		// nations' land; provinces sort by relative distance to them,
		// the contested middle stays neutral — the same shape as a real
		// slice.
		let mut poles = (0usize, 1usize);
		let mut best = 0.0f32;
		for a in 0..count {
			for b in a + 1..count {
				let d = distance(a, b);
				if d > best {
					best = d;
					poles = (a, b);
				}
			}
		}
		let mut order: Vec<usize> = (0..count).collect();
		order.sort_by(|&i, &j| {
			(distance(i, poles.1) - distance(i, poles.0))
				.partial_cmp(&(distance(j, poles.1) - distance(j, poles.0)))
				.unwrap_or(std::cmp::Ordering::Equal)
		});
		// Ownership by rank of relative capital distance; provinces stay
		// indexed by their original point index so edges match.
		let mut owner = vec![Side::Neutral; count];
		for (rank, &i) in order.iter().enumerate() {
			owner[i] = if rank < count / 3 {
				Side::Ours
			} else if rank >= count * 2 / 3 {
				Side::Theirs
			} else {
				Side::Neutral
			};
		}
		let mut provinces = Vec::with_capacity(count);
		for i in 0..count {
			// Development skewed low like real provinces, with the odd
			// rich one.
			let development = 3
				+ rng.below(10) as i32
				+ if rng.below(5) == 0 {
					rng.below(25) as i32
				} else {
					0
				};
			provinces.push(Province {
				terrain: [PLAINS, FOREST, HILLS, MOUNTAINS, MARSH][rng.below(5) as usize],
				development,
				fort_level: if owner[i] != Side::Neutral {
					1 + rng.below(2) as i32
				} else {
					0
				},
				original_owner: owner[i],
				controller: owner[i],
				x: points[i].0,
				y: points[i].1,
			});
		}
		// One strait: the widest non-adjacent gap across the frontline.
		let mut strait: Option<(f32, usize, usize)> = None;
		for a in 0..count {
			for b in 0..count {
				if owner[a] != Side::Ours || owner[b] != Side::Theirs {
					continue;
				}
				if edges[a].iter().any(|(to, _, _)| *to as usize == b) {
					continue;
				}
				let d = distance(a, b);
				if strait.is_none_or(|(bd, _, _)| d < bd) {
					strait = Some((d, a, b));
				}
			}
		}
		if let Some((_, a, b)) = strait {
			link(
				&mut edges,
				a,
				b,
				EdgeKind::Strait {
					width: DAYS_PER_PROVINCE,
				},
			);
		}
		// Re-cost land edges from terrain (link() used a flat cost).
		for a in 0..count {
			for slot in 0..edges[a].len() {
				let (to, _, kind) = edges[a][slot];
				if let EdgeKind::Land = kind {
					let days = march_days(provinces[a].terrain)
						.max(march_days(provinces[to as usize].terrain));
					edges[a][slot].1 = days;
					for other in 0..edges[to as usize].len() {
						if edges[to as usize][other].0 as usize == a {
							edges[to as usize][other].1 = days;
							break;
						}
					}
				}
			}
		}
		// Capitals: a uniformly random province of each side — any of a
		// nation's provinces can be its seat, rich or central or not.
		let mut pick_capital = |side: Side| -> u32 {
			let owned: Vec<u32> = provinces
				.iter()
				.enumerate()
				.filter(|(_, p)| p.original_owner == side)
				.map(|(i, _)| i as u32)
				.collect();
			owned[rng.below(owned.len() as u64) as usize]
		};
		let our_capital = pick_capital(Side::Ours);
		let their_capital = pick_capital(Side::Theirs);
		Self {
			provinces,
			edges,
			our_capital,
			their_capital,
		}
	}

	pub fn travel_days(&self, from: u32, to: u32, side: Side) -> u32 {
		self.travel_days_all(from, side)[to as usize]
	}

	/// Days from `from` to every province at once — the AI targets with
	/// one pass instead of a search per candidate.
	pub fn travel_days_all(&self, from: u32, side: Side) -> Vec<u32> {
		let mut best = vec![u32::MAX; self.provinces.len()];
		best[from as usize] = 0;
		let mut frontier = vec![from];
		while let Some(current) = frontier.pop() {
			let days = best[current as usize];
			for (next, cost, kind) in &self.edges[current as usize] {
				if let EdgeKind::Strait { .. } = kind {
					let a = self.provinces[current as usize].controller;
					let b = self.provinces[*next as usize].controller;
					if a != side && b != side {
						continue;
					}
				}
				let total = days.saturating_add(*cost);
				if total < best[*next as usize] {
					best[*next as usize] = total;
					frontier.push(*next);
				}
			}
		}
		best
	}

	pub fn path(&self, from: u32, to: u32, side: Side) -> Vec<u32> {
		if from == to {
			return Vec::new();
		}
		let mut best = vec![u32::MAX; self.provinces.len()];
		let mut came = vec![None; self.provinces.len()];
		best[from as usize] = 0;
		let mut frontier = vec![from];
		while let Some(current) = frontier.pop() {
			let days = best[current as usize];
			for (next, cost, kind) in &self.edges[current as usize] {
				if let EdgeKind::Strait { .. } = kind {
					let a = self.provinces[current as usize].controller;
					let b = self.provinces[*next as usize].controller;
					if a != side && b != side {
						continue;
					}
				}
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

	pub fn held_by(&self, side: Side) -> Vec<u32> {
		self.provinces
			.iter()
			.enumerate()
			.filter(|(_, p)| p.controller == side)
			.map(|(i, _)| i as u32)
			.collect()
	}

	pub fn supply_at(&self, province: u32, side: Side) -> f32 {
		let p = &self.provinces[province as usize];
		let base = if p.controller == side { 4.0 } else { 0.0 };
		base + p.development as f32 * if p.controller == side { 1.0 } else { 0.3 }
	}

	/// Whether `side` can see a position: standing in any province it
	/// controls, or in one of that province's neighbors.
	pub fn sees_position(&self, pos: u32, side: Side) -> bool {
		for (i, p) in self.provinces.iter().enumerate() {
			if p.controller == side {
				if i as u32 == pos {
					return true;
				}
				for (n, _, _) in &self.edges[i] {
					if *n == pos {
						return true;
					}
				}
			}
		}
		false
	}
}

/// Warscore charged for bleeding the whole mobilization pool dry.
const BLOOD_PRICE: f32 = 20.0;
/// Cap on war exhaustion (`MAX_WAR_EXHAUSTION`).
const MAX_WAR_EXHAUSTION: f32 = 20.0;
/// The whole EU4 province graph, loaded once from the game's own data
/// in a flat whitespace format (see `_province_pool.py`). Slices of it
/// become war maps with the real adjacency, development, positions, and
/// straits.
pub struct ProvincePool {
	pub development: Vec<i32>,
	pub owner: Vec<String>,
	pub x: Vec<f32>,
	pub y: Vec<f32>,
	/// Neighbor indices into this pool.
	pub neighbors: Vec<Vec<usize>>,
	/// Strait edges as index pairs.
	pub straits: Vec<(usize, usize)>,
	/// The largest connected component, so slices stay connected.
	pub largest: Vec<usize>,
	/// Each nation's actual capital, read from the game's country
	/// history files: tag -> province. The value is the pool index when
	/// the capital province made the pool, else the raw province id,
	/// which no slice member matches.
	pub capitals: std::collections::HashMap<String, u32>,
}

impl ProvincePool {
	pub fn load(text: &str) -> Self {
		// (id, development, owner, x, y, raw neighbor ids)
		let mut raw: Vec<(u32, i32, String, f32, f32, Vec<u32>)> = Vec::new();
		let mut raw_straits: Vec<(u32, u32)> = Vec::new();
		let mut raw_capitals: Vec<(String, u32)> = Vec::new();
		let mut in_straits = false;
		let mut in_capitals = false;
		for line in text.lines() {
			let words: Vec<&str> = line.split_whitespace().collect();
			match words.first() {
				Some(&"PROVINCE_POOL") => continue,
				Some(&"STRAITS") => in_straits = true,
				Some(&"CAPITALS") => {
					in_straits = false;
					in_capitals = true;
					continue;
				}
				_ => {}
			}
			if words.is_empty() {
				continue;
			}
			if in_capitals {
				if words.len() >= 2
					&& let Ok(id) = words[1].parse::<u32>()
				{
					raw_capitals.push((words[0].to_string(), id));
				}
				continue;
			}
			if in_straits {
				if let (Ok(a), Ok(b)) = (words[0].parse::<u32>(), words[1].parse::<u32>()) {
					raw_straits.push((a, b));
				}
				continue;
			}
			if words.len() < 5 {
				continue;
			}
			let (Ok(id), Ok(development)) = (words[0].parse::<u32>(), words[1].parse::<i32>())
			else {
				continue;
			};
			raw.push((
				id,
				development,
				words[2].to_string(),
				words[3].parse().unwrap_or(0.0),
				words[4].parse().unwrap_or(0.0),
				words[5..]
					.iter()
					.filter_map(|w| w.parse::<u32>().ok())
					.collect(),
			));
		}
		let index_of: std::collections::HashMap<u32, usize> =
			raw.iter().enumerate().map(|(i, r)| (r.0, i)).collect();
		let neighbors: Vec<Vec<usize>> = raw
			.iter()
			.map(|r| {
				r.5.iter()
					.filter_map(|id| index_of.get(id).copied())
					.collect()
			})
			.collect();
		let straits: Vec<(usize, usize)> = raw_straits
			.iter()
			.filter_map(|(a, b)| Some((*index_of.get(a)?, *index_of.get(b)?)))
			.collect();
		let capitals: std::collections::HashMap<String, u32> = raw_capitals
			.into_iter()
			.map(|(tag, id)| (tag, index_of.get(&id).map_or(id, |&i| i as u32)))
			.collect();
		// The largest connected component, by flood fill.
		let mut seen = vec![false; raw.len()];
		let mut largest = Vec::new();
		for start in 0..raw.len() {
			if seen[start] {
				continue;
			}
			let mut component = vec![start];
			seen[start] = true;
			let mut frontier = vec![start];
			while let Some(current) = frontier.pop() {
				for &next in &neighbors[current] {
					if !seen[next] {
						seen[next] = true;
						component.push(next);
						frontier.push(next);
					}
				}
			}
			if component.len() > largest.len() {
				largest = component;
			}
		}
		largest.sort_unstable();
		Self {
			development: raw.iter().map(|r| r.1).collect(),
			owner: raw.iter().map(|r| r.2.clone()).collect(),
			x: raw.iter().map(|r| r.3).collect(),
			y: raw.iter().map(|r| r.4).collect(),
			neighbors,
			straits,
			largest,
			capitals,
		}
	}

	/// A random connected slice of the real map around a real border:
	/// a random edge whose endpoints belong to two different nations
	/// (as the game's own history owns them) seeds a BFS that grows
	/// over those two nations' land and the unowned ground between
	/// them — A's provinces are ours, B's are theirs, no man's land is
	/// neutral. If the two nations' reachable land runs out before the
	/// slice is full, remaining provinces join in BFS order as neutral
	/// ground. Capitals are the nations' real ones (from the CAPITALS
	/// section) whenever they made the slice. A pool without owned
	/// provinces falls back to any edge and the distance-based split
	/// of the synthetic generator. Terrain is drawn from the vanilla
	/// distribution until terrain.bmp wiring lands.
	pub fn slice(&self, rng: &mut Rng, count: usize) -> TerritoryMap {
		let count = count.max(4);
		let unowned = |tag: &str| tag.is_empty() || tag == "NONE";
		// The real borders: edges whose endpoints belong to different
		// nations, kept inside the largest component so the BFS behind
		// them can always grow the slice to full size.
		let in_largest: std::collections::HashSet<usize> = self.largest.iter().copied().collect();
		let mut borders: Vec<(usize, usize)> = Vec::new();
		for a in 0..self.owner.len() {
			if unowned(&self.owner[a]) || !in_largest.contains(&a) {
				continue;
			}
			for &b in &self.neighbors[a] {
				if a < b
					&& in_largest.contains(&b)
					&& !unowned(&self.owner[b])
					&& self.owner[a] != self.owner[b]
				{
					borders.push((a, b));
				}
			}
		}
		// A random border edge (deterministic from the rng): nation A at
		// one end, nation B at the other.
		let (seed_a, seed_b, tag_a, tag_b) =
			if let Some(&(a, b)) = borders.get(rng.below(borders.len() as u64) as usize) {
				(a, b, self.owner[a].as_str(), self.owner[b].as_str())
			} else {
				// No borders in the pool: any edge (or lone province)
				// keeps it slicing; sides fall back to the distance
				// split below.
				let start = self.largest[rng.below(self.largest.len() as u64) as usize];
				let next = self.neighbors[start]
					.get(rng.below(self.neighbors[start].len().max(1) as u64) as usize)
					.copied()
					.unwrap_or(start);
				(start, next, "", "")
			};
		let real_nations = !unowned(tag_a) && !unowned(tag_b) && tag_a != tag_b;
		// BFS outward from the border edge in insertion order
		// (deterministic per rng), over A's, B's, and unowned land.
		let mut members: Vec<usize> = vec![seed_a];
		let mut member_of: std::collections::HashMap<usize, usize> =
			std::collections::HashMap::new();
		member_of.insert(seed_a, 0);
		if seed_b != seed_a {
			member_of.insert(seed_b, 1);
			members.push(seed_b);
		}
		let mut head = 0;
		while members.len() < count && head < members.len() {
			let current = members[head];
			head += 1;
			for &next in &self.neighbors[current] {
				if member_of.contains_key(&next) {
					continue;
				}
				let tag = &self.owner[next];
				let eligible = !real_nations || tag == tag_a || tag == tag_b || unowned(tag);
				if eligible {
					member_of.insert(next, members.len());
					members.push(next);
					if members.len() >= count {
						break;
					}
				}
			}
		}
		// Top-up: A's and B's reachable land ran out — take whatever is
		// left in BFS order; those provinces fight as neutral ground.
		if members.len() < count {
			let mut head = 0;
			while members.len() < count && head < members.len() {
				let current = members[head];
				head += 1;
				for &next in &self.neighbors[current] {
					if let std::collections::hash_map::Entry::Vacant(entry) = member_of.entry(next)
					{
						entry.insert(members.len());
						members.push(next);
						if members.len() >= count {
							break;
						}
					}
				}
			}
		}
		// Sides from the real owners: A's land is ours, B's is theirs,
		// everything else neutral.
		let mut sides: Vec<Side> = members
			.iter()
			.map(|&m| {
				let tag = &self.owner[m];
				if real_nations && tag == tag_a {
					Side::Ours
				} else if real_nations && tag == tag_b {
					Side::Theirs
				} else {
					Side::Neutral
				}
			})
			.collect();
		if !real_nations {
			// Hop distances from a province, within the slice.
			let dist_from = |root: usize, members: &[usize]| -> Vec<u32> {
				let mut dist = vec![u32::MAX; members.len()];
				dist[member_of[&root]] = 0;
				let mut frontier = std::collections::VecDeque::from(vec![member_of[&root]]);
				while let Some(current) = frontier.pop_front() {
					for &next in &self.neighbors[members[current]] {
						if let Some(&position) = member_of.get(&next)
							&& dist[position] == u32::MAX
						{
							dist[position] = dist[current] + 1;
							frontier.push_back(position);
						}
					}
				}
				dist
			};
			// Split by relative distance to the seed provinces, as the
			// synthetic generator does.
			let dist_a = dist_from(seed_a, &members);
			let dist_b = dist_from(seed_b, &members);
			let mut order: Vec<usize> = (0..members.len()).collect();
			order.sort_by_key(|&i| dist_a[i].saturating_sub(dist_b[i]));
			for (rank, &i) in order.iter().enumerate() {
				sides[i] = if rank < order.len() / 3 {
					Side::Ours
				} else if rank >= order.len() * 2 / 3 {
					Side::Theirs
				} else {
					Side::Neutral
				};
			}
		}
		let mut provinces = Vec::with_capacity(members.len());
		for (i, &m) in members.iter().enumerate() {
			let owner = sides[i];
			let terrain = [PLAINS, FOREST, HILLS, MOUNTAINS, MARSH][rng.below(5) as usize];
			provinces.push(Province {
				terrain,
				development: self.development[m].clamp(1, 40),
				fort_level: if owner != Side::Neutral {
					1 + rng.below(2) as i32
				} else {
					0
				},
				original_owner: owner,
				controller: owner,
				x: self.x[m],
				y: self.y[m],
			});
		}
		let strait_pair: std::collections::HashSet<(usize, usize)> = self
			.straits
			.iter()
			.filter_map(|(a, b)| match (member_of.get(a), member_of.get(b)) {
				(Some(&a), Some(&b)) => Some((a.min(b), a.max(b))),
				_ => None,
			})
			.collect();
		// Pool index -> slice index, so straits and neighbor lists (pool
		// indices) resolve against the same space.
		let mut edges: Vec<Vec<(u32, u32, EdgeKind)>> = vec![Vec::new(); provinces.len()];
		for (a, &m) in members.iter().enumerate() {
			for &next in &self.neighbors[m] {
				let Some(&b) = member_of.get(&next) else {
					continue;
				};
				if a == b || edges[a].iter().any(|(to, _, _)| *to as usize == b) {
					continue;
				}
				let key = (a.min(b), a.max(b));
				let kind = if strait_pair.contains(&key) {
					EdgeKind::Strait {
						width: DAYS_PER_PROVINCE,
					}
				} else {
					EdgeKind::Land
				};
				let days = match kind {
					EdgeKind::Land => {
						march_days(provinces[a].terrain).max(march_days(provinces[b].terrain))
					}
					EdgeKind::Strait { width } => width,
					EdgeKind::Pass => march_days(MOUNTAINS) * 2,
				};
				edges[a].push((b as u32, days, kind));
				edges[b].push((a as u32, days, kind));
			}
		}
		// Capitals: the nations' real ones when they made the slice.
		let mut side_capital = |tag: &str, side: Side| -> u32 {
			if real_nations
				&& let Some(&capital) = self.capitals.get(tag)
				&& let Some(&at) = member_of.get(&(capital as usize))
			{
				return at as u32;
			}
			// Fallback: a real nation's capital can sit outside this
			// front, so any of its provinces here stands in — a random
			// owned province of the side, drawn from the same rng.
			let owned: Vec<usize> = sides
				.iter()
				.enumerate()
				.filter(|&(_, side_of)| *side_of == side)
				.map(|(i, _)| i)
				.collect();
			owned
				.get(rng.below(owned.len().max(1) as u64) as usize)
				.copied()
				.unwrap_or(0) as u32
		};
		let our_capital = side_capital(tag_a, Side::Ours);
		let their_capital = side_capital(tag_b, Side::Theirs);
		TerritoryMap {
			provinces,
			edges,
			our_capital,
			their_capital,
		}
	}
}

/// One AI-led field army on the map, enemy or allied: its strength, its
/// position, the march it is executing, and whether it is in shattered
/// retreat.
#[derive(Clone, Debug)]
pub struct FrontArmy {
	pub army: Army,
	pub at: u32,
	pub path: Vec<u32>,
	pub hop_days: u32,
	pub fleeing: bool,
}

#[derive(Clone)]
pub struct WarCampaign {
	pub map: TerritoryMap,
	pub defines: DefineDraw,
	pub day: u32,
	pub horizon: u32,
	pub our_army: Army,
	pub our_at: u32,
	pub our_path: Vec<u32>,
	pub our_hop_days: u32,
	pub our_wait: u32,
	pub our_pips: Pips,
	/// The enemy's field stacks, all fighting under `enemy_pips`.
	pub enemies: Vec<FrontArmy>,
	/// Allied stacks fighting on our side under `ally_pips`.
	pub allies: Vec<FrontArmy>,
	/// The enemy general's pips, shared by every enemy stack.
	pub enemy_pips: Pips,
	/// Which side declared the war — alternates by scenario.
	pub aggressor: Side,
	/// The war goal for conquest wars: one specific province claimed
	/// by the aggressor (`None` for show-superity wars, where battles
	/// themselves tick the score). The capital is a different thing
	/// entirely.
	pub wargoal: Option<u32>,
	/// Each side's capital: the seat of its land, priced higher in
	/// peace deals (`PEACE_COST_DEMAND_CAPITAL_MULT`). Not necessarily
	/// rich.
	pub our_capital: u32,
	pub their_capital: u32,
	/// The allied general's pips, shared by every allied stack.
	pub ally_pips: Pips,
	pub siege_progress: Vec<i32>,
	pub war_score: f32,
	/// Every regiment this general's own army has lost, battle and
	/// attrition alike. Priced against the force the general was
	/// given: burning through your own army is bad generalship no
	/// matter how deep the nation's manpower sits behind it. In the
	/// DLL this is read from the commanded army's actual strength.
	pub our_losses: f32,
	/// The force at mobilization: the denominator of the blood price.
	pub our_force_full: f32,
	/// War exhaustion from a dragging war: 0.05 per month, capped at
	/// `MAX_WAR_EXHAUSTION` (20). Its penalty lands on the army, like
	/// the engine's — morale recovers and regiments reinforce more
	/// slowly as the nation tires. It never touches the score.
	pub exhaustion: f32,
	pub provinces_taken: u32,
	pub provinces_lost: u32,
	pub battles_won: u32,
	pub battles_lost: u32,
	/// Enemy stacks destroyed outright — decisive moments that leave
	/// every remaining province unguarded. Nothing raises them back;
	/// reinforcing standing regiments is the player's merge.
	pub wipes: u32,
	pub decision_pending: bool,
	/// The day full occupation was achieved, if it was. The 5-year
	/// timer from this point drives warscore to 100.
	pub full_occupation_since: Option<u32>,
	/// Manpower each side can still spend on regiments this war: a
	/// fraction of the nation's total development at mobilization.
	pub our_manpower: f32,
	pub enemy_manpower: f32,
	/// The pools at mobilization. Reinforcement slows as the pool
	/// empties, so a war of attrition is winnable: bleed the enemy
	/// faster than his provinces refill the pool and his army stops
	/// healing.
	pub our_manpower_full: f32,
	pub enemy_manpower_full: f32,
	/// Whether our army marched into the current meeting province; the
	/// mover is the attacker and eats the ground's combat penalty.
	pub attacker_ours: bool,
	/// The day our army last completed a march. Catching a retreating
	/// enemy destroys it only on that day and the next: marching onto a
	/// broken stack kills it, standing by while it withdraws does not.
	pub our_arrival_day: u32,
	/// Our army is in shattered retreat: it runs home invulnerable and
	/// takes no orders until it arrives.
	pub our_fleeing: bool,
	/// Where we stood before the last completed march — the player
	/// obviously knows their own last position; the policy sees it so it
	/// can learn not to dither between two provinces.
	pub our_last: u32,
	pub our_modifiers: Modifiers,
	pub enemy_modifiers: Modifiers,
}

impl WarCampaign {
	pub fn new(seed: u64, personality: &str) -> Self {
		Self::new_on_map(TerritoryMap::generate(seed), seed, personality)
	}

	/// A war on a given map — synthetic or a slice of the real game's
	/// province graph.
	pub fn new_on_map(map: TerritoryMap, seed: u64, personality: &str) -> Self {
		let our_side = map.held_by(Side::Ours);
		let their_side = map.held_by(Side::Theirs);
		// Mobilization pools: the manpower a nation can still spend this
		// war, from the development of its provinces.
		let our_manpower: f32 = map
			.provinces
			.iter()
			.filter(|p| p.original_owner == Side::Ours)
			.map(|p| p.development as f32)
			.sum::<f32>()
			* 0.25;
		let enemy_manpower: f32 = map
			.provinces
			.iter()
			.filter(|p| p.original_owner == Side::Theirs)
			.map(|p| p.development as f32)
			.sum::<f32>()
			* 0.25;
		// Two enemy stacks on their own ground, one allied stack on ours —
		// the player's own army fights beside the ally, not through it.
		let field_army = |at: u32, infantry: f32| FrontArmy {
			army: Army {
				infantry,
				cavalry: 1.0,
				artillery: 2.0,
				morale: 1.0,
			},
			at,
			path: Vec::new(),
			hop_days: 0,
			fleeing: false,
		};
		let enemies = their_side
			.iter()
			.take(2)
			.map(|&at| field_army(at, 10.0))
			.collect();
		let allies = vec![field_army(our_side.first().copied().unwrap_or(0), 6.0)];
		// The war alternates: half the scenarios the enemy declares on us.
		let aggressor = if seed.is_multiple_of(2) {
			Side::Ours
		} else {
			Side::Theirs
		};
		let defender = aggressor.flip();
		// Capitals come off the map: the nations' real ones on sliced
		// maps, a random owned province on synthetic ones.
		let our_capital = map.our_capital;
		let their_capital = map.their_capital;
		// The claim: two wars in three are conquest of one specific
		// defender province; the rest are show-superity wars with no
		// goal province at all.
		let mut claim_rng = Rng::seed(seed ^ 0xc1a1);
		let wargoal = if claim_rng.below(3) < 2 {
			let claims: Vec<u32> = map
				.provinces
				.iter()
				.enumerate()
				.filter(|(_, p)| p.original_owner == defender)
				.map(|(i, _)| i as u32)
				.collect();
			Some(claims[claim_rng.below(claims.len() as u64) as usize])
		} else {
			None
		};
		Self {
			defines: DefineDraw::draw(seed),
			siege_progress: vec![0; map.provinces.len()],
			our_at: our_side.first().copied().unwrap_or(0),
			map,
			day: 0,
			horizon: 3600,
			our_army: Army {
				infantry: 10.0,
				cavalry: 2.0,
				artillery: 2.0,
				morale: 1.0,
			},
			our_path: Vec::new(),
			our_hop_days: 0,
			our_wait: 0,
			our_pips: Pips::of_personality(personality),
			enemies,
			allies,
			aggressor,
			wargoal,
			our_capital,
			their_capital,
			enemy_pips: Pips { fire: 1, shock: 1 },
			ally_pips: Pips { fire: 1, shock: 1 },
			war_score: 0.0,
			our_losses: 0.0,
			our_force_full: 14.0,
			exhaustion: 0.0,
			provinces_taken: 0,
			provinces_lost: 0,
			battles_won: 0,
			battles_lost: 0,
			wipes: 0,

			decision_pending: true,
			full_occupation_since: None,
			our_manpower,
			enemy_manpower,
			our_manpower_full: our_manpower.max(1.0),
			enemy_manpower_full: enemy_manpower.max(1.0),

			attacker_ours: true,
			our_arrival_day: 0,
			our_fleeing: false,
			our_last: 0,
			our_modifiers: Modifiers::draw(seed),
			enemy_modifiers: Modifiers::draw(seed.wrapping_mul(0x2222)),
		}
	}

	pub fn order(&mut self, destination: u32) {
		if self.our_fleeing {
			return; // shattered retreat cannot be redirected
		}
		self.decision_pending = false;
		if destination == self.our_at {
			self.our_wait = DAYS_PER_SIEGE_PHASE;
			self.our_path.clear();
			self.our_hop_days = 0;
			return;
		}
		self.our_path = self.map.path(self.our_at, destination, Side::Ours);
		if let Some(&first) = self.our_path.first() {
			self.our_hop_days = self.edge_days(self.our_at, first);
		}
	}

	fn edge_days(&self, from: u32, to: u32) -> u32 {
		self.map.edges[from as usize]
			.iter()
			.find(|(n, _, _)| *n == to)
			.map(|(_, d, _)| *d)
			.unwrap_or(DAYS_PER_PROVINCE)
	}

	pub fn candidates(&self) -> Vec<u32> {
		let mut picked = vec![self.our_at];
		for (next, _, _) in &self.map.edges[self.our_at as usize] {
			if !picked.contains(next) && picked.len() < CANDIDATE_LIMIT {
				picked.push(*next);
			}
		}
		for province in self.map.held_by(Side::Ours) {
			if !picked.contains(&province) && picked.len() < CANDIDATE_LIMIT {
				picked.push(province);
			}
		}
		picked
	}

	#[allow(clippy::collapsible_if)]
	pub fn step_day(&mut self, rng: &mut Rng) {
		if self.decision_pending || self.finished() {
			return;
		}
		self.day += 1;
		for index in 0..self.enemies.len() {
			if self.our_army.regiments() < 1.0 {
				break;
			}
			let stack = &self.enemies[index];
			if self.our_at != stack.at || stack.army.regiments() < 1.0 {
				continue;
			}
			// Marching onto an army in shattered retreat destroys it:
			// broken morale, no order to fight.
			if stack.fleeing && self.day <= self.our_arrival_day + 1 {
				let destroyed = stack.army.regiments();
				self.war_score += 0.1 + destroyed / 20.0;
				self.battles_won += 1;
				self.wipe_enemy(index);
			} else {
				self.fight(rng, index);
			}
		}
		if self.our_hop_days > 0 {
			self.our_hop_days -= 1;
			if self.our_hop_days == 0 {
				if let Some(next) = self.our_path.first().copied() {
					self.our_path.remove(0);
					self.our_last = self.our_at;
					self.our_at = next;
					if self.our_fleeing && self.our_path.is_empty() {
						self.our_fleeing = false;
					}
					self.attacker_ours = true;
					self.our_arrival_day = self.day;
					self.arrive(next);
				}
			}
		} else if self.our_wait > 0 {
			self.our_wait -= 1;
			if self.our_wait == 0 {
				if self.our_fleeing {
					return;
				}
				let pips = self.our_army.artillery.min(5.0) as i32 + self.our_modifiers.siege_pips;
				self.besiege_step(rng, self.our_at, Side::Ours, pips);
				if self.map.provinces[self.our_at as usize].controller == Side::Ours {
					self.decision_pending = !self.our_fleeing;
				} else {
					self.our_wait = DAYS_PER_SIEGE_PHASE;
				}
			}
		} else {
			self.decision_pending = !self.our_fleeing;
		}
		// The allied and enemy stacks move under their own AI, then any
		// that share a province fight it out.
		for index in 0..self.allies.len() {
			self.front_ai(rng, index, Side::Ours);
		}
		let mut enemy_arrived = vec![false; self.enemies.len()];
		for (index, arrived) in enemy_arrived.iter_mut().enumerate() {
			*arrived = self.front_ai(rng, index, Side::Theirs);
		}
		self.ai_battles(rng, &enemy_arrived);
		// A siege is lifted when no besieger stands on the province —
		// the engine resets progress when the besieging army leaves.
		for i in 0..self.map.provinces.len() {
			if self.siege_progress[i] == 0 {
				continue;
			}
			let controller = self.map.provinces[i].controller;
			let our_siege = controller != Side::Ours
				&& ((self.our_at == i as u32 && self.our_army.regiments() >= 1.0)
					|| self
						.allies
						.iter()
						.any(|s| s.at == i as u32 && s.army.regiments() >= 1.0));
			let their_siege = controller == Side::Ours
				&& self
					.enemies
					.iter()
					.any(|s| s.at == i as u32 && s.army.regiments() >= 1.0);
			if !our_siege && !their_siege {
				self.siege_progress[i] = 0;
			}
		}
		// Track full occupation for the 5-year timer.
		let their_remaining = self.map.held_by(Side::Theirs).len()
			+ self
				.map
				.provinces
				.iter()
				.filter(|p| p.controller == Side::Neutral && p.original_owner == Side::Theirs)
				.count();
		if their_remaining == 0 {
			if self.full_occupation_since.is_none() {
				self.full_occupation_since = Some(self.day);
			}
		} else {
			self.full_occupation_since = None;
		}
		if self.day.is_multiple_of(DAYS_PER_SIEGE_PHASE) {
			self.monthly_recovery();
			let supply = self.map.supply_at(self.our_at, Side::Ours);
			let excess = (self.our_army.regiments() - supply).max(0.0);
			let terrain = self.map.provinces[self.our_at as usize].terrain;
			let rate = terrain.attrition + excess / self.our_army.regiments().max(1.0) * 0.05;
			let losses = self.our_army.regiments() * rate;
			self.our_army.infantry = (self.our_army.infantry - losses).max(0.0);
			self.our_losses += losses;
		}
	}

	fn arrive(&mut self, province: u32) {
		let controller = self.map.provinces[province as usize].controller;
		if controller == Side::Neutral {
			self.map.provinces[province as usize].controller = Side::Ours;
		} else if controller == Side::Theirs {
			self.our_wait = DAYS_PER_SIEGE_PHASE;
		}
	}

	fn fight(&mut self, rng: &mut Rng, enemy: usize) {
		// Shattered retreat is invulnerable in the engine: a routing
		// army fights nothing until it lands and stands.
		if self.stacks(Side::Theirs)[enemy].fleeing || self.our_fleeing {
			return;
		}
		// Armies that no longer exist do not fight.
		if self.enemies[enemy].army.regiments() < 1.0 {
			return;
		}
		let terrain = self.map.provinces[self.our_at as usize].terrain;
		let (points, destroyed, losses, our_morale, their_morale) = resolve_battle_slots(
			rng,
			self.our_army,
			self.enemies[enemy].army,
			self.our_pips,
			self.enemy_pips,
			terrain,
			self.attacker_ours,
			(
				1.0 + self.our_modifiers.morale,
				1.0 + self.enemy_modifiers.morale,
			),
			self.defines.combat_width.max(1.0) as usize,
			self.defines.flank_range.max(1.0) as usize,
		);
		self.war_score += points;
		// A beaten army stays brittle until it recovers on friendly ground.
		self.our_army.morale = our_morale;
		self.our_losses += losses;
		self.enemies[enemy].army.morale = their_morale;
		self.enemies[enemy].army.infantry =
			(self.enemies[enemy].army.infantry - destroyed).max(0.0);
		self.our_army.infantry = (self.our_army.infantry - losses).max(0.0);
		// An army broken below one regiment is destroyed outright — no
		// fractional zombies that neither act nor count as wiped.
		if self.enemies[enemy].army.regiments() < 1.0 {
			self.wipe_enemy(enemy);
		}
		if self.our_army.regiments() < 1.0 {
			self.our_army.infantry = 0.0;
			self.our_army.cavalry = 0.0;
			self.our_army.artillery = 0.0;
			self.our_army.morale = 0.0;
			self.our_path.clear();
			self.our_hop_days = 0;
			self.our_wait = 0;
		}
		if points > 0.0 {
			self.battles_won += 1;
			let at = self.our_at as usize;
			if self.map.provinces[at].controller != Side::Ours {
				self.map.provinces[at].controller = Side::Ours;
				self.siege_progress[at] = 0;
				self.provinces_taken += 1;
			}
			if self.enemies[enemy].army.regiments() >= 1.0 {
				// Shattered retreat: the beaten stack flees toward home
				// ground. Catching it mid-flight — still disorganized,
				// cut off from reinforcement — is how stacks die.
				self.stack_retreat(enemy, Side::Theirs);
			}
		} else {
			self.battles_lost += 1;
			// Shattered retreat is forced in the engine: a beaten army
			// runs for home, invulnerable, and takes no orders.
			if self.our_army.regiments() >= 1.0 {
				let home = self
					.map
					.provinces
					.iter()
					.enumerate()
					.filter(|(_, p)| p.controller == Side::Ours)
					.min_by_key(|(i, _p)| self.map.travel_days(self.our_at, *i as u32, Side::Ours))
					.map(|(i, _)| i as u32);
				if let Some(home) = home.filter(|&h| h != self.our_at) {
					self.our_fleeing = true;
					self.our_path = self.map.path(self.our_at, home, Side::Ours);
					self.our_hop_days = self
						.our_path
						.first()
						.map(|&first| self.edge_days(self.our_at, first))
						.unwrap_or(0);
					self.our_wait = 0;
				}
			}
		}
	}

	/// The field armies on one side: allies for `Ours`, enemies for
	/// `Theirs`.
	fn stacks(&self, side: Side) -> &[FrontArmy] {
		if side == Side::Ours {
			&self.allies
		} else {
			&self.enemies
		}
	}

	fn stacks_mut(&mut self, side: Side) -> &mut [FrontArmy] {
		if side == Side::Ours {
			&mut self.allies
		} else {
			&mut self.enemies
		}
	}

	/// Flee toward the nearest province `side` still controls, preferring
	/// richer ground on a tie.
	fn stack_retreat(&mut self, index: usize, side: Side) {
		let (regiments, hop_days, at) = {
			let stack = &self.stacks(side)[index];
			(stack.army.regiments(), stack.hop_days, stack.at)
		};
		if regiments < 1.0 || hop_days > 0 {
			return;
		}
		let days = self.map.travel_days_all(at, side);
		let home = self
			.map
			.provinces
			.iter()
			.enumerate()
			.filter(|(_, p)| p.controller == side)
			.min_by_key(|(i, p)| (days[*i], std::cmp::Reverse(p.development)))
			.map(|(i, _)| i as u32);
		let Some(home) = home else { return };
		if home == at {
			return;
		}
		let path = self.map.path(at, home, side);
		let hop_days = path
			.first()
			.map(|&first| self.edge_days(at, first))
			.unwrap_or(DAYS_PER_PROVINCE);
		let stack = &mut self.stacks_mut(side)[index];
		stack.fleeing = true;
		stack.path = path;
		stack.hop_days = hop_days;
	}

	/// An enemy stack destroyed outright — zeroed out and counted as a
	/// wipe, so nothing half-alive keeps besieging or gets chased.
	fn wipe_enemy(&mut self, index: usize) {
		let stack = &mut self.enemies[index];
		stack.army.infantry = 0.0;
		stack.army.cavalry = 0.0;
		stack.army.artillery = 0.0;
		stack.path.clear();
		stack.hop_days = 0;
		stack.fleeing = false;
		self.wipes += 1;
	}

	/// An allied stack destroyed outright. Only enemy stacks raise
	/// `wipes`; the ally's death is a lost battle, not a wipe.
	fn wipe_ally(&mut self, index: usize) {
		let stack = &mut self.allies[index];
		stack.army.infantry = 0.0;
		stack.army.cavalry = 0.0;
		stack.army.artillery = 0.0;
		stack.army.morale = 0.0;
		stack.path.clear();
		stack.hop_days = 0;
		stack.fleeing = false;
	}

	fn besiege_step(&mut self, rng: &mut Rng, province: u32, side: Side, siege_pips: i32) {
		let fort = self.map.provinces[province as usize].fort_level;
		// The engine's siege roll: the die plus the besieger's siege
		// pips (artillery, up to +5, and the general's pips) against
		// the fort.
		let progress = crate::world::siege_phase(rng, siege_pips, fort);
		self.siege_progress[province as usize] += progress;
		if self.siege_progress[province as usize] >= SIEGE_WIN {
			self.map.provinces[province as usize].controller = side;
			self.siege_progress[province as usize] = 0;
			if side == Side::Ours {
				self.provinces_taken += 1;
				self.war_score +=
					5.0 + self.map.provinces[province as usize].development as f32 * 0.3;
			} else {
				self.provinces_lost += 1;
				self.war_score -= 5.0;
			}
		}
	}

	/// The shared field AI, one stack at a time — enemy or allied. A
	/// mauled or retreating stack limps home; a fresh one marches on the
	/// enemy's ground and besieges it on arrival. Returns whether the
	/// stack completed a march today, which decides who attacked in an
	/// AI-versus-AI meeting.
	#[allow(clippy::collapsible_if)]
	fn front_ai(&mut self, rng: &mut Rng, index: usize, side: Side) -> bool {
		if self.stacks(side)[index].army.regiments() < 1.0 {
			return false;
		}
		// A mauled stack measures itself against every field army the
		// other side has; a pursuer can catch its flight and finish it.
		// Measured against the average opposing field stack, so a matched
		// front sorties instead of cowering before the enemy's total.
		let (total, count): (f32, f32) = if side == Side::Theirs {
			let allies: f32 = self.allies.iter().map(|s| s.army.regiments()).sum();
			(
				self.our_army.regiments() + allies,
				1.0 + self.allies.len() as f32,
			)
		} else {
			let enemies: f32 = self.enemies.iter().map(|s| s.army.regiments()).sum();
			(enemies, self.enemies.len().max(1) as f32)
		};
		let mauled = self.stacks(side)[index].army.regiments() < (total / count) * 0.6;
		if mauled || self.stacks(side)[index].fleeing {
			self.stack_retreat(index, side);
		} else if self.stacks(side)[index].hop_days == 0 {
			let at = self.stacks(side)[index].at;
			// Relief first: a foe standing on our ground is besieging
			// it; the nearest such province outranks any conquest.
			let foe_positions: Vec<u32> = if side == Side::Theirs {
				let mut ours = vec![self.our_at];
				ours.extend(self.allies.iter().map(|s| s.at));
				ours
			} else {
				self.enemies.iter().map(|s| s.at).collect()
			};
			let mut target = None;
			if let Some(&besieged) = foe_positions
				.iter()
				.filter(|&&pos| self.map.provinces[pos as usize].controller == side)
				.min_by_key(|&&pos| self.map.travel_days(at, pos, side))
			{
				target = Some(besieged);
			}
			// The enemy hunts the poorest of our provinces, nearest on a
			// tie; the ally takes the nearest enemy province, poorest on
			// a tie.
			let days = self.map.travel_days_all(at, side);
			let selected = self
				.map
				.provinces
				.iter()
				.enumerate()
				.filter(|(_, p)| p.controller == side.flip())
				.min_by_key(|(i, p)| -> (u64, i64) {
					if side == Side::Ours {
						(days[*i] as u64, p.development as i64)
					} else {
						(p.development as u64, days[*i] as i64)
					}
				})
				.map(|(i, _)| i as u32)
				.unwrap_or(at);
			// The aggressor's campaign is the war goal until it falls.
			let target = match (side == self.aggressor, self.wargoal) {
				(true, Some(goal)) if self.map.provinces[goal as usize].controller != side => goal,
				_ => target.unwrap_or(selected),
			};
			if target != at {
				let path = self.map.path(at, target, side);
				let hop_days = path
					.first()
					.map(|&first| self.edge_days(at, first))
					.unwrap_or(DAYS_PER_PROVINCE);
				let stack = &mut self.stacks_mut(side)[index];
				stack.path = path;
				stack.hop_days = hop_days;
			}
		}
		let mut arrived = false;
		if self.stacks(side)[index].hop_days > 0 {
			self.stacks_mut(side)[index].hop_days -= 1;
			if self.stacks(side)[index].hop_days == 0
				&& let Some(next) = self.stacks(side)[index].path.first().copied()
			{
				self.stacks_mut(side)[index].path.remove(0);
				self.stacks_mut(side)[index].at = next;
				arrived = true;
				if side == Side::Theirs {
					self.attacker_ours = false;
				}
				if self.stacks(side)[index].path.is_empty() {
					// The shattered retreat ends at home ground.
					self.stacks_mut(side)[index].fleeing = false;
				}
				if self.map.provinces[next as usize].controller == side.flip() {
					let pips = self.stacks(side)[index].army.artillery.min(5.0) as i32
						+ if side == Side::Ours {
							self.our_modifiers.siege_pips
						} else {
							self.enemy_modifiers.siege_pips
						};
					self.besiege_step(rng, next, side, pips);
				}
			}
		}
		arrived
	}

	/// Meetings between allied and enemy stacks in the field, after both
	/// sides have moved. A stack caught mid-retreat is destroyed
	/// outright; otherwise the battle resolves in full.
	fn ai_battles(&mut self, rng: &mut Rng, enemy_arrived: &[bool]) {
		let _ = enemy_arrived;
		for ally in 0..self.allies.len() {
			if self.allies[ally].army.regiments() < 1.0 || self.allies[ally].fleeing {
				continue;
			}
			let at = self.allies[ally].at;
			for (enemy, &arrived) in enemy_arrived.iter().enumerate() {
				if self.allies[ally].army.regiments() < 1.0 {
					break;
				}
				if self.enemies[enemy].army.regiments() < 1.0
					|| self.enemies[enemy].at != at
					|| self.enemies[enemy].fleeing
				{
					continue;
				}
				// Catching a stack in shattered retreat destroys it —
				// broken morale, no order to fight.
				if self.enemies[enemy].fleeing {
					let destroyed = self.enemies[enemy].army.regiments();
					self.war_score += 0.1 + destroyed / 20.0;
					self.battles_won += 1;
					self.wipe_enemy(enemy);
					continue;
				}
				if self.allies[ally].fleeing {
					let destroyed = self.allies[ally].army.regiments();
					self.war_score -= 0.1 + destroyed / 20.0;
					self.battles_lost += 1;
					self.wipe_ally(ally);
					break;
				}
				self.ai_battle(rng, ally, enemy, arrived);
			}
		}
	}

	/// One AI-versus-AI battle: the ally counts as the attacker exactly
	/// when the enemy stack marched in that day, and as the defender in
	/// every unclear case.
	fn ai_battle(&mut self, rng: &mut Rng, ally: usize, enemy: usize, ally_attacks: bool) {
		// Armies that no longer exist do not fight.
		if self.allies[ally].army.regiments() < 1.0 || self.enemies[enemy].army.regiments() < 1.0 {
			return;
		}
		let at = self.allies[ally].at;
		let terrain = self.map.provinces[at as usize].terrain;
		let ally_start = self.allies[ally].army.regiments();
		let enemy_start = self.enemies[enemy].army.regiments();
		let (points, destroyed, losses, ally_morale, enemy_morale) = resolve_battle_slots(
			rng,
			self.allies[ally].army,
			self.enemies[enemy].army,
			self.ally_pips,
			self.enemy_pips,
			terrain,
			ally_attacks,
			(
				1.0 + self.our_modifiers.morale,
				1.0 + self.enemy_modifiers.morale,
			),
			self.defines.combat_width.max(1.0) as usize,
			self.defines.flank_range.max(1.0) as usize,
		);
		self.war_score += points;
		// Both stacks persist the morale they leave the battle with.
		self.allies[ally].army.morale = ally_morale;
		self.enemies[enemy].army.morale = enemy_morale;
		self.enemies[enemy].army.infantry =
			(self.enemies[enemy].army.infantry - destroyed).max(0.0);
		self.allies[ally].army.infantry = (self.allies[ally].army.infantry - losses).max(0.0);
		if points > 0.0 {
			self.battles_won += 1;
		} else {
			self.battles_lost += 1;
		}
		if self.enemies[enemy].army.regiments() < 1.0 {
			self.wipe_enemy(enemy);
		} else if self.allies[ally].army.regiments() < 1.0 {
			self.wipe_ally(ally);
		} else {
			// The side that lost the larger share of itself shatters and
			// flees toward home.
			let enemy_share = destroyed / enemy_start.max(1.0);
			let ally_share = losses / ally_start.max(1.0);
			if enemy_share > ally_share {
				self.stack_retreat(enemy, Side::Theirs);
			} else if ally_share > enemy_share {
				self.stack_retreat(ally, Side::Ours);
			}
		}
	}

	/// The monthly war tick: regiments reinforce and morale recovers,
	/// both at the drawn define rates. Morale recovers
	/// `MORALE_RECOVERY_SPEED` of maximum per month plus the
	/// own-territory bonus on controlled ground; reinforcement refills
	/// `MONTHLY_REINFORCE` of each regiment's strength and draws on the
	/// manpower pool, which slows as it empties — a war of attrition is
	/// winnable by bleeding the enemy faster than the pool allows him
	/// to heal. The engine regenerates the pool from provinces over
	/// years (`MAX_MANPOWER`); this war is too short for that to
	/// matter. No armies are raised automatically: reinforcing the
	/// standing regiments is the merge the engine leaves to the player;
	/// a destroyed army stays destroyed and its provinces unguarded.
	fn monthly_recovery(&mut self) {
		self.exhaustion = (self.exhaustion + 0.1).min(MAX_WAR_EXHAUSTION);
		// A tired nation recovers slower — the engine's war-exhaustion
		// penalty, applied to morale and reinforcement alike.
		let weariness = 1.0 - self.exhaustion * 0.02;
		// Pool depletion slows reinforcement to a quarter rate.
		let our_depletion = 0.25 + 0.75 * self.our_manpower / self.our_manpower_full;
		let their_depletion = 0.25 + 0.75 * self.enemy_manpower / self.enemy_manpower_full;
		// Morale: months of rest restore a beaten army, fastest at home.
		let our_home = self.map.provinces[self.our_at as usize].controller == Side::Ours;
		if self.our_army.regiments() >= 1.0 {
			let recovery = (self.defines.morale_recovery
				+ if our_home {
					self.defines.morale_recovery_own
				} else {
					0.0
				}) * weariness;
			self.our_army.morale = (self.our_army.morale + recovery).min(1.0);
			// Reinforcing requires home ground, standing in for the
			// engine's distance-from-capital rule.
			if self.our_army.regiments() < 14.0 && our_home {
				let rate = (self.defines.monthly_reinforce * 14.0 * our_depletion * weariness)
					.min(14.0 - self.our_army.regiments())
					.min(self.our_manpower);
				self.our_army.infantry += rate;
				self.our_manpower -= rate;
			}
		}
		// The enemy's stacks share one pool and one cap: twelve
		// regiments across the whole front.
		let mut enemy_total: f32 = self.enemies.iter().map(|s| s.army.regiments()).sum();
		for stack in self.enemies.iter_mut() {
			if stack.army.regiments() < 1.0 {
				continue;
			}
			let home = self.map.provinces[stack.at as usize].controller == Side::Theirs;
			let recovery = (self.defines.morale_recovery
				+ if home {
					self.defines.morale_recovery_own
				} else {
					0.0
				}) * weariness;
			stack.army.morale = (stack.army.morale + recovery).min(1.0);
			// Occupying the enemy's provinces leaves him no home ground
			// to reinforce on.
			if enemy_total < 22.0 && home {
				let rate = (self.defines.monthly_reinforce * 22.0 * their_depletion * weariness)
					.min(22.0 - enemy_total)
					.min(self.enemy_manpower);
				stack.army.infantry += rate;
				self.enemy_manpower -= rate;
				enemy_total += rate;
			}
		}
		// Each allied stack reinforces from our pool, capped at seven
		// regiments per stack.
		for stack in self.allies.iter_mut() {
			if stack.army.regiments() < 1.0 {
				continue;
			}
			let home = self.map.provinces[stack.at as usize].controller == Side::Ours;
			let recovery = (self.defines.morale_recovery
				+ if home {
					self.defines.morale_recovery_own
				} else {
					0.0
				}) * weariness;
			stack.army.morale = (stack.army.morale + recovery).min(1.0);
			if stack.army.regiments() < 7.0 && home {
				let rate = (self.defines.monthly_reinforce * 7.0 * our_depletion * weariness)
					.min(7.0 - stack.army.regiments())
					.min(self.our_manpower);
				stack.army.infantry += rate;
				self.our_manpower -= rate;
			}
		}
	}

	pub fn finished(&self) -> bool {
		self.day >= self.horizon
			|| self.map.held_by(Side::Ours).is_empty()
			|| self.map.held_by(Side::Theirs).is_empty()
			// With our army destroyed the war is over for us.
			|| self.our_army.regiments() < 1.0
	}

	/// The three score parts the engine itself pays, before this
	/// general's own costs (patience, blood): battle points under the
	/// `WARSCORE_MAX_FROM_BATTLES` cap, the occupation share of the
	/// enemy's total province cost, and the war goal's ticking.
	/// `reward` combines them with the general's costs; `rules_vector`
	/// shows them to the policy as observable state.
	pub fn score_components(&self) -> (f32, f32, f32) {
		let battle = self
			.war_score
			.clamp(-self.defines.battles_cap, self.defines.battles_cap);
		// Each occupied province pays its share of the enemy's total
		// province cost, from the decompiled PROVINCE_WARSCORE_COST.
		let enemy_total: f32 = self
			.map
			.provinces
			.iter()
			.filter(|p| p.original_owner == Side::Theirs)
			.map(|p| {
				(crate::score::PROVINCE_COST_BASE as f32 + p.development as f32 * 1000.0) / 1000.0
			})
			.sum();
		let captured: f32 = self
			.map
			.provinces
			.iter()
			.enumerate()
			.filter(|(_, p)| p.original_owner == Side::Theirs && p.controller == Side::Ours)
			.map(|(i, p)| {
				let cost = crate::score::PROVINCE_COST_BASE as f32 + p.development as f32 * 1000.0;
				// Demanding the enemy's capital costs more, rich or not
				// (`PEACE_COST_DEMAND_CAPITAL_MULT`).
				let capital_mult = if i as u32 == self.their_capital {
					self.defines.capital_mult
				} else {
					1.0
				};
				cost / 1000.0 * capital_mult
			})
			.sum();
		let occupation = if enemy_total > 0.0 {
			captured / enemy_total * 100.0
		} else {
			0.0
		};
		// Ticking: each month holding enemy ground adds the define rate.
		let months = self.day / DAYS_PER_SIEGE_PHASE;
		// Ticking warscore: conquest wars tick for whoever holds the
		// claimed province; show-superity wars tick with the battles.
		let ticking = match self.wargoal {
			Some(goal) => {
				let goal_side = self.map.provinces[goal as usize].controller;
				months as f32
					* self.defines.ticking_per_month
					* 0.5 * match goal_side {
					Side::Ours => 1.0,
					Side::Theirs => -1.0,
					Side::Neutral => 0.0,
				}
			}
			None => 0.0,
		};
		(battle, occupation, ticking)
	}

	/// War score follows the engine's own model: battle points under the
	/// `WARSCORE_MAX_FROM_BATTLES` cap, plus each occupied province's
	/// share of the enemy's total province cost, plus ticking for the
	/// war goal. Full occupation approaches `MAX_WARSCORE` naturally
	/// because every province's cost is included; high-development and
	/// capital provinces pay more than poor ones, and the capital
	/// multiplier from the defines applies. The nonlinear behavior
	/// comes from the province cost model, not from a hand-written
	/// exponent. Speed matters through the horizon: a slower campaign
	/// accumulates more attrition and fewer monthly ticking ticks.
	pub fn reward(&self) -> f32 {
		let (battle, occupation, ticking) = self.score_components();
		// The player's patience: an uncapped, mild drag for every month the
		// war drags on. Nothing in the game enforces it; a human simply
		// will not wait forever.
		let patience = self.day as f32 / DAYS_PER_SIEGE_PHASE as f32 * 0.1;
		// Blood as the game accounts it: manpower consumed, as a share
		// of the mobilization pool, times a price coefficient.
		let blood = BLOOD_PRICE * self.our_losses / self.our_force_full;
		let total = battle + occupation + ticking - patience - blood;
		// Full occupation for five years drives warscore to 100, the
		// engine's automatic ticking for a completely conquered enemy.
		if let Some(since) = self.full_occupation_since {
			let years_held = (self.day - since) as f32 / 360.0;
			if years_held >= 5.0 {
				return 100.0;
			}
			// Before 5 years, the occupation score ramps toward 100.
			let auto = years_held / 5.0 * 100.0;
			return total.max(auto).clamp(-100.0, 100.0);
		}
		// The engine's clamp: no score beyond MAX_WARSCORE.
		total.clamp(-100.0, 100.0)
	}

	/// The observable board, one row of 28 features per province, all of
	/// it things a player can see from the map: position, development,
	/// fort, control and ownership, the stacks standing there (ours and
	/// our allies' in full detail, the enemy's only where our vision
	/// reaches), sieges, the war goal, capitals, terrain, and supply.
	pub fn board(&self) -> (usize, usize, Vec<Vec<f32>>) {
		let n = self.map.provinces.len();
		let mut cells = Vec::new();
		for (i, p) in self.map.provinces.iter().enumerate() {
			let mut row = vec![
				p.x / 5000.0,
				p.y / 5000.0,
				p.development as f32 / 35.0,
				p.fort_level as f32 / 3.0,
			];
			row.push(match p.controller {
				Side::Ours => 1.0,
				_ => 0.0,
			});
			row.push(match p.controller {
				Side::Theirs => 1.0,
				_ => 0.0,
			});
			row.push(match p.controller {
				Side::Neutral => 1.0,
				_ => 0.0,
			});
			row.push(match p.original_owner {
				Side::Ours => 1.0,
				_ => 0.0,
			});
			// Our own army: full composition and morale, only where it
			// stands.
			let ours = i as u32 == self.our_at;
			row.push(if ours {
				self.our_army.infantry / 14.0
			} else {
				0.0
			});
			row.push(if ours {
				self.our_army.cavalry / 14.0
			} else {
				0.0
			});
			row.push(if ours {
				self.our_army.artillery / 14.0
			} else {
				0.0
			});
			row.push(if ours { self.our_army.morale } else { 0.0 });
			// Allied stacks: unconditional visibility — we fight beside
			// them, so their composition and average morale are open.
			let allied_here: Vec<&FrontArmy> =
				self.allies.iter().filter(|s| s.at == i as u32).collect();
			row.push(allied_here.iter().map(|s| s.army.infantry).sum::<f32>() / 14.0);
			row.push(allied_here.iter().map(|s| s.army.cavalry).sum::<f32>() / 14.0);
			row.push(allied_here.iter().map(|s| s.army.artillery).sum::<f32>() / 14.0);
			row.push(if allied_here.is_empty() {
				0.0
			} else {
				allied_here.iter().map(|s| s.army.morale).sum::<f32>() / allied_here.len() as f32
			});
			// Enemy stacks: full composition, but only where our vision
			// reaches — a province we control or one adjacent to it.
			let enemy_here: Vec<&FrontArmy> =
				self.enemies.iter().filter(|s| s.at == i as u32).collect();
			let seen = self.map.sees_position(i as u32, Side::Ours);
			for part in [
				enemy_here.iter().map(|s| s.army.infantry).sum::<f32>(),
				enemy_here.iter().map(|s| s.army.cavalry).sum::<f32>(),
				enemy_here.iter().map(|s| s.army.artillery).sum::<f32>(),
			] {
				row.push(if seen { part / 14.0 } else { 0.0 });
			}
			row.push(self.siege_progress[i] as f32 / SIEGE_WIN as f32);
			row.push(if self.wargoal == Some(i as u32) {
				1.0
			} else {
				0.0
			});
			row.push(if i as u32 == self.our_capital {
				1.0
			} else if i as u32 == self.their_capital {
				-1.0
			} else {
				0.0
			});
			// Terrain one-hot in a fixed order, matched by name.
			for terrain in [PLAINS, FOREST, HILLS, MOUNTAINS, MARSH] {
				row.push(if p.terrain.name == terrain.name {
					1.0
				} else {
					0.0
				});
			}
			// Supply for our side on this ground.
			row.push(self.map.supply_at(i as u32, Side::Ours) / 35.0);
			// Where we stood before the last march — the player knows
			// their own path; the policy learns not to dither.
			row.push(
				if i as u32 == self.our_last && self.our_last != self.our_at {
					1.0
				} else {
					0.0
				},
			);
			cells.push(row);
		}
		let side = (n as f32).sqrt().ceil() as usize + 2;
		(side, side, cells)
	}

	/// Per-candidate rollout values: the MCTS oracle. Roll each candidate
	/// out to the horizon under a simple continuation and return average
	/// rewards. The values are per-target advantage for a value head.
	pub fn oracle_values(&self, candidates: &[u32], seeds: u32) -> Vec<f32> {
		let mut values = Vec::with_capacity(candidates.len());
		for candidate in candidates {
			let mut total = 0.0f32;
			for seed in 0..seeds {
				let mut branch = self.clone();
				let mut rng = Rng::seed(
					(u64::from(branch.day) << 32) ^ u64::from(seed + 1).wrapping_mul(0x9e37_79b9),
				);
				branch.order(*candidate);
				while !branch.finished() {
					if branch.decision_pending {
						let next = branch.candidates();
						// A greedy continuation, not standing still: head
						// for the nearest enemy-held province. Rollout
						// values under a passive continuation are noise.
						let here = branch.our_at;
						let days = branch.map.travel_days_all(here, Side::Ours);
						let goal = branch
							.map
							.provinces
							.iter()
							.enumerate()
							.filter(|(i, p)| p.controller == Side::Theirs && days[*i] < u32::MAX)
							.min_by_key(|(i, p)| (days[*i], p.development))
							.map(|(i, _)| i as u32);
						let pick = match goal {
							Some(goal) => *next
								.iter()
								.min_by_key(|&&t| branch.map.travel_days(t, goal, Side::Ours))
								.unwrap_or(&next[0]),
							None => next[0],
						};
						branch.order(pick);
					}
					branch.step_day(&mut rng);
				}
				total += branch.reward();
			}
			values.push(total / seeds as f32);
		}
		values
	}

	/// The best candidate from the MCTS oracle.
	pub fn oracle_label(&self, candidates: &[u32], seeds: u32) -> usize {
		self.oracle_values(candidates, seeds)
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
			.map(|(i, _)| i)
			.unwrap_or(0)
	}

	pub fn adjacency(&self) -> Vec<[usize; 2]> {
		let mut out = Vec::new();
		for (a, neighbors) in self.map.edges.iter().enumerate() {
			for (b, _, _) in neighbors {
				if *b as usize > a {
					out.push([a, *b as usize]);
				}
			}
		}
		out
	}

	pub fn rules_vector(&self) -> [f32; 26] {
		let our_held = self.map.held_by(Side::Ours).len();
		let their_held = self.map.held_by(Side::Theirs).len();
		let total = self.map.provinces.len();
		let enemy_regiments: f32 = self.enemies.iter().map(|s| s.army.regiments()).sum();
		let ally_regiments: f32 = self.allies.iter().map(|s| s.army.regiments()).sum();
		// The engine's own score parts, shown to the policy.
		let (battle, occupation, ticking) = self.score_components();
		[
			self.defines.battles_cap / 40.0,
			self.defines.wargoal_bonus / 5.0,
			self.defines.ticking_per_month / 0.4,
			self.day as f32 / self.horizon as f32,
			our_held as f32 / total as f32,
			their_held as f32 / total as f32,
			self.our_army.regiments() / 14.0,
			enemy_regiments / 14.0,
			self.our_army.morale,
			self.war_score / 50.0,
			self.map.supply_at(self.our_at, Side::Ours) / 35.0,
			self.siege_progress.iter().filter(|&&s| s > 0).count() as f32 / total as f32,
			// Recovery rates: a mod that changes them moves both the
			// world and these features.
			self.defines.morale_recovery / 0.15,
			self.defines.morale_recovery_own / 0.05,
			self.defines.monthly_reinforce / 0.1,
			ally_regiments / 14.0,
			// Line-combat geometry the battles deploy on.
			self.defines.combat_width / 15.0,
			self.defines.flank_range / 2.0,
			// Who declared, and so whose land the war goal sits on.
			if self.aggressor == Side::Ours {
				1.0
			} else {
				0.0
			},
			// Conquest (a claimed province) or show-superity (battles tick).
			if self.wargoal.is_some() { 1.0 } else { 0.0 },
			// The score the engine would pay right now, piece by piece.
			battle / 40.0,
			occupation / 100.0,
			ticking / 50.0,
			// What the nation still has to spend, and what the war has
			// cost it: the pool, the exhaustion, and the blood ledger.
			self.our_manpower / self.our_manpower_full,
			self.exhaustion / MAX_WAR_EXHAUSTION,
			self.our_losses / self.our_force_full,
		]
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_generated_map_splits_its_provinces() {
		for seed in 0..16u64 {
			let map = TerritoryMap::generate(seed);
			assert!(!map.held_by(Side::Ours).is_empty(), "seed {seed}");
			assert!(!map.held_by(Side::Theirs).is_empty(), "seed {seed}");
		}
	}

	#[test]
	fn supply_is_better_on_friendly_ground() {
		let map = TerritoryMap::generate(21);
		let ours = map.held_by(Side::Ours)[0];
		let theirs = map.held_by(Side::Theirs)[0];
		assert!(map.supply_at(ours, Side::Ours) > map.supply_at(theirs, Side::Ours));
	}

	#[test]
	fn the_war_runs_and_tracks_activity() {
		let mut rng = Rng::seed(3);
		let mut war = WarCampaign::new(11, "glory_seeker_personality");
		for _ in 0..400 {
			if war.decision_pending {
				let c = war.candidates();
				war.order(c[0]);
			}
			war.step_day(&mut rng);
		}
		assert!(war.day > 0);
		assert!(
			war.battles_won + war.battles_lost > 0 || war.provinces_taken + war.provinces_lost > 0
		);
	}

	#[test]
	fn a_pool_slice_splits_like_a_border_war() {
		let text = "\
PROVINCE_POOL 6
1 10 SWE 100.0 100.0 2 3 4
2 12 SWE 110.0 100.0 1 4
3 8 SWE 100.0 110.0 1 4
4 15 DAN 110.0 110.0 1 2 3 5
5 20 NOR 120.0 110.0 4 6
6 14 NOR 130.0 110.0 5
STRAITS 1
1 4
";
		let pool = ProvincePool::load(text);
		assert_eq!(pool.largest.len(), 6);
		assert_eq!(pool.straits, vec![(0, 3)]);
		for seed in 0..8u64 {
			let mut rng = Rng::seed(seed);
			let map = pool.slice(&mut rng, 6);
			assert_eq!(map.provinces.len(), 6, "seed {seed}");
			assert!(!map.held_by(Side::Ours).is_empty(), "seed {seed}");
			assert!(!map.held_by(Side::Theirs).is_empty(), "seed {seed}");
			// The strait survives into the slice as a strait edge.
			assert!(
				map.edges
					.iter()
					.flat_map(|list| list.iter())
					.any(|(_, _, kind)| matches!(kind, EdgeKind::Strait { .. })),
				"seed {seed}"
			);
		}
	}

	#[test]
	fn a_slice_is_deterministic_for_a_seed() {
		let text = "\
PROVINCE_POOL 4
1 10 SWE 0.0 0.0 2
2 10 SWE 1.0 0.0 1 3
3 10 DAN 2.0 0.0 2 4
4 10 DAN 3.0 0.0 3
STRAITS 0
";
		let pool = ProvincePool::load(text);
		let first = pool.slice(&mut Rng::seed(42), 4);
		let second = pool.slice(&mut Rng::seed(42), 4);
		let owners: Vec<Side> = first.provinces.iter().map(|p| p.original_owner).collect();
		let owners_again: Vec<Side> = second.provinces.iter().map(|p| p.original_owner).collect();
		assert_eq!(owners, owners_again);
	}

	#[test]
	fn slice_sides_follow_real_owner_tags() {
		// Two nations on one landmass, with the capitals the game's own
		// country files record (the CAPITALS section).
		let text = "\
PROVINCE_POOL 6
1 10 SWE 100.0 100.0 2 3
2 12 SWE 110.0 100.0 1 4
3 8 SWE 100.0 110.0 1 4
4 15 DAN 110.0 110.0 2 3 5
5 20 DAN 120.0 110.0 4 6
6 14 DAN 130.0 110.0 5
STRAITS 0
CAPITALS 2
SWE 2
DAN 5
";
		let pool = ProvincePool::load(text);
		let tag_at = |x: f32, y: f32| -> &str {
			match (x, y) {
				(110.0, 100.0) | (100.0, 100.0) | (100.0, 110.0) => "SWE",
				_ => "DAN",
			}
		};
		for seed in 0..8u64 {
			let mut rng = Rng::seed(seed);
			let map = pool.slice(&mut rng, 6);
			assert_eq!(map.provinces.len(), 6, "seed {seed}");
			// Every province of a side carries that side's one tag.
			let ours: Vec<&str> = map
				.provinces
				.iter()
				.filter(|p| p.original_owner == Side::Ours)
				.map(|p| tag_at(p.x, p.y))
				.collect();
			let theirs: Vec<&str> = map
				.provinces
				.iter()
				.filter(|p| p.original_owner == Side::Theirs)
				.map(|p| tag_at(p.x, p.y))
				.collect();
			assert!(!ours.is_empty() && !theirs.is_empty(), "seed {seed}");
			assert!(ours.iter().all(|t| *t == ours[0]), "seed {seed}");
			assert!(theirs.iter().all(|t| *t == theirs[0]), "seed {seed}");
			assert_ne!(ours[0], theirs[0], "seed {seed}");
			// Both named capitals are in this slice, so the map's
			// capitals are exactly them (Stockholm at 110/100, the
			// Danish one at 120/110).
			let our = &map.provinces[map.our_capital as usize];
			let their = &map.provinces[map.their_capital as usize];
			assert_eq!((our.x, our.y), (110.0, 100.0), "seed {seed}");
			assert_eq!((their.x, their.y), (120.0, 110.0), "seed {seed}");
		}
	}

	#[test]
	fn slice_capitals_fall_back_to_owned_land() {
		// Neither named capital makes the slice: SWE's is not even a
		// province of the pool, DAN's sits beyond the requested size.
		// Both sides still get a capital inside their own land.
		let text = "\
PROVINCE_POOL 8
1 10 SWE 100.0 100.0 2
2 12 SWE 110.0 100.0 1 3
3 8 SWE 120.0 100.0 2 4
4 15 DAN 130.0 100.0 3 5
5 20 DAN 140.0 100.0 4 6
6 14 DAN 150.0 100.0 5 7
7 9 DAN 160.0 100.0 6 8
8 11 DAN 170.0 100.0 7
STRAITS 0
CAPITALS 2
SWE 999
DAN 8
";
		let pool = ProvincePool::load(text);
		for seed in 0..8u64 {
			let mut rng = Rng::seed(seed);
			let map = pool.slice(&mut rng, 4);
			assert_eq!(map.provinces.len(), 4, "seed {seed}");
			assert_eq!(
				map.provinces[map.our_capital as usize].original_owner,
				Side::Ours,
				"seed {seed}"
			);
			assert_eq!(
				map.provinces[map.their_capital as usize].original_owner,
				Side::Theirs,
				"seed {seed}"
			);
		}
	}

	#[test]
	fn generated_capitals_sit_in_each_sides_land() {
		for seed in 0..16u64 {
			let map = TerritoryMap::generate(seed);
			assert_eq!(
				map.provinces[map.our_capital as usize].original_owner,
				Side::Ours,
				"seed {seed}"
			);
			assert_eq!(
				map.provinces[map.their_capital as usize].original_owner,
				Side::Theirs,
				"seed {seed}"
			);
		}
	}

	#[test]
	fn capturing_a_province_changes_the_controller() {
		let mut war = WarCampaign::new(7, "glory_seeker_personality");
		let theirs = war.map.held_by(Side::Theirs);
		assert!(!theirs.is_empty());
		let target = theirs[0] as usize;
		war.map.provinces[target].controller = Side::Ours;
		assert!(war.map.held_by(Side::Theirs).len() < theirs.len());
	}

	#[test]
	fn morale_recovers_at_the_drawn_rate() {
		let mut war = WarCampaign::new(11, "careful_personality");
		// Pin vanilla so the math is exact; the draw scales the rates.
		war.defines = DefineDraw::VANILLA;
		war.our_army.morale = 0.5;
		war.our_at = war.map.held_by(Side::Ours)[0];
		war.monthly_recovery();
		let weariness = 1.0 - 0.1 * 0.02;
		let expected = 0.5
			+ (DefineDraw::VANILLA.morale_recovery + DefineDraw::VANILLA.morale_recovery_own)
				* weariness;
		assert!(
			(war.our_army.morale - expected).abs() < 1e-5,
			"home ground should recover at MORALE_RECOVERY_SPEED + OWN_TERRITORY"
		);
	}

	#[test]
	fn wars_alternate_the_aggressor() {
		let attack = WarCampaign::new(4, "careful_personality");
		let defend = WarCampaign::new(5, "careful_personality");
		assert_eq!(attack.aggressor, Side::Ours);
		assert_eq!(defend.aggressor, Side::Theirs);
		// Capitals sit in each side's own land; claims, when they
		// exist, sit in the defender's.
		for war in [&attack, &defend] {
			let defender = war.aggressor.flip();
			assert_eq!(
				war.map.provinces[war.our_capital as usize].original_owner,
				Side::Ours
			);
			assert_eq!(
				war.map.provinces[war.their_capital as usize].original_owner,
				Side::Theirs
			);
			if let Some(goal) = war.wargoal {
				assert_eq!(war.map.provinces[goal as usize].original_owner, defender);
			}
		}
		// The same invariants on a slice of two real nations.
		let text = "\
PROVINCE_POOL 6
1 10 SWE 100.0 100.0 2 3
2 12 SWE 110.0 100.0 1 4
3 8 SWE 100.0 110.0 1 4
4 15 DAN 110.0 110.0 2 3 5
5 20 DAN 120.0 110.0 4 6
6 14 DAN 130.0 110.0 5
STRAITS 0
CAPITALS 2
SWE 2
DAN 5
";
		let pool = ProvincePool::load(text);
		for seed in 0..4u64 {
			let mut rng = Rng::seed(seed);
			let map = pool.slice(&mut rng, 6);
			let war = WarCampaign::new_on_map(map, seed, "careful_personality");
			assert_eq!(
				war.map.provinces[war.our_capital as usize].original_owner,
				Side::Ours,
				"seed {seed}"
			);
			assert_eq!(
				war.map.provinces[war.their_capital as usize].original_owner,
				Side::Theirs,
				"seed {seed}"
			);
			if let Some(goal) = war.wargoal {
				let defender = war.aggressor.flip();
				assert_eq!(
					war.map.provinces[goal as usize].original_owner, defender,
					"seed {seed}"
				);
			}
		}
	}

	#[test]
	fn a_siege_is_lifted_when_the_besieger_leaves() {
		let mut rng = Rng::seed(2);
		let mut war = WarCampaign::new(9, "careful_personality");
		let theirs = war.map.held_by(Side::Theirs)[0] as usize;
		war.siege_progress[theirs] = 10;
		war.decision_pending = false;
		// Nobody hostile stands on the province: a day passes and the
		// progress resets.
		war.step_day(&mut rng);
		assert_eq!(war.siege_progress[theirs], 0, "no besieger, no progress");
	}

	#[test]
	fn a_wiped_army_stays_destroyed() {
		let mut rng = Rng::seed(3);
		let mut war = WarCampaign::new(11, "careful_personality");
		for stack in war.enemies.iter_mut() {
			stack.army = Army {
				infantry: 0.0,
				cavalry: 0.0,
				artillery: 0.0,
				morale: 1.0,
			};
		}
		// Nothing raises armies automatically; merging recruits into a
		// destroyed stack is the player's job, not the campaign's.
		for _ in 0..180 {
			if war.decision_pending {
				war.order(war.our_at);
			}
			war.step_day(&mut rng);
		}
		assert!(
			war.enemies.iter().all(|s| s.army.regiments() < 1.0),
			"no fresh batch should appear"
		);
	}

	#[test]
	fn morale_drops_in_battle_and_recovers_at_home() {
		let mut rng = Rng::seed(17);
		let mut war = WarCampaign::new(11, "careful_personality");
		// A weaker enemy so the engagement does not destroy us.
		war.enemies[0].army.infantry = 6.0;
		war.enemies[0].army.cavalry = 0.0;
		war.enemies[0].at = war.our_at;
		war.decision_pending = false;
		war.step_day(&mut rng);
		assert!(
			war.our_army.morale < 1.0 && war.enemies[0].army.morale < 1.0,
			"both sides should leave the battle brittle"
		);
		// Months of waiting on home ground restore the army.
		let start = war.our_army.morale;
		for _ in 0..150 {
			if war.finished() {
				break;
			}
			if war.decision_pending {
				war.order(war.our_at);
			}
			war.step_day(&mut rng);
		}
		assert!(war.our_army.morale > start, "rest should recover morale");
		assert!(war.our_army.morale >= 0.99, "four months at home is full");
	}

	#[test]
	fn a_mauled_enemy_limps_home_instead_of_besieging() {
		let mut rng = Rng::seed(5);
		let mut war = WarCampaign::new(3, "careful_personality");
		// The enemy mauled below 60% of our strength, spotted on
		// neutral ground away from home.
		war.enemies[0].army.infantry = 3.0;
		war.enemies[0].army.cavalry = 0.0;
		// No manpower to heal past the mauled threshold with.
		war.enemy_manpower = 0.0;
		war.enemy_manpower_full = 1.0;
		let neutral = war
			.map
			.provinces
			.iter()
			.position(|p| p.controller == Side::Neutral)
			.expect("a neutral province") as u32;
		war.enemies[0].at = neutral;
		war.enemies[0].path.clear();
		war.enemies[0].hop_days = 0;
		for _ in 0..120 {
			if war.decision_pending {
				war.order(war.our_at);
			}
			war.step_day(&mut rng);
		}
		assert_eq!(
			war.map.provinces[war.enemies[0].at as usize].controller,
			Side::Theirs,
			"the mauled army should be back on home ground"
		);
	}

	#[test]
	fn catching_a_retreating_stack_destroys_it() {
		let mut rng = Rng::seed(9);
		let mut war = WarCampaign::new(4, "bold_fighter_personality");
		// The enemy sits mid-retreat one land edge away; we march onto it.
		let ours = war.map.held_by(Side::Ours)[0];
		let neighbor = war.map.edges[ours as usize]
			.iter()
			.find(|(_, _, k)| !matches!(k, EdgeKind::Strait { .. }))
			.map(|(n, _, _)| *n)
			.expect("a land neighbor");
		war.enemies[0].at = neighbor;
		war.enemies[0].fleeing = true;
		war.enemies[0].path.clear();
		// A shattered stack takes days to get moving.
		war.enemies[0].hop_days = 30;
		war.order(neighbor);
		for _ in 0..30 {
			war.step_day(&mut rng);
			if war.our_at == neighbor {
				break;
			}
		}
		assert_eq!(war.our_at, neighbor, "we should arrive");
		for _ in 0..3 {
			war.step_day(&mut rng);
		}
		assert!(
			war.enemies[0].army.regiments() < 1.0,
			"the caught stack should be destroyed"
		);
	}

	#[test]
	fn occupying_enemy_land_starves_their_reinforcement() {
		let mut war = WarCampaign::new(11, "careful_personality");
		for stack in war.enemies.iter_mut() {
			stack.army.infantry = 6.0;
		}
		let pool = war.enemy_manpower;
		for p in war.map.provinces.iter_mut() {
			if p.controller == Side::Theirs {
				p.controller = Side::Ours;
			}
		}
		for _ in 0..4 {
			war.monthly_recovery();
		}
		assert!(
			war.enemies.iter().all(|s| s.army.infantry == 6.0),
			"no dev left to reinforce from"
		);
		assert_eq!(war.enemy_manpower, pool, "no manpower spent");
	}

	#[test]
	fn reward_combines_the_score_components() {
		// The refactor moved the three engine-paid parts into
		// `score_components`; reward must still equal them minus the
		// general's patience and blood, clamped to MAX_WARSCORE.
		let mut rng = Rng::seed(3);
		let mut war = WarCampaign::new(11, "glory_seeker_personality");
		for _ in 0..400 {
			if war.finished() {
				break;
			}
			if war.decision_pending {
				let candidates = war.candidates();
				war.order(candidates[0]);
			}
			war.step_day(&mut rng);
			// The 5-year full-occupation ramp is a separate branch; check
			// it on states that are not inside it.
			if war.full_occupation_since.is_some() {
				continue;
			}
			let (battle, occupation, ticking) = war.score_components();
			let patience = war.day as f32 / DAYS_PER_SIEGE_PHASE as f32 * 0.1;
			let blood = BLOOD_PRICE * war.our_losses / war.our_force_full;
			let expected = (battle + occupation + ticking - patience - blood).clamp(-100.0, 100.0);
			assert!(
				(war.reward() - expected).abs() < 1e-4,
				"day {}: reward {} vs components {}",
				war.day,
				war.reward(),
				expected
			);
		}
	}

	#[test]
	fn the_board_rows_carry_29_observables_and_the_rules_26() {
		let mut rng = Rng::seed(3);
		let mut war = WarCampaign::new(11, "glory_seeker_personality");
		for _ in 0..200 {
			if war.finished() {
				break;
			}
			if war.decision_pending {
				let candidates = war.candidates();
				war.order(candidates[0]);
			}
			war.step_day(&mut rng);
		}
		let (_, _, cells) = war.board();
		assert_eq!(cells.len(), war.map.provinces.len());
		for row in &cells {
			assert_eq!(row.len(), 29);
		}
		assert_eq!(war.rules_vector().len(), 26);
		// Our own army shows exactly where it stands: composition and
		// morale at our position, zeros everywhere else.
		let at = war.our_at as usize;
		assert!((cells[at][8] - war.our_army.infantry / 14.0).abs() < 1e-6);
		assert!((cells[at][9] - war.our_army.cavalry / 14.0).abs() < 1e-6);
		assert!((cells[at][10] - war.our_army.artillery / 14.0).abs() < 1e-6);
		assert!((cells[at][11] - war.our_army.morale).abs() < 1e-6);
		for (i, row) in cells.iter().enumerate() {
			if i != at {
				assert_eq!(&row[8..12], &[0.0, 0.0, 0.0, 0.0][..], "province {i}");
			}
		}
		// Allied stacks show in full wherever they stand.
		for (i, row) in cells.iter().enumerate() {
			let ally_infantry: f32 = war
				.allies
				.iter()
				.filter(|s| s.at == i as u32)
				.map(|s| s.army.infantry)
				.sum();
			assert!(
				(row[12] - ally_infantry / 14.0).abs() < 1e-6,
				"province {i}"
			);
		}
	}

	#[test]
	fn terrain_is_exactly_one_one_hot_slot_per_province() {
		for seed in 0..8u64 {
			let war = WarCampaign::new(seed, "careful_personality");
			let (_, _, cells) = war.board();
			for (i, row) in cells.iter().enumerate() {
				let one_hot = &row[22..27];
				assert!(
					(one_hot.iter().sum::<f32>() - 1.0).abs() < 1e-6,
					"seed {seed}, province {i}: exactly one terrain slot"
				);
				let terrains = [PLAINS, FOREST, HILLS, MOUNTAINS, MARSH];
				let slot = terrains
					.iter()
					.position(|t| t.name == war.map.provinces[i].terrain.name)
					.unwrap();
				assert_eq!(one_hot[slot], 1.0, "seed {seed}, province {i}");
			}
		}
	}

	#[test]
	fn fog_hides_enemy_composition_beyond_our_vision() {
		let mut war = WarCampaign::new(11, "careful_personality");
		// Only the province our army stands on stays ours: vision then
		// covers exactly it and its neighbors.
		for (i, p) in war.map.provinces.iter_mut().enumerate() {
			if i as u32 != war.our_at {
				p.controller = Side::Theirs;
			}
		}
		let visible: std::collections::HashSet<usize> = std::iter::once(war.our_at as usize)
			.chain(
				war.map.edges[war.our_at as usize]
					.iter()
					.map(|(n, _, _)| *n as usize),
			)
			.collect();
		let hidden = (0..war.map.provinces.len())
			.find(|&i| !visible.contains(&i))
			.expect("a province beyond one hop of our army");
		war.enemies[0].army = Army {
			infantry: 9.0,
			cavalry: 1.0,
			artillery: 2.0,
			morale: 1.0,
		};
		war.enemies[0].at = hidden as u32;
		war.enemies[0].path.clear();
		war.enemies[0].hop_days = 0;
		let (_, _, cells) = war.board();
		assert_eq!(
			&cells[hidden][16..19],
			&[0.0, 0.0, 0.0][..],
			"a stack beyond our vision shows no composition"
		);
		// The same stack one province from our army is fully visible.
		let neighbor = war.map.edges[war.our_at as usize][0].0;
		war.enemies[0].at = neighbor;
		let (_, _, cells) = war.board();
		assert!((cells[neighbor as usize][16] - 9.0 / 14.0).abs() < 1e-6);
		assert!((cells[neighbor as usize][17] - 1.0 / 14.0).abs() < 1e-6);
		assert!((cells[neighbor as usize][18] - 2.0 / 14.0).abs() < 1e-6);
	}
}
