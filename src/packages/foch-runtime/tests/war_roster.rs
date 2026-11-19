#![cfg(windows)]

use foch_runtime::{
	dataplane::Memory,
	war::{
		CountryId, JoinRefusal,
		eu4::{CampaignStamp, WarRoster, check_country_restrictions},
	},
};

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
	let read = |context: &[u32]| unsafe { CampaignStamp::read(&Memory, context.as_ptr() as usize) };
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
