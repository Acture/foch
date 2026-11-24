use super::*;

const DELTA: u64 = 0x400;

fn embedded() -> serde_json::Value {
	serde_json::from_str(EMBEDDED[0]).unwrap()
}

fn shift_offsets(value: &mut serde_json::Value, delta: u64) {
	match value {
		serde_json::Value::Object(map) => {
			for (key, child) in map.iter_mut() {
				if (key == "offset" || key == "rva")
					&& let Some(text) = child.as_str()
					&& let Some(digits) = text.strip_prefix("0x")
					&& let Ok(number) = u64::from_str_radix(digits, 16)
				{
					*child = serde_json::Value::String(format!("0x{:x}", number + delta));
				} else {
					shift_offsets(child, delta);
				}
			}
		}
		serde_json::Value::Array(items) => {
			for item in items {
				shift_offsets(item, delta);
			}
		}
		_ => {}
	}
}

fn synthetic_profile() -> &'static LayoutProfile {
	let mut value = embedded();
	shift_offsets(&mut value, DELTA);
	let pattern = value["hooks"]["monthly_tick"]["pattern"]
		.as_str()
		.unwrap()
		.replacen("88", "90", 1);
	value["hooks"]["monthly_tick"]["pattern"] = serde_json::Value::String(pattern);
	value["exe_sha256"] = serde_json::Value::String("b".repeat(64));
	value["version"] = serde_json::Value::String("synthetic-b".into());
	let profile = parse_profile(&serde_json::to_string(&value).unwrap()).expect("profile B");
	let leaked = Box::leak(Box::new(profile));
	install_test_profile(leaked);
	leaked
}

fn mutated(edit: impl FnOnce(&mut serde_json::Value)) -> Result<LayoutProfile, String> {
	let mut value = embedded();
	edit(&mut value);
	parse_profile(&value.to_string())
}

#[test]
fn experiment_b_rejects_unknown_digests_and_bad_schemas() {
	let pinned = default_profile().exe_sha256.clone();
	assert!(profile_for_build(&pinned).is_some());
	assert!(profile_for_build("1.37.5").is_none());
	assert!(profile_for_build(&"ab".repeat(32)).is_none());
	assert!(profile_for_build(&pinned.to_ascii_uppercase()).is_none());
	assert!(super::super::supports_build(&pinned));
	assert!(!super::super::supports_build(&pinned.to_ascii_uppercase()));

	let uppercase = mutated(|value| {
		value["exe_sha256"] = serde_json::Value::String(pinned.to_ascii_uppercase());
	})
	.unwrap_err();
	assert!(uppercase.contains("exe_sha256"), "{uppercase}");

	let bad_stl = mutated(|value| {
		value["abi"]["stl"] = serde_json::Value::String("itanium".into());
	})
	.unwrap_err();
	assert!(bad_stl.contains("abi.stl"), "{bad_stl}");

	let bad_pointer = mutated(|value| {
		value["abi"]["pointer_size"] = serde_json::json!(4);
	})
	.unwrap_err();
	assert!(bad_pointer.contains("pointer_size"), "{bad_pointer}");

	let zero_stride = mutated(|value| {
		value["structs"]["node"]["entry_size"]["offset"] = serde_json::Value::String("0x0".into());
	})
	.unwrap_err();
	assert!(zero_stride.contains("stride"), "{zero_stride}");

	for bad in ["bb0", "0xGG", "0xBB0", "0X10"] {
		let malformed = mutated(|value| {
			value["structs"]["country"]["tag"]["offset"] = serde_json::Value::String(bad.into());
		})
		.unwrap_err();
		assert!(malformed.contains("lowercase"), "{bad}: {malformed}");
	}

	let bad_status = mutated(|value| {
		value["structs"]["node"]["value"]["status"] = serde_json::Value::String("verified".into());
	})
	.unwrap_err();
	assert!(bad_status.contains("status"), "{bad_status}");

	let bad_schema = mutated(|value| {
		value["schema"] = serde_json::json!(2);
	})
	.unwrap_err();
	assert!(bad_schema.contains("schema"), "{bad_schema}");
}

#[test]
fn experiment_c_status_matches_the_1375_evidence_split() {
	let profile = default_profile();
	assert_eq!(profile.schema, 1);
	assert_eq!(profile.game, "eu4");
	assert_eq!(profile.version, "1.37.5");
	assert_eq!(profile.platform, "windows-x64");
	assert_eq!(profile.abi_stl, "msvc_x64");
	assert_eq!(profile.pointer_size, 8);
	assert_eq!(
		profile.exe_sha256,
		"c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e"
	);
	assert_eq!(profile.monthly_tick.rva, 0x75d690);
	assert_eq!(profile.country_treasury.offset, 0xbb0);
	assert_eq!(profile.country_tag.offset, 0x20);
	assert_eq!(profile.node_value.offset, 0xb0);
	assert_eq!(profile.node_power.offset, 0xc8);
	assert_eq!(profile.def_routes.offset, 0x98);
	assert_eq!(profile.province_tax.offset, 0x3e4);
	assert_eq!(profile.province_manpower.offset, 0x3e0);
	assert_eq!(profile.province_cprovince_size.offset, 0x2e10);
	assert_eq!(profile.route_target_key.offset, 0x10);
	assert_eq!(profile.country_treasury_clamp, 1_000_000_000);
	assert_eq!(profile.war_max_wars, 4096);
	assert_eq!(profile.war_unset_hour, 0x029c_77f8);
	assert!(
		profile
			.route_target_key
			.evidence
			.as_deref()
			.unwrap()
			.contains("node_members")
	);
	assert!(profile.monthly_tick.evidence.is_some());

	let live = [
		"hooks.monthly_tick",
		"globals.context",
		"globals.country_manager",
		"context.db",
		"context.countries_begin",
		"context.countries_count",
		"db.nodes",
		"db.node_count",
		"node.entry_table",
		"node.entry_size",
		"node.weights",
		"node.definition",
		"node.value",
		"node.local_value",
		"node.transferred",
		"node.power",
		"entry.power",
		"entry.mode_collect",
		"entry.mode_transfer",
		"def.name",
		"def.routes",
		"def.routes_end",
		"route.size",
		"route.target",
		"province.cprovinces",
		"province.cprovince_size",
		"province.manpower",
		"province.tax",
		"country_manager.index_array",
		"country.tag",
		"country.treasury",
		"ui.controller",
		"ui.flags",
		"ui.treasury_dirty",
	];
	let static_only = [
		"globals.flag_names",
		"def.members",
		"route.target_key",
		"country.monthly_income_bank",
		"country.overlord",
		"country.subjects_vec",
		"country.tribute_type",
		"campaign.guid",
		"campaign.current_hours",
		"war.active_wars",
		"war.array_counts",
		"war.started",
		"war.attackers",
		"war.defenders",
		"war.original_attacker",
		"war.original_defender",
		"diplomacy.country_relations",
		"diplomacy.relation_size",
		"diplomacy.truce_flags",
		"flags.country_flags",
		"flags.flag_ids",
		"flags.flag_dates",
		"flags.string_size",
	];
	assert_eq!(live.len() + static_only.len(), FIELD_PATHS.len());
	for path in FIELD_PATHS {
		let status = profile_field_status(profile, path).unwrap();
		if live.contains(path) {
			assert_eq!(status, FieldStatus::LiveVerified, "{path}");
		} else if static_only.contains(path) {
			assert_eq!(status, FieldStatus::StaticOnly, "{path}");
		} else {
			panic!("uncategorized profile field {path}");
		}
	}
}

#[test]
fn experiment_d_stock_steam_digest_shares_the_amdac_layout() {
	// EJLO9NT's clean Steam `eu4.exe` digest; AMDAC ships a build that
	// differs only in four PE header bytes and the two checksum bytes.
	const STOCK: &str = "9ad3efe1af169f40ee577f9dae5debbc87af6fb8b5450fb345ebf110dc4d771a";

	// EMBEDDED[0] stays the pinned default build; EMBEDDED[1] is the stock.
	assert_eq!(EMBEDDED.len(), 2);
	assert_eq!(
		default_profile().exe_sha256,
		"c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e"
	);
	let stock_json: serde_json::Value = serde_json::from_str(EMBEDDED[1]).unwrap();
	assert_eq!(stock_json["exe_sha256"], serde_json::json!(STOCK));

	assert!(super::super::supports_build(STOCK));
	let stock = profile_for_build(STOCK).expect("stock Steam 1.37.5 profile");
	assert!(!std::ptr::eq(stock, default_profile()));
	assert!(profile_for_build(&STOCK.to_ascii_uppercase()).is_none());

	// The executables are byte-identical where it matters, so every hook,
	// global, struct field, stride and cap must carry over unchanged.
	let amdac = default_profile();
	assert_eq!(stock.version, amdac.version);
	assert_eq!(stock.platform, amdac.platform);
	assert_eq!(stock.abi_stl, amdac.abi_stl);
	assert_eq!(stock.pointer_size, amdac.pointer_size);
	assert_eq!(stock.monthly_tick.rva, amdac.monthly_tick.rva);
	assert_eq!(stock.monthly_tick.pattern, amdac.monthly_tick.pattern);
	assert_eq!(stock.monthly_tick.status, amdac.monthly_tick.status);
	assert_eq!(
		stock.campaign_calendar_epoch_hours,
		amdac.campaign_calendar_epoch_hours
	);
	assert_eq!(stock.country_treasury_clamp, amdac.country_treasury_clamp);
	assert_eq!(stock.war_max_wars, amdac.war_max_wars);
	assert_eq!(stock.war_unset_hour, amdac.war_unset_hour);
	assert_eq!(stock.war_unidentified_serial, amdac.war_unidentified_serial);
	for path in FIELD_PATHS {
		if *path == "hooks.monthly_tick" {
			continue; // the rva, pattern and status are compared above
		}
		let stock_field = profile_field(stock, path).unwrap();
		let amdac_field = profile_field(amdac, path).unwrap();
		assert_eq!(stock_field.offset, amdac_field.offset, "{path}");
		assert_eq!(stock_field.status, amdac_field.status, "{path}");
	}
}

#[test]
fn experiment_a_profiles_differ_before_any_memory_access() {
	let left = default_profile();
	let right = synthetic_profile();
	assert!(!std::ptr::eq(left, right));
	assert_ne!(left.exe_sha256, right.exe_sha256);
	assert_ne!(left.country_treasury.offset, right.country_treasury.offset);
	assert_eq!(
		right.country_treasury.offset,
		left.country_treasury.offset + DELTA as usize
	);
	assert_ne!(left.country_tag.offset, right.country_tag.offset);
	assert_ne!(left.node_value.offset, right.node_value.offset);
	assert_ne!(left.war_started.offset, right.war_started.offset);
	assert_ne!(left.route_size.offset, right.route_size.offset);
	assert_ne!(left.monthly_tick.rva, right.monthly_tick.rva);
	assert_ne!(left.monthly_tick.pattern, right.monthly_tick.pattern);
	assert_eq!(left.country_treasury_clamp, right.country_treasury_clamp);
	assert_eq!(left.war_max_wars, right.war_max_wars);
	assert!(profile_for_build(&right.exe_sha256).is_some());
	assert!(super::super::Eu4Game::from_tick_for_build("not-a-digest", 0x20_0000, 1).is_none());
}

#[cfg(windows)]
struct BindGuard {
	keys: Vec<(usize, usize)>,
}

#[cfg(windows)]
impl Drop for BindGuard {
	fn drop(&mut self) {
		for (exe_base, context) in &self.keys {
			unbind_game(*exe_base, *context);
		}
	}
}

#[cfg(windows)]
fn write_i32(buffer: &mut [u64], offset: usize, value: i32) {
	let index = offset / 8;
	let shift = (offset % 8) * 8;
	let mask = !(u64::from(u32::MAX) << shift);
	buffer[index] = (buffer[index] & mask) | (u64::from(value as u32) << shift);
}

#[cfg(windows)]
fn write_u64(buffer: &mut [u64], offset: usize, value: u64) {
	assert!(offset.is_multiple_of(8), "misaligned {offset:#x}");
	buffer[offset / 8] = value;
}

#[cfg(windows)]
fn write_bytes(buffer: &mut [u64], offset: usize, bytes: &[u8]) {
	for (position, byte) in bytes.iter().enumerate() {
		let at = offset + position;
		let index = at / 8;
		let shift = (at % 8) * 8;
		let mask = !(0xffu64 << shift);
		buffer[index] = (buffer[index] & mask) | (u64::from(*byte) << shift);
	}
}

#[cfg(windows)]
fn plant_sso(buffer: &mut [u64], offset: usize, key: &str) {
	let mut inline = [0u8; 16];
	inline[..key.len()].copy_from_slice(key.as_bytes());
	write_bytes(buffer, offset, &inline);
	write_u64(buffer, offset + 0x10, key.len() as u64);
	write_u64(buffer, offset + 0x18, 15);
}

#[cfg(windows)]
fn pack(tag: &[u8; 3], index: u16, flag: u8) -> u64 {
	let idx = (index as i16).to_le_bytes();
	u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, flag])
}

#[cfg(windows)]
fn plant_side(war: &mut [u64], offset: usize, members: &[u64]) {
	let begin = members.as_ptr() as u64;
	let bytes = (members.len() * 8) as u64;
	write_u64(war, offset, begin);
	write_u64(war, offset + 8, begin + bytes);
	write_u64(war, offset + 16, begin + bytes);
}

#[test]
#[cfg(windows)]
fn experiment_a_dual_profiles_do_not_cross_on_shared_objects() {
	use super::super::Memory;
	use super::super::game::Eu4Game;
	use super::super::war::CountryRef;
	use crate::war::{CountryId, JoinRefusal};

	let profile_a = default_profile();
	let profile_b = synthetic_profile();
	let epoch = profile_a.campaign_calendar_epoch_hours;
	assert_eq!(epoch, profile_b.campaign_calendar_epoch_hours);

	let mut image_a = vec![
		0u64;
		profile_a
			.context_global
			.offset
			.max(profile_a.country_manager_global.offset)
			/ 8 + 1
	];
	let mut image_b = vec![
		0u64;
		profile_b
			.context_global
			.offset
			.max(profile_b.country_manager_global.offset)
			/ 8 + 1
	];
	let mut context = vec![0u64; 0x600];
	let mut manager = vec![0u64; 0x100];
	let mut pointers = [0u64; 2];
	let mut country0 = vec![0u64; 0x600];
	let mut country1 = vec![0u64; 0x600];
	let relations = vec![0u64; 0x40];
	let controller = vec![0u64; 0x100];
	let mut node_ptrs = [0u64; 1];
	let mut node = vec![0u64; 0x100];
	let mut definition = vec![0u64; 0x100];
	let mut route_a = vec![0u64; profile_a.route_size.offset.div_ceil(8)];
	let mut route_b = vec![0u64; profile_b.route_size.offset.div_ceil(8)];
	let mut province = vec![0u8; 0x800];
	let mut war_a = vec![0u64; 0xc0];
	let mut war_b = vec![0u64; 0xc0];
	let attackers_a = [pack(b"HAB", 1, 1)];
	let defenders_a = [pack(b"FRA", 0, 1)];
	let attackers_b = [pack(b"SWE", 1, 1)];
	let defenders_b = [pack(b"ENG", 0, 1)];
	let list_a = [0u64; 1];
	let list_b = [0u64; 1];

	write_u64(
		&mut country0,
		profile_a.country_tag.offset,
		pack(b"FRA", 0, 1),
	);
	write_u64(
		&mut country0,
		profile_b.country_tag.offset,
		pack(b"ENG", 0, 1),
	);
	write_i32(&mut country0, profile_a.country_treasury.offset, 1_000);
	write_i32(&mut country0, profile_b.country_treasury.offset, 2_000);
	write_u64(&mut country0, profile_b.country_overlord.offset, 1u64 << 56);
	write_u64(
		&mut country0,
		profile_a.diplomacy_country_relations.offset,
		relations.as_ptr() as u64,
	);
	write_u64(
		&mut country0,
		profile_b.diplomacy_country_relations.offset,
		relations.as_ptr() as u64,
	);
	write_u64(
		&mut country1,
		profile_a.country_tag.offset,
		pack(b"HAB", 1, 1),
	);
	write_u64(
		&mut country1,
		profile_b.country_tag.offset,
		pack(b"SWE", 1, 1),
	);
	pointers[0] = country0.as_ptr() as u64;
	pointers[1] = country1.as_ptr() as u64;
	write_u64(
		&mut manager,
		profile_a.country_manager_index_array.offset,
		pointers.as_ptr() as u64,
	);
	write_u64(
		&mut manager,
		profile_b.country_manager_index_array.offset,
		pointers.as_ptr() as u64,
	);

	write_i32(&mut context, profile_a.countries_count.offset, 2);
	write_i32(&mut context, profile_b.countries_count.offset, 2);
	write_bytes(&mut context, profile_a.campaign_guid.offset, &[0x11; 16]);
	write_bytes(&mut context, profile_b.campaign_guid.offset, &[0x22; 16]);
	write_i32(
		&mut context,
		profile_a.campaign_current_hours.offset,
		(epoch + 24 * 20) as i32,
	);
	write_i32(
		&mut context,
		profile_b.campaign_current_hours.offset,
		(epoch + 24 * 30) as i32,
	);
	write_u64(
		&mut context,
		profile_a.ui_controller.offset,
		controller.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		profile_b.ui_controller.offset,
		controller.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		profile_a.province_cprovinces.offset,
		province.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		profile_b.province_cprovinces.offset,
		province.as_ptr() as u64,
	);
	province[profile_a.province_tax.offset..profile_a.province_tax.offset + 4]
		.copy_from_slice(&12_000i32.to_le_bytes());
	province[profile_b.province_tax.offset..profile_b.province_tax.offset + 4]
		.copy_from_slice(&34_000i32.to_le_bytes());
	province[profile_a.province_manpower.offset..profile_a.province_manpower.offset + 4]
		.copy_from_slice(&6_000i32.to_le_bytes());
	province[profile_b.province_manpower.offset..profile_b.province_manpower.offset + 4]
		.copy_from_slice(&8_000i32.to_le_bytes());

	node_ptrs[0] = node.as_ptr() as u64;
	let db_a = profile_a.context_db.offset;
	let db_b = profile_b.context_db.offset;
	write_u64(
		&mut context,
		db_a + profile_a.db_nodes.offset,
		node_ptrs.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		db_b + profile_b.db_nodes.offset,
		node_ptrs.as_ptr() as u64,
	);
	write_i32(&mut context, db_a + profile_a.db_node_count.offset, 1);
	write_i32(&mut context, db_b + profile_b.db_node_count.offset, 1);
	write_u64(
		&mut node,
		profile_a.node_definition.offset,
		definition.as_ptr() as u64,
	);
	write_u64(
		&mut node,
		profile_b.node_definition.offset,
		definition.as_ptr() as u64,
	);
	write_i32(&mut node, profile_a.node_value.offset, 100);
	write_i32(&mut node, profile_b.node_value.offset, 250);
	plant_sso(&mut route_a, profile_a.route_target_key.offset, "lubeck");
	plant_sso(&mut route_b, profile_b.route_target_key.offset, "genoa");
	let route_a_ptr = route_a.as_ptr() as u64;
	let route_b_ptr = route_b.as_ptr() as u64;
	write_u64(&mut definition, profile_a.def_routes.offset, route_a_ptr);
	write_u64(
		&mut definition,
		profile_a.def_routes_end.offset,
		route_a_ptr + profile_a.route_size.offset as u64,
	);
	write_u64(&mut definition, profile_b.def_routes.offset, route_b_ptr);
	write_u64(
		&mut definition,
		profile_b.def_routes_end.offset,
		route_b_ptr + profile_b.route_size.offset as u64,
	);

	plant_side(&mut war_a, profile_a.war_attackers.offset, &attackers_a);
	plant_side(&mut war_a, profile_a.war_defenders.offset, &defenders_a);
	write_u64(
		&mut war_a,
		profile_a.war_original_attacker.offset,
		attackers_a[0],
	);
	write_u64(
		&mut war_a,
		profile_a.war_original_defender.offset,
		defenders_a[0],
	);
	write_i32(
		&mut war_a,
		profile_a.war_started.offset,
		(epoch + 24 * 4) as i32,
	);
	write_i32(&mut war_a, profile_b.war_started.offset, 99);
	plant_side(&mut war_b, profile_b.war_attackers.offset, &attackers_b);
	plant_side(&mut war_b, profile_b.war_defenders.offset, &defenders_b);
	write_u64(
		&mut war_b,
		profile_b.war_original_attacker.offset,
		attackers_b[0],
	);
	write_u64(
		&mut war_b,
		profile_b.war_original_defender.offset,
		defenders_b[0],
	);
	write_i32(
		&mut war_b,
		profile_b.war_started.offset,
		(epoch + 24 * 11) as i32,
	);
	write_i32(&mut war_b, profile_a.war_started.offset, 1);
	let mut lists_a = list_a;
	let mut lists_b = list_b;
	lists_a[0] = war_a.as_ptr() as u64;
	lists_b[0] = war_b.as_ptr() as u64;
	write_u64(
		&mut context,
		profile_a.war_active_wars.offset,
		lists_a.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		profile_b.war_active_wars.offset,
		lists_b.as_ptr() as u64,
	);
	write_u64(
		&mut context,
		profile_a.war_array_counts.offset,
		1u64 | (1u64 << 32),
	);
	write_u64(
		&mut context,
		profile_b.war_array_counts.offset,
		1u64 | (1u64 << 32),
	);

	let context_ptr = context.as_ptr() as u64;
	let manager_ptr = manager.as_ptr() as u64;
	image_a[profile_a.context_global.offset / 8] = context_ptr;
	image_a[profile_a.country_manager_global.offset / 8] = manager_ptr;
	image_b[profile_b.context_global.offset / 8] = context_ptr;
	image_b[profile_b.country_manager_global.offset / 8] = manager_ptr;
	std::hint::black_box((
		&image_a,
		&image_b,
		&context,
		&manager,
		&pointers,
		&country0,
		&country1,
		&relations,
		&controller,
		&node_ptrs,
		&node,
		&definition,
		&route_a,
		&route_b,
		&province,
		&war_a,
		&war_b,
		&attackers_a,
		&defenders_a,
		&attackers_b,
		&defenders_b,
		&lists_a,
		&lists_b,
	));

	let base_a = image_a.as_ptr() as usize;
	let base_b = image_b.as_ptr() as usize;
	let context_addr = context.as_ptr() as usize;
	let game_a = Eu4Game::from_tick_for_build(&profile_a.exe_sha256, base_a, context_addr)
		.expect("profile A context");
	let game_b = Eu4Game::from_tick_for_build(&profile_b.exe_sha256, base_b, context_addr)
		.expect("profile B context");
	let _guard = BindGuard {
		keys: vec![(base_a, context_addr), (base_b, context_addr)],
	};

	let country_a = game_a.countries().unwrap().country(0).unwrap();
	let country_b = game_b.countries().unwrap().country(0).unwrap();
	assert_eq!(country_a.tag(), "FRA");
	assert_eq!(country_b.tag(), "ENG");
	assert_eq!(country_a.treasury(), Some(1_000));
	assert_eq!(country_b.treasury(), Some(2_000));
	assert_eq!(country_a.independence(), Some(true));
	assert_eq!(country_b.independence(), Some(false));
	unsafe { game_a.pay_country(0, 50) }.unwrap();
	assert_eq!(
		game_a.countries().unwrap().country(0).unwrap().treasury(),
		Some(1_050)
	);
	assert_eq!(
		game_b.countries().unwrap().country(0).unwrap().treasury(),
		Some(2_000)
	);
	unsafe { game_b.pay_country(0, 25) }.unwrap();
	assert_eq!(
		game_a.countries().unwrap().country(0).unwrap().treasury(),
		Some(1_050)
	);
	assert_eq!(
		game_b.countries().unwrap().country(0).unwrap().treasury(),
		Some(2_025)
	);
	assert_eq!(
		Memory.read_i32(controller.as_ptr() as usize + profile_a.ui_flags.offset),
		Some(profile_a.ui_treasury_dirty.offset as i32)
	);
	assert_eq!(
		Memory.read_i32(controller.as_ptr() as usize + profile_b.ui_flags.offset),
		Some(profile_b.ui_treasury_dirty.offset as i32)
	);

	let province_a = game_a.province(0).unwrap();
	let province_b = game_b.province(0).unwrap();
	assert_eq!(province_a.tax(), Some(12_000));
	assert_eq!(province_b.tax(), Some(34_000));
	assert_eq!(province_a.manpower(), Some(6_000));
	assert_eq!(province_b.manpower(), Some(8_000));

	let nodes_a = game_a.trade_nodes().unwrap();
	let nodes_b = game_b.trade_nodes().unwrap();
	let node_a = nodes_a.node(0).unwrap();
	let node_b = nodes_b.node(0).unwrap();
	assert_eq!(node_a.value(), Some(100));
	assert_eq!(node_b.value(), Some(250));
	assert_eq!(node_a.live_route_count(&nodes_a), Some(1));
	assert_eq!(node_b.live_route_count(&nodes_b), Some(1));
	assert_eq!(
		node_a.outgoing_target_keys(&nodes_a).as_deref(),
		Some(["lubeck".to_string()][..].as_ref())
	);
	assert_eq!(
		nodes_b
			.route_target_key({
				let (begin, _) = nodes_b.routes(node_b.definition).unwrap();
				begin
			})
			.as_deref(),
		Some("genoa")
	);

	let stamp_a = unsafe { game_a.campaign_stamp() }.unwrap();
	let stamp_b = unsafe { game_b.campaign_stamp() }.unwrap();
	assert_eq!(stamp_a.day, 20);
	assert_eq!(stamp_b.day, 30);
	assert!(stamp_a.campaign.starts_with("1111"));
	assert!(stamp_b.campaign.starts_with("2222"));
	let world_a = unsafe { game_a.read_world(1) }.unwrap();
	let world_b = unsafe { game_b.read_world(2) }.unwrap();
	assert_eq!(world_a.countries[&CountryId(0)].tag, "FRA");
	assert_eq!(world_b.countries[&CountryId(0)].tag, "ENG");
	assert_eq!(world_a.countries[&CountryId(0)].independent, Some(true));
	assert_eq!(world_b.countries[&CountryId(0)].independent, Some(false));
	assert_eq!(world_a.wars[0].id.started, 4);
	assert_eq!(world_b.wars[0].id.started, 11);
	assert_eq!(world_a.wars[0].id.serial, 0);
	assert_eq!(world_a.wars[0].id.declarer, CountryId(1));
	assert_eq!(world_b.wars[0].id.declared_on, CountryId(0));

	let expected_a = CountryRef {
		id: CountryId(0),
		tag: "FRA".into(),
	};
	let enemy_a = CountryRef {
		id: CountryId(1),
		tag: "HAB".into(),
	};
	let expected_b = CountryRef {
		id: CountryId(0),
		tag: "ENG".into(),
	};
	let enemy_b = CountryRef {
		id: CountryId(1),
		tag: "SWE".into(),
	};
	assert_eq!(
		unsafe { country_a.check_war_restrictions(&expected_a, &[enemy_a]) },
		Ok(())
	);
	assert_eq!(
		unsafe { country_b.check_war_restrictions(&expected_b, &[enemy_b]) },
		Err(JoinRefusal::Subject)
	);

	eprintln!(
		"experiment A measured treasury A=1050 B=2025 tag A=FRA B=ENG node A=100 B=250 tax A=12000 B=34000 war-day A=4 B=11 stamp-day A=20 B=30 key A=lubeck B=genoa route-stride A={:#x} B={:#x}",
		profile_a.route_size.offset, profile_b.route_size.offset
	);
}

#[test]
#[cfg(all(windows, target_arch = "x86_64"))]
fn experiment_a_monthly_tick_routes_by_digest() {
	use foch_plugin_sdk::*;
	use std::sync::Mutex;

	let profile_a = default_profile();
	let profile_b = synthetic_profile();
	struct Seen {
		pattern: String,
	}
	static SEEN: Mutex<Seen> = Mutex::new(Seen {
		pattern: String::new(),
	});
	unsafe extern "C" fn log(_: *const FochHostApi, _: i32, _: FochStr) {}
	unsafe extern "C" fn report(_: *const FochHostApi, _: *const FochStatus) {}
	unsafe extern "C" fn dir(_: *const FochHostApi, _: i32) -> FochStr {
		FochStr::EMPTY
	}
	unsafe extern "C" fn config(_: *const FochHostApi) -> FochStr {
		FochStr::EMPTY
	}
	unsafe extern "C" fn plugin_state(_: *const FochHostApi, _: FochStr) -> i32 {
		state::INACTIVE
	}
	unsafe extern "C" fn find(
		host: *const FochHostApi,
		pattern: FochStr,
		out: *mut *mut core::ffi::c_void,
	) -> i32 {
		let seen = unsafe { pattern.as_str() }.unwrap_or("");
		SEEN.lock().unwrap().pattern = seen.to_string();
		let base = unsafe { (*host).exe_base } as usize;
		let profile_a = default_profile();
		let profile_b = profile_for_build(&"b".repeat(64)).unwrap();
		if seen == profile_a.monthly_tick.pattern {
			unsafe { *out = base.wrapping_add(profile_a.monthly_tick.rva) as *mut _ };
			return result::OK;
		}
		if seen == profile_b.monthly_tick.pattern {
			unsafe { *out = base.wrapping_add(profile_b.monthly_tick.rva) as *mut _ };
			return result::OK;
		}
		result::E_INVALID
	}
	let host_for = |digest: &str| FochHostApi {
		size: size_of::<FochHostApi>() as u32,
		abi_major: ABI_MAJOR,
		abi_minor: ABI_MINOR,
		plugin_id: FochStr::from_str("dev.foch.profile"),
		run_id: FochStr::from_str("unit"),
		game_version: FochStr::from_str("synthetic"),
		exe_sha256: FochStr::from_str(digest),
		exe_base: 0x1400_0000 as *mut _,
		exe_size: 0x300_0000,
		log,
		report,
		dir,
		config_json: config,
		plugin_state,
		find_pattern: find,
		hook_create: unsupported_hook,
		hook_set_enabled: unsupported_hook_enable,
		patch_create: unsupported_patch,
		patch_set_enabled: unsupported_patch_enable,
	};
	unsafe extern "C" fn unsupported_hook(
		_: *const FochHostApi,
		_: *mut core::ffi::c_void,
		_: *mut core::ffi::c_void,
		_: i32,
		_: *mut *const *const core::ffi::c_void,
		_: *mut *mut FochHook,
	) -> i32 {
		result::E_UNSUPPORTED
	}
	unsafe extern "C" fn unsupported_hook_enable(
		_: *const FochHostApi,
		_: *mut FochHook,
		_: i32,
	) -> i32 {
		result::E_UNSUPPORTED
	}
	unsafe extern "C" fn unsupported_patch(
		_: *const FochHostApi,
		_: *mut core::ffi::c_void,
		_: *const u8,
		_: usize,
		_: *mut *mut FochPatch,
	) -> i32 {
		result::E_UNSUPPORTED
	}
	unsafe extern "C" fn unsupported_patch_enable(
		_: *const FochHostApi,
		_: *mut FochPatch,
		_: i32,
	) -> i32 {
		result::E_UNSUPPORTED
	}
	let digest_b = "b".repeat(64);
	let host_b = host_for(&digest_b);
	let tick_b = unsafe { super::super::resolve_monthly_tick(&host_b) }.unwrap();
	assert_eq!(tick_b as usize, 0x1400_0000 + profile_b.monthly_tick.rva);
	assert_eq!(SEEN.lock().unwrap().pattern, profile_b.monthly_tick.pattern);
	let digest_a = profile_a.exe_sha256.clone();
	let host_a = host_for(&digest_a);
	let tick_a = unsafe { super::super::resolve_monthly_tick(&host_a) }.unwrap();
	assert_eq!(tick_a as usize, 0x1400_0000 + profile_a.monthly_tick.rva);
	assert_eq!(SEEN.lock().unwrap().pattern, profile_a.monthly_tick.pattern);
	assert_ne!(tick_a, tick_b);
	eprintln!(
		"experiment A measured tick A={:#x} B={:#x}",
		tick_a as usize, tick_b as usize
	);
}
