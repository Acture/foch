//! Read-only EU4 war fields through the shared data plane.
//!
//! This layout is statically identified for the data plane's pinned executable;
//! it has not yet passed a Foch game-session probe. It does not establish a
//! stable war identity, provide entry eligibility or implement `WarBridge`.
//! An adapter must reconcile every country against the live country registry.

use super::{CountryId, JoinRefusal};
use crate::dataplane::{Memory, decode_packed_tag, layout::country};
use std::collections::{BTreeMap, BTreeSet};

// War-specific fields belong here; the common registry, reader and executable
// layout remain owned by dataplane. These are not configurable offsets.
const ATTACKERS: usize = 0x48;
const DEFENDERS: usize = 0x78;
const ORIGINAL_ATTACKER: usize = 0x150;
const ORIGINAL_DEFENDER: usize = 0x158;
const MAX_PARTICIPANTS: usize = 1024;
const ACTIVE_WARS: usize = 0x1e30;
const WAR_ARRAY_COUNTS: usize = 0x1e38;
const MAX_WARS: usize = 4096;
const DIPLOMACY: usize = 0x1510;
const RELATION_SIZE: usize = 0x48;
const TRUCE_FLAGS: usize = 0x45;
const CAMPAIGN_GUID: usize = 0x24d4;
const CURRENT_HOURS: usize = 0x1dd0;
const CALENDAR_EPOCH_HOURS: i64 = 43_800_000;

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
				word.copy_from_slice(&field(CAMPAIGN_GUID + index * 4)?.to_le_bytes());
			}
			Ok((guid, field(CURRENT_HOURS)?))
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
			day: ((i64::from(hours) - CALENDAR_EPOCH_HOURS).div_euclid(24)) as i32,
		})
	}
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
		let counts = qword(memory, context, WAR_ARRAY_COUNTS)?;
		let capacity = counts as u32 as i32;
		let count = (counts >> 32) as u32 as i32;
		if capacity < 0 || count < 0 || count > capacity || capacity as usize > MAX_WARS {
			return Err("invalid active-war array capacity or count".into());
		}
		if count == 0 {
			return Ok(vec![]);
		}
		let begin = qword(memory, context, ACTIVE_WARS)? as usize;
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
				unsafe { Self::read(memory, address) }
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
			original_attacker: CountryRef::from_packed(qword(memory, address, ORIGINAL_ATTACKER)?)?,
			original_defender: CountryRef::from_packed(qword(memory, address, ORIGINAL_DEFENDER)?)?,
			attackers: participants(memory, address, ATTACKERS)?,
			defenders: participants(memory, address, DEFENDERS)?,
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
	let overlord = qword(memory, address, country::OVERLORD).map_err(unavailable)?;
	if overlord >> 56 != 0 {
		return Err(JoinRefusal::Subject);
	}
	let relations = address
		.checked_add(DIPLOMACY)
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
			.checked_add(enemy.id.0 as usize * RELATION_SIZE + TRUCE_FLAGS)
			.and_then(|address| memory.read_u8(address))
			.ok_or_else(|| JoinRefusal::Unsupported("native truce flag is unavailable".into()))?;
		// This is the same bit read by EU4's truce_with country trigger.
		if flags & 1 != 0 {
			return Err(JoinRefusal::Truce(enemy.id));
		}
	}
	Ok(())
}
