//! Read-only EU4 war, diplomacy, and campaign-clock views through the data plane.
//!
//! Participant vectors, original principals and the declaration hour are
//! statically identified for the data plane's pinned executable. No CWar
//! serial field was identified, so [`read_world`] records `WarId.serial` as 0
//! and the declaration tuple is the identity. These reads have not yet passed
//! a Foch game-session probe. Entry still refuses every subject, including
//! tributaries and custom types. On this image `CWar::AddAttacker`
//! (`FUN_140d89cd0`) and `CWar::AddDefender` (`FUN_140d8caf0`) take one
//! `CWar` as `this` and a packed country identity as the next argument; an
//! index already present in the attacker vector at `+0x48` or the defender
//! vector at `+0x78` is not added again. The remaining arguments are not
//! identified, so nothing here calls either method.
//! `join_all_offensive_wars_of` and `join_all_defensive_wars_of` occur only
//! as token-table names, with no direct code reference. No `CCountry` field
//! pointing at a `CSubjectType` was identified.
//!
//! Callers that already hold an [`Eu4Game`](super::Eu4Game) should use
//! [`Eu4Game::campaign_stamp`](super::Eu4Game::campaign_stamp),
//! [`Eu4Game::active_wars`](super::Eu4Game::active_wars), and
//! [`Eu4Game::read_world`](super::Eu4Game::read_world).
//! [`CountryView::check_war_restrictions`](super::CountryView::check_war_restrictions)
//! is the same entry check as [`check_country_restrictions`] without a separate
//! object-address argument. The free functions remain for a caller that has
//! already checked the context or `CCountry*` address.

use super::layout::{campaign, country, diplomacy, war};
use super::{Eu4Game, Memory, decode_packed_tag};
use crate::war::{Country, CountryId, JoinRefusal, War, WarId, World};
use std::collections::{BTreeMap, BTreeSet};

/// Packed country indexes are non-negative `i16` values, and the country
/// registry accepts at most `i16::MAX` entries. Participant vectors use that
/// same ceiling so an overhaul mod is not truncated at 1,024 countries.
const MAX_PARTICIPANTS: usize = i16::MAX as usize;

/// Identity and calendar metadata serialized by the pinned engine.
///
/// The GUID identifies a campaign, not a particular save or rollback. A pact
/// payload still needs to travel with its matching game save. Static save/load
/// paths establish these fields; a Foch save/load probe is still required.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignStamp {
	pub campaign: String,
	pub day: i32,
}

impl CampaignStamp {
	/// Read the stored GUID in the engine's printed byte order and convert its
	/// hour count to whole calendar days, including dates before its epoch.
	///
	/// # Safety
	/// The caller has verified the pinned executable and holds the live game
	/// context stable on the game thread for the whole read, including its GUID
	/// and clock fields. The repeated read detects some changes; it does not
	/// replace that lifetime and coherence requirement or make a snapshot atomic.
	pub unsafe fn read(memory: &Memory, context: usize) -> Result<Self, String> {
		let sample = || -> Result<([u8; 16], i32), String> {
			let field = |offset| {
				context
					.checked_add(offset)
					.and_then(|address| memory.read_i32(address))
					.ok_or_else(|| "campaign metadata is unavailable".to_string())
			};
			let mut guid = [0u8; 16];
			for (index, word) in guid.chunks_exact_mut(4).enumerate() {
				word.copy_from_slice(&field(campaign::GUID + index * 4)?.to_le_bytes());
			}
			Ok((guid, field(campaign::CURRENT_HOURS)?))
		};
		let (guid, hours) = sample()?;
		if sample()? != (guid, hours) {
			return Err("campaign metadata changed during read".into());
		}
		if guid == [0; 16] {
			return Err("campaign identity is nil".into());
		}
		let mut campaign = String::with_capacity(36);
		for (index, byte) in guid.into_iter().enumerate() {
			if matches!(index, 4 | 6 | 8 | 10) {
				campaign.push('-');
			}
			use std::fmt::Write;
			write!(&mut campaign, "{byte:02x}").expect("writing to a string cannot fail");
		}
		Ok(Self {
			campaign,
			day: calendar_day(hours),
		})
	}
}

fn calendar_day(hours: i32) -> i32 {
	((i64::from(hours) - campaign::CALENDAR_EPOCH_HOURS).div_euclid(24)) as i32
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountryRef {
	/// Index from the packed engine identity's signed high word. This is
	/// provisional until checked against the real country manager's entry.
	pub id: CountryId,
	pub tag: String,
}

impl CountryRef {
	fn from_packed(packed: u64) -> Result<Self, String> {
		let decoded = decode_packed_tag(packed, true)
			.filter(|tag| (tag.index as usize) < MAX_PARTICIPANTS)
			.ok_or_else(|| "invalid packed country identity in war".to_string())?;
		Ok(Self {
			id: CountryId(u32::from(decoded.index)),
			tag: decoded.tag,
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarRoster {
	pub original_attacker: CountryRef,
	pub original_defender: CountryRef,
	pub attackers: Vec<CountryRef>,
	pub defenders: Vec<CountryRef>,
}

fn qword(memory: &Memory, address: usize, offset: usize) -> Result<u64, String> {
	address
		.checked_add(offset)
		.and_then(|address| memory.read_qword(address))
		.ok_or_else(|| "war field is unavailable".into())
}

fn participants(memory: &Memory, address: usize, offset: usize) -> Result<Vec<CountryRef>, String> {
	let begin = qword(memory, address, offset)? as usize;
	let end = qword(memory, address, offset + 8)? as usize;
	let capacity = qword(memory, address, offset + 16)? as usize;
	if begin <= 0x1_0000
		|| begin & 7 != 0
		|| end <= begin
		|| end > capacity
		|| !end.is_multiple_of(8)
		|| !capacity.is_multiple_of(8)
		|| (end - begin) / 8 > MAX_PARTICIPANTS
	{
		return Err("invalid war participant vector bounds".into());
	}
	(0..(end - begin) / 8)
		.map(|index| CountryRef::from_packed(qword(memory, begin, index * 8)?))
		.collect()
}

impl WarRoster {
	/// Read the array the engine serializes as `active_war`. Ended-war history
	/// lives in a separate list and must not become a fulfillment opportunity.
	///
	/// # Safety
	/// As `read`, for the live global game context, its complete active-war
	/// pointer array and every CWar allocation and vector it references.
	pub unsafe fn read_active(memory: &Memory, context: usize) -> Result<Vec<Self>, String> {
		// SAFETY: same caller contract as the entries variant this delegates to.
		unsafe { Self::read_active_entries(memory, context) }
			.map(|entries| entries.into_iter().map(|(_, roster)| roster).collect())
	}

	/// Read the active wars together with their live object addresses, so a
	/// snapshot builder can also read the fields the roster itself does not
	/// carry (war serial and declaration date).
	///
	/// # Safety
	/// As `read`, for the live global game context and its complete
	/// active-war pointer array and every CWar allocation it references.
	pub unsafe fn read_active_entries(
		memory: &Memory,
		context: usize,
	) -> Result<Vec<(usize, Self)>, String> {
		let counts = qword(memory, context, war::ARRAY_COUNTS)?;
		let capacity = counts as u32 as i32;
		let count = (counts >> 32) as u32 as i32;
		if capacity < 0 || count < 0 || count > capacity || capacity as usize > war::MAX_WARS {
			return Err("invalid active-war array capacity or count".into());
		}
		if count == 0 {
			return Ok(vec![]);
		}
		let begin = qword(memory, context, war::ACTIVE_WARS)? as usize;
		if begin <= 0x1_0000 || !begin.is_multiple_of(8) {
			return Err("active-war array is unavailable".into());
		}
		let mut addresses = BTreeSet::new();
		(0..count as usize)
			.map(|index| {
				let address = qword(memory, begin, index * 8)? as usize;
				if address <= 0x1_0000 || !address.is_multiple_of(8) || !addresses.insert(address) {
					return Err("invalid or duplicate active-war object".into());
				}
				// SAFETY: the caller retains every complete engine object for the read.
				unsafe { Self::read(memory, address).map(|roster| (address, roster)) }
			})
			.collect()
	}

	/// Read original declaration identities and actual present participants.
	/// The first current attacker or defender is not the original principal.
	///
	/// # Safety
	/// The caller has verified the pinned executable and runs on the game
	/// thread, with a live CWar allocation covering these fields and live
	/// vectors covering their advertised ranges for the whole read. Shared
	/// Memory plausibility checks alone do not prove object lifetime or type.
	pub unsafe fn read(memory: &Memory, address: usize) -> Result<Self, String> {
		let roster = Self {
			original_attacker: CountryRef::from_packed(qword(
				memory,
				address,
				war::ORIGINAL_ATTACKER,
			)?)?,
			original_defender: CountryRef::from_packed(qword(
				memory,
				address,
				war::ORIGINAL_DEFENDER,
			)?)?,
			attackers: participants(memory, address, war::ATTACKERS)?,
			defenders: participants(memory, address, war::DEFENDERS)?,
		};
		if roster.original_attacker.id == roster.original_defender.id {
			return Err("original war principals contradict one another".into());
		}
		let mut identities = BTreeMap::new();
		for country in [&roster.original_attacker, &roster.original_defender]
			.into_iter()
			.chain(roster.attackers.iter())
			.chain(roster.defenders.iter())
		{
			if identities
				.insert(country.id, &country.tag)
				.is_some_and(|tag| tag != &country.tag)
			{
				return Err("one war country index has inconsistent tags".into());
			}
		}
		let mut members = BTreeSet::new();
		if roster
			.attackers
			.iter()
			.chain(&roster.defenders)
			.any(|country| !members.insert(country.id))
		{
			return Err("duplicate country or contradictory war membership".into());
		}
		Ok(roster)
	}
}

/// Build one coherent campaign snapshot from the live country registry, the
/// active-war array and the campaign stamp.
///
/// `revision` is the caller's monotonically increasing snapshot number. The
/// caller has verified the pinned executable and holds the game thread, with
/// the context, country registry and every active `CWar` stable for the read.
///
/// # Safety
/// As [`WarRoster::read`] and [`CampaignStamp::read`], for the live global
/// context, its country registry and every active war those structures name.
pub unsafe fn read_world(
	memory: &Memory,
	exe_base: usize,
	context: usize,
	revision: u64,
) -> Result<World, String> {
	let stamp = unsafe { CampaignStamp::read(memory, context) }?;
	let game = Eu4Game {
		memory: *memory,
		exe_base,
		context,
	};
	let registry = game
		.countries()
		.ok_or_else(|| "country registry is unavailable".to_string())?;
	let mut countries = BTreeMap::new();
	for index in 0..registry.count() {
		let Some(view) = registry.country(index) else {
			continue;
		};
		if view.is_sentinel_tag() == Some(true) {
			continue;
		}
		let tag = view
			.try_tag()
			.ok_or_else(|| format!("country {index} has no packed tag"))?;
		let packed_index = view
			.index()
			.ok_or_else(|| format!("country {index} has no packed index"))?;
		if usize::from(packed_index) != index {
			return Err(format!("country {index} packed index is {packed_index}"));
		}
		countries.insert(
			CountryId(u32::try_from(index).map_err(|_| "country index overflows")?),
			Country {
				tag,
				independent: view.independence(),
			},
		);
	}
	if countries.is_empty() {
		return Err("country registry yielded no valid countries".into());
	}
	let entries = unsafe { WarRoster::read_active_entries(memory, context) }?;
	let mut wars = Vec::with_capacity(entries.len());
	for (address, roster) in entries {
		for country in [&roster.original_attacker, &roster.original_defender]
			.into_iter()
			.chain(&roster.attackers)
			.chain(&roster.defenders)
		{
			match countries.get(&country.id) {
				Some(actual) if actual.tag == country.tag => {}
				_ => {
					return Err("war participant differs from the real country registry".into());
				}
			}
		}
		let hours = memory
			.read_i32(
				address
					.checked_add(war::STARTED)
					.ok_or_else(|| "war start date is unavailable".to_string())?,
			)
			.ok_or_else(|| "war start date is unavailable".to_string())?;
		if hours == war::UNSET_HOUR {
			return Err("war start date is unset".into());
		}
		wars.push(War {
			id: WarId {
				serial: war::UNIDENTIFIED_SERIAL,
				started: calendar_day(hours),
				declarer: roster.original_attacker.id,
				declared_on: roster.original_defender.id,
			},
			attackers: roster
				.attackers
				.into_iter()
				.map(|country| country.id)
				.collect(),
			defenders: roster
				.defenders
				.into_iter()
				.map(|country| country.id)
				.collect(),
		});
	}
	let world = World {
		campaign: stamp.campaign,
		day: stamp.day,
		revision,
		countries,
		wars,
	};
	world.validate()?;
	Ok(world)
}

/// Check the entering country's own subject and truce fields. This is only
/// one part of entry validation: it does not establish engine permission,
/// validate the opposing countries' records or check concurrent wars.
///
/// # Safety
/// As `WarRoster::read`, for the live entering CCountry allocation and its
/// complete diplomacy array. The caller has reconciled every enemy identity
/// against the same live country registry and coherent game-thread snapshot.
pub unsafe fn check_country_restrictions(
	memory: &Memory,
	address: usize,
	expected: &CountryRef,
	enemies: &[CountryRef],
) -> Result<(), JoinRefusal> {
	let unavailable = |reason: String| JoinRefusal::Unsupported(reason);
	let actual =
		CountryRef::from_packed(qword(memory, address, country::TAG).map_err(unavailable)?)
			.map_err(unavailable)?;
	if actual != *expected {
		return Err(JoinRefusal::Unsupported(
			"entering country's engine identity changed".into(),
		));
	}
	// Byte 7 of the overlord packed identity is the subject flag. Tributaries
	// and custom subject types keep that byte set. File-level subject-type
	// policy does not identify which type this country is, so they stay refused.
	let overlord = qword(memory, address, country::OVERLORD).map_err(unavailable)?;
	if overlord >> 56 != 0 {
		return Err(JoinRefusal::Subject);
	}
	let relations = address
		.checked_add(diplomacy::COUNTRY_RELATIONS)
		.and_then(|field| memory.read_ptr(field))
		.ok_or_else(|| JoinRefusal::Unsupported("native diplomacy array is unavailable".into()))?;
	for enemy in enemies {
		if enemy.id == actual.id {
			return Err(JoinRefusal::OpposingWar);
		}
		if enemy.id.0 as usize >= MAX_PARTICIPANTS {
			return Err(JoinRefusal::Unsupported(
				"enemy country index is out of range".into(),
			));
		}
		let flags = relations
			.checked_add(enemy.id.0 as usize * diplomacy::RELATION_SIZE + diplomacy::TRUCE_FLAGS)
			.and_then(|address| memory.read_u8(address))
			.ok_or_else(|| JoinRefusal::Unsupported("native truce flag is unavailable".into()))?;
		// This is the same bit read by EU4's truce_with country trigger.
		if flags & 1 != 0 {
			return Err(JoinRefusal::Truce(enemy.id));
		}
	}
	Ok(())
}

#[cfg(all(test, windows))]
mod tests {
	use super::*;
	use crate::dataplane::layout;

	struct Scene {
		image: Vec<u64>,
		context: Vec<u64>,
		manager: Vec<u64>,
		pointers: Vec<u64>,
		countries: Vec<(usize, Vec<u64>)>,
		war: Vec<u64>,
		attackers: Vec<u64>,
		defenders: Vec<u64>,
		war_list: Vec<u64>,
	}

	fn write_i32(buffer: &mut [u64], offset: usize, value: i32) {
		let index = offset / 8;
		let shift = (offset % 8) * 8;
		let mask = !(u64::from(u32::MAX) << shift);
		buffer[index] = (buffer[index] & mask) | (u64::from(value as u32) << shift);
	}

	fn write_bytes(buffer: &mut [u64], offset: usize, bytes: &[u8]) {
		for (position, byte) in bytes.iter().enumerate() {
			let at = offset + position;
			let index = at / 8;
			let shift = (at % 8) * 8;
			let mask = !(0xffu64 << shift);
			buffer[index] = (buffer[index] & mask) | (u64::from(*byte) << shift);
		}
	}

	fn pack_tag(tag: [u8; 3], index: u16, valid_flag: u8) -> u64 {
		let idx = (index as i16).to_le_bytes();
		u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, valid_flag])
	}

	impl Scene {
		fn build(countries: &[(usize, [u8; 3], bool)], war_active: bool) -> Self {
			let count = countries
				.iter()
				.map(|(index, _, _)| index + 1)
				.max()
				.unwrap();
			let mut scene = Self {
				image: vec![0; layout::CONTEXT_GLOBAL_RVA / 8 + 1],
				context: vec![0; 0x2500 / 8],
				manager: vec![0; 0x120 / 8],
				pointers: vec![0; count],
				countries: countries
					.iter()
					.map(|(index, tag, independent)| {
						let mut country = vec![0; 0x1900 / 8];
						country[layout::country::TAG / 8] = pack_tag(*tag, *index as u16, 1);
						if !independent {
							country[layout::country::OVERLORD / 8] = 1u64 << 56;
						}
						(*index, country)
					})
					.collect(),
				war: vec![0; 0x160 / 8],
				attackers: vec![],
				defenders: vec![],
				war_list: vec![0],
			};
			let guid = [
				0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
				0x0f, 0x10,
			];
			write_bytes(&mut scene.context, campaign::GUID, &guid);
			write_i32(
				&mut scene.context,
				campaign::CURRENT_HOURS,
				(campaign::CALENDAR_EPOCH_HOURS + 24 * 40) as i32,
			);
			write_i32(&mut scene.context, layout::COUNTRIES_COUNT, count as i32);
			if war_active {
				let declarer = countries.last().unwrap();
				let target = countries.first().unwrap();
				scene.attackers = vec![pack_tag(declarer.1, declarer.0 as u16, 1)];
				scene.defenders = vec![pack_tag(target.1, target.0 as u16, 1)];
				write_i32(
					&mut scene.war,
					war::STARTED,
					(campaign::CALENDAR_EPOCH_HOURS + 24 * 10) as i32,
				);
			}
			scene.link(war_active);
			scene
		}

		fn link(&mut self, war_active: bool) {
			let countries: Vec<_> = self
				.countries
				.iter()
				.map(|(index, country)| (*index, country.as_ptr() as u64))
				.collect();
			for (index, address) in countries {
				self.pointers[index] = address;
			}
			self.manager[layout::country_manager::INDEX_ARRAY / 8] = self.pointers.as_ptr() as u64;
			self.image[layout::COUNTRY_MANAGER_GLOBAL_RVA / 8] = self.manager.as_ptr() as u64;
			self.image[layout::CONTEXT_GLOBAL_RVA / 8] = self.context.as_ptr() as u64;
			if !war_active {
				return;
			}
			let attackers = self.attackers.as_ptr() as u64;
			let defenders = self.defenders.as_ptr() as u64;
			self.war[war::ATTACKERS / 8] = attackers;
			self.war[war::ATTACKERS / 8 + 1] = attackers + 8;
			self.war[war::ATTACKERS / 8 + 2] = attackers + 8;
			self.war[war::DEFENDERS / 8] = defenders;
			self.war[war::DEFENDERS / 8 + 1] = defenders + 8;
			self.war[war::DEFENDERS / 8 + 2] = defenders + 8;
			self.war[war::ORIGINAL_ATTACKER / 8] = self.attackers[0];
			self.war[war::ORIGINAL_DEFENDER / 8] = self.defenders[0];
			self.war_list[0] = self.war.as_ptr() as u64;
			self.context[war::ACTIVE_WARS / 8] = self.war_list.as_ptr() as u64;
			self.context[war::ARRAY_COUNTS / 8] = 1u64 | (1u64 << 32);
		}

		fn read(&self, revision: u64) -> Result<World, String> {
			// SAFETY: the synthetic objects above stay allocated and unchanged
			// for this synchronous read.
			unsafe {
				read_world(
					&Memory,
					self.image.as_ptr() as usize,
					self.context.as_ptr() as usize,
					revision,
				)
			}
		}
	}

	#[test]
	fn read_world_maps_registry_independence_and_the_declaration_day() {
		let scene = Scene::build(
			&[(0, *b"FRA", true), (1, *b"SWE", false), (2, *b"HAB", true)],
			true,
		);
		let world = scene.read(7).unwrap();
		assert_eq!(world.revision, 7);
		assert_eq!(world.day, 40);
		assert_eq!(world.campaign, "01020304-0506-0708-090a-0b0c0d0e0f10");
		assert_eq!(world.countries[&CountryId(0)].tag, "FRA");
		assert_eq!(world.countries[&CountryId(0)].independent, Some(true));
		assert_eq!(world.countries[&CountryId(1)].independent, Some(false));
		assert_eq!(
			world.wars,
			vec![War {
				id: WarId {
					serial: 0,
					started: 10,
					declarer: CountryId(2),
					declared_on: CountryId(0),
				},
				attackers: vec![CountryId(2)],
				defenders: vec![CountryId(0)],
			}]
		);
	}

	#[test]
	fn read_world_accepts_country_indexes_past_the_old_participant_cap() {
		let scene = Scene::build(&[(1500, *b"AAA", true), (1501, *b"BBB", true)], true);
		let world = scene.read(1).unwrap();
		assert_eq!(world.wars[0].id.declarer, CountryId(1501));
		assert_eq!(world.wars[0].id.declared_on, CountryId(1500));
		assert!(world.countries.contains_key(&CountryId(1500)));
		assert!(!world.countries.contains_key(&CountryId(0)));
	}

	#[test]
	fn read_world_skips_slot_zero_sentinel_and_rejects_empty_or_corrupt_slots() {
		let scene = Scene::build(
			&[(0, *b"---", true), (1, *b"FRA", true), (2, *b"HAB", true)],
			false,
		);
		let world = scene.read(1).unwrap();
		assert!(!world.countries.contains_key(&CountryId(0)));
		assert_eq!(world.countries[&CountryId(1)].tag, "FRA");
		assert_eq!(world.countries[&CountryId(2)].tag, "HAB");

		let only_sentinel = Scene::build(&[(0, *b"---", true)], false);
		assert_eq!(
			only_sentinel.read(1).unwrap_err(),
			"country registry yielded no valid countries"
		);

		let corrupt = Scene::build(&[(0, *b"???", true), (1, *b"FRA", true)], false);
		assert_eq!(corrupt.read(1).unwrap_err(), "country 0 has no packed tag");
	}

	#[test]
	fn read_world_rejects_an_unset_start_and_a_registry_mismatch() {
		let mut unset = Scene::build(&[(0, *b"FRA", true), (1, *b"HAB", true)], true);
		write_i32(&mut unset.war, war::STARTED, war::UNSET_HOUR);
		assert_eq!(unset.read(1).unwrap_err(), "war start date is unset");

		let mut mismatched = Scene::build(&[(0, *b"FRA", true), (1, *b"HAB", true)], true);
		mismatched.countries[0].1[layout::country::TAG / 8] = pack_tag(*b"ENG", 0, 1);
		assert_eq!(
			mismatched.read(1).unwrap_err(),
			"war participant differs from the real country registry"
		);
	}

	#[test]
	fn game_view_reads_stamp_wars_and_world() {
		let scene = Scene::build(
			&[(0, *b"FRA", true), (1, *b"SWE", false), (2, *b"HAB", true)],
			true,
		);
		let game = Eu4Game::from_tick(
			scene.image.as_ptr() as usize,
			scene.context.as_ptr() as usize,
		)
		.expect("pinned global context");
		// SAFETY: the synthetic scene stays allocated and unchanged for these reads.
		let stamp = unsafe { game.campaign_stamp() }.unwrap();
		assert_eq!(stamp.day, 40);
		assert_eq!(stamp.campaign, "01020304-0506-0708-090a-0b0c0d0e0f10");
		let wars = unsafe { game.active_wars() }.unwrap();
		assert_eq!(wars.len(), 1);
		assert_eq!(wars[0].original_attacker.tag, "HAB");
		let world = unsafe { game.read_world(9) }.unwrap();
		assert_eq!(world.revision, 9);
		assert_eq!(world.day, 40);
		assert_eq!(world.campaign, stamp.campaign);
		let country = game.countries().unwrap().country(2).unwrap();
		let refusal = unsafe {
			country.check_war_restrictions(
				&wars[0].original_attacker,
				&[wars[0].original_defender.clone()],
			)
		};
		assert!(matches!(refusal, Err(JoinRefusal::Unsupported(_))));
	}

	fn packed(tag: &[u8; 3], index: u16) -> u64 {
		u64::from_le_bytes([
			tag[0],
			tag[1],
			tag[2],
			0,
			index as u8,
			(index >> 8) as u8,
			7,
			1,
		])
	}

	#[test]
	fn native_campaign_fields_use_stored_guid_bytes_and_calendar_days() {
		let mut context = vec![0u32; 0x2500 / 4];
		let read =
			|context: &[u32]| unsafe { CampaignStamp::read(&Memory, context.as_ptr() as usize) };
		assert!(read(&context).is_err()); // A nil game identity is not a campaign.
		for (word, bytes) in (0..16u8).collect::<Vec<_>>().chunks_exact(4).enumerate() {
			context[0x24d4 / 4 + word] = u32::from_le_bytes(bytes.try_into().unwrap());
		}
		context[0x1dd0 / 4] = 43_800_000 + 1444 * 365 * 24;
		let stamp = read(&context).unwrap();
		assert_eq!(stamp.campaign, "00010203-0405-0607-0809-0a0b0c0d0e0f");
		assert_eq!(stamp.day, 1444 * 365);
		let copied_context = context.clone();
		assert_ne!(copied_context.as_ptr(), context.as_ptr());
		assert_eq!(read(&copied_context).unwrap(), stamp); // Not a pointer/host identity.
		context[0x1dd0 / 4] += 23;
		assert_eq!(read(&context).unwrap().day, stamp.day);
		context[0x1dd0 / 4] += 1;
		assert_eq!(read(&context).unwrap().day, stamp.day + 1);
		context[0x1dd0 / 4] = 43_800_000 - 1;
		assert_eq!(read(&context).unwrap().day, -1); // Engine calendar floors negative hours.
		context[0x24d4 / 4] ^= 1;
		assert_ne!(read(&context).unwrap().campaign, stamp.campaign);
	}

	#[test]
	fn raw_roster_preserves_actual_enemy_members_and_original_principals() {
		let attacker = [packed(b"ENG", 7), packed(b"HAB", 19)];
		let defender = [packed(b"FRA", 4)];
		let mut war = vec![0u64; 0x218 / 8];
		war[0x48 / 8] = attacker.as_ptr() as u64;
		war[0x50 / 8] = war[0x48 / 8] + 16;
		war[0x58 / 8] = war[0x50 / 8];
		war[0x78 / 8] = defender.as_ptr() as u64;
		war[0x80 / 8] = war[0x78 / 8] + 8;
		war[0x88 / 8] = war[0x80 / 8];
		war[0x150 / 8] = packed(b"ENG", 7);
		war[0x158 / 8] = packed(b"FRA", 4);
		// SAFETY: the complete pinned-layout buffers stay live throughout the read.
		let roster = unsafe { WarRoster::read(&Memory, war.as_ptr() as usize) }.unwrap();
		assert_eq!(roster.original_attacker.id, CountryId(7));
		assert_eq!(roster.original_defender.tag, "FRA");
		assert_eq!(roster.attackers[1].id, CountryId(19));
		assert_eq!(roster.attackers[1].tag, "HAB");
		assert_eq!(roster.defenders.len(), 1);
		let pointers = [war.as_ptr() as u64];
		let mut context = vec![0u64; 0x1e40 / 8];
		context[0x1e30 / 8] = pointers.as_ptr() as u64;
		context[0x1e38 / 8] = (1u64 << 32) | 1;
		let active = unsafe { WarRoster::read_active(&Memory, context.as_ptr() as usize) }.unwrap();
		assert_eq!(active, vec![roster]);
		// Original principals may have left after a leader change. Never replace
		// the original declaration identity with the current vector's first entry.
		war[0x150 / 8] = packed(b"DAN", 13);
		let roster = unsafe { WarRoster::read(&Memory, war.as_ptr() as usize) }.unwrap();
		assert_eq!(roster.original_attacker.tag, "DAN");
	}

	#[test]
	fn malformed_membership_is_refused_before_it_can_drive_a_join() {
		let attacker = [packed(b"HAB", 19)];
		let mut defender = [packed(b"HAB", 19)];
		let mut war = vec![0u64; 0x218 / 8];
		for (offset, members) in [(0x48, attacker.as_slice()), (0x78, defender.as_slice())] {
			war[offset / 8] = members.as_ptr() as u64;
			war[(offset + 8) / 8] = war[offset / 8] + 8;
			war[(offset + 16) / 8] = war[(offset + 8) / 8];
		}
		war[0x150 / 8] = packed(b"HAB", 19);
		war[0x158 / 8] = packed(b"FRA", 4);
		let read = |war: &[u64]| unsafe { WarRoster::read(&Memory, war.as_ptr() as usize) };
		assert!(read(&war).is_err()); // same country on both sides
		defender[0] = packed(b"FRA", 4);
		std::hint::black_box(&defender);
		assert!(read(&war).is_ok());
		war[0x50 / 8] += 1;
		assert!(read(&war).is_err()); // non-element boundary; no payload dereference
		war[0x50 / 8] -= 1;
		war[0x50 / 8] += 8;
		assert!(read(&war).is_err()); // end exceeds allocation capacity
		war[0x50 / 8] -= 8;
		war[0x150 / 8] = packed(b"FRA", 4);
		assert!(read(&war).is_err()); // contradictory original declaration
		war[0x150 / 8] = packed(b"HAB", 0xffff);
		assert!(read(&war).is_err()); // negative engine country index
	}

	#[test]
	fn active_war_array_checks_engine_count_before_following_any_war_pointer() {
		let mut context = vec![0u64; 0x1e40 / 8];
		let address = context.as_ptr() as usize;
		assert!(
			unsafe { WarRoster::read_active(&Memory, address) }
				.unwrap()
				.is_empty()
		);
		// Advertise two wars but capacity for one; the null pointer must never
		// reach a payload read. The array's capacity/count share the final qword.
		context[0x1e38 / 8] = (2u64 << 32) | 1;
		assert!(unsafe { WarRoster::read_active(&Memory, address) }.is_err());
		context[0x1e38 / 8] = (4097u64 << 32) | 4097;
		assert!(unsafe { WarRoster::read_active(&Memory, address) }.is_err());
	}

	#[test]
	fn native_country_checks_refuse_subjects_truces_and_unverified_identity() {
		let attackers = [packed(b"HAB", 19)];
		let defenders = [packed(b"FRA", 4), packed(b"SWE", 2)];
		let mut war = vec![0u64; 0x218 / 8];
		for (offset, members) in [(0x48, attackers.as_slice()), (0x78, defenders.as_slice())] {
			war[offset / 8] = members.as_ptr() as u64;
			war[(offset + 8) / 8] = war[offset / 8] + members.len() as u64 * 8;
			war[(offset + 16) / 8] = war[(offset + 8) / 8];
		}
		war[0x150 / 8] = attackers[0];
		war[0x158 / 8] = defenders[0];
		let roster = unsafe { WarRoster::read(&Memory, war.as_ptr() as usize) }.unwrap();
		let mut country = vec![0u64; 0x1900 / 8];
		let mut relations = vec![0u64; 32 * 0x48 / 8];
		country[0x20 / 8] = packed(b"SWE", 2);
		country[0x1510 / 8] = relations.as_ptr() as u64;
		let check = |country: &[u64]| unsafe {
			check_country_restrictions(
				&Memory,
				country.as_ptr() as usize,
				&roster.defenders[1],
				&roster.attackers,
			)
		};
		assert_eq!(check(&country), Ok(()));
		let view = super::super::CountryView::from_ptr(Memory, country.as_ptr() as usize).unwrap();
		assert_eq!(
			unsafe { view.check_war_restrictions(&roster.defenders[1], &roster.attackers) },
			Ok(())
		);
		let bit_offset = 19 * 0x48 + 0x45;
		relations[bit_offset / 8] |= 1u64 << ((bit_offset % 8) * 8);
		std::hint::black_box(&relations);
		assert_eq!(check(&country), Err(JoinRefusal::Truce(CountryId(19))));
		country[0x18f0 / 8] = packed(b"DAN", 13);
		assert_eq!(check(&country), Err(JoinRefusal::Subject));
		country[0x18f0 / 8] = 0;
		country[0x20 / 8] = packed(b"DAN", 13);
		assert!(matches!(check(&country), Err(JoinRefusal::Unsupported(_))));
		country[0x20 / 8] = packed(b"SWE", 2);
		country[0x1510 / 8] = 0;
		assert!(matches!(check(&country), Err(JoinRefusal::Unsupported(_))));
	}
}
