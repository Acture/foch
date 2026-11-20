//! Compiled-in EU4 layout profiles.
//!
//! Production builds embed the reviewed JSON with `include_str!` and publish
//! one `&'static LayoutProfile` per executable digest through a [`OnceLock`].
//! Lookup is the exact lowercase `exe_sha256`. A version string is recorded
//! for humans and is never a match key. External unsigned JSON is not loaded.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};

use serde::Deserialize;

const EMBEDDED: &[&str] = &[include_str!("../../profiles/eu4/1.37.5-windows-x64.json")];

const FIELD_PATHS: &[&str] = &[
	"hooks.monthly_tick",
	"globals.context",
	"globals.country_manager",
	"globals.flag_names",
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
	"def.members",
	"def.routes",
	"def.routes_end",
	"route.size",
	"route.target_key",
	"route.target",
	"province.cprovinces",
	"province.cprovince_size",
	"province.manpower",
	"province.tax",
	"country_manager.index_array",
	"country.tag",
	"country.monthly_income_bank",
	"country.treasury",
	"country.overlord",
	"country.subjects_vec",
	"country.tribute_type",
	"ui.controller",
	"ui.flags",
	"ui.treasury_dirty",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FieldStatus {
	LiveVerified,
	StaticOnly,
}

#[derive(Debug)]
pub(crate) struct Field {
	pub offset: usize,
	pub status: FieldStatus,
	/// Review note carried by the JSON profile. Production reads use `offset`.
	#[cfg_attr(not(test), allow(dead_code))]
	pub evidence: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Hook {
	pub rva: usize,
	pub pattern: String,
	pub status: FieldStatus,
	/// Review note carried by the JSON profile. Resolution uses `rva` and `pattern`.
	#[cfg_attr(not(test), allow(dead_code))]
	pub evidence: Option<String>,
}

#[derive(Debug)]
pub(crate) struct LayoutProfile {
	/// Schema identity. Lookup never uses these strings; tests lock them.
	#[cfg_attr(not(test), allow(dead_code))]
	pub schema: u32,
	#[cfg_attr(not(test), allow(dead_code))]
	pub game: String,
	#[cfg_attr(not(test), allow(dead_code))]
	pub version: String,
	#[cfg_attr(not(test), allow(dead_code))]
	pub platform: String,
	#[cfg_attr(not(test), allow(dead_code))]
	pub abi_stl: String,
	pub pointer_size: usize,
	pub exe_sha256: String,
	pub monthly_tick: Hook,
	pub context_global: Field,
	pub country_manager_global: Field,
	pub flag_names: Field,
	pub context_db: Field,
	pub countries_begin: Field,
	pub countries_count: Field,
	pub db_nodes: Field,
	pub db_node_count: Field,
	pub node_entry_table: Field,
	pub node_entry_size: Field,
	pub node_weights: Field,
	pub node_definition: Field,
	pub node_value: Field,
	pub node_local_value: Field,
	pub node_transferred: Field,
	pub node_power: Field,
	pub entry_power: Field,
	pub entry_mode_collect: Field,
	pub entry_mode_transfer: Field,
	pub def_name: Field,
	pub def_members: Field,
	pub def_routes: Field,
	pub def_routes_end: Field,
	pub route_size: Field,
	pub route_target_key: Field,
	pub route_target: Field,
	pub province_cprovinces: Field,
	pub province_cprovince_size: Field,
	pub province_manpower: Field,
	pub province_tax: Field,
	pub country_manager_index_array: Field,
	pub country_tag: Field,
	pub country_monthly_income_bank: Field,
	pub country_treasury: Field,
	pub country_treasury_clamp: i64,
	pub country_overlord: Field,
	pub country_subjects_vec: Field,
	pub country_tribute_type: Field,
	pub ui_controller: Field,
	pub ui_flags: Field,
	pub ui_treasury_dirty: Field,
	pub campaign_guid: Field,
	pub campaign_current_hours: Field,
	pub campaign_calendar_epoch_hours: i64,
	pub war_active_wars: Field,
	pub war_array_counts: Field,
	pub war_max_wars: usize,
	pub war_started: Field,
	pub war_unset_hour: i32,
	pub war_unidentified_serial: u32,
	pub war_attackers: Field,
	pub war_defenders: Field,
	pub war_original_attacker: Field,
	pub war_original_defender: Field,
	pub diplomacy_country_relations: Field,
	pub diplomacy_relation_size: Field,
	pub diplomacy_truce_flags: Field,
	pub flags_country_flags: Field,
	pub flags_flag_ids: Field,
	pub flags_flag_dates: Field,
	pub flags_string_size: Field,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
	schema: u32,
	game: String,
	version: String,
	platform: String,
	abi: RawAbi,
	exe_sha256: String,
	hooks: RawHooks,
	globals: RawGlobals,
	structs: RawStructs,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAbi {
	stl: String,
	pointer_size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHooks {
	monthly_tick: RawHook,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHook {
	rva: String,
	pattern: String,
	status: String,
	#[serde(default)]
	evidence: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGlobals {
	context: RawField,
	country_manager: RawField,
	flag_names: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStructs {
	context: RawContext,
	db: RawDb,
	node: RawNode,
	entry: RawEntry,
	#[serde(rename = "def")]
	definition: RawDefinition,
	route: RawRoute,
	province: RawProvince,
	country_manager: RawCountryManager,
	country: RawCountry,
	ui: RawUi,
	campaign: RawCampaign,
	war: RawWar,
	diplomacy: RawDiplomacy,
	flags: RawFlags,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
	offset: String,
	status: String,
	#[serde(default)]
	evidence: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContext {
	db: RawField,
	countries_begin: RawField,
	countries_count: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDb {
	nodes: RawField,
	node_count: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
	entry_table: RawField,
	entry_size: RawField,
	weights: RawField,
	definition: RawField,
	value: RawField,
	local_value: RawField,
	transferred: RawField,
	power: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
	power: RawField,
	mode_collect: RawField,
	mode_transfer: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefinition {
	name: RawField,
	members: RawField,
	routes: RawField,
	routes_end: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
	size: RawField,
	target_key: RawField,
	target: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvince {
	cprovinces: RawField,
	cprovince_size: RawField,
	manpower: RawField,
	tax: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCountryManager {
	index_array: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCountry {
	tag: RawField,
	monthly_income_bank: RawField,
	treasury: RawField,
	treasury_clamp: i64,
	overlord: RawField,
	subjects_vec: RawField,
	tribute_type: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUi {
	controller: RawField,
	flags: RawField,
	treasury_dirty: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCampaign {
	guid: RawField,
	current_hours: RawField,
	calendar_epoch_hours: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWar {
	active_wars: RawField,
	array_counts: RawField,
	max_wars: usize,
	started: RawField,
	unset_hour: String,
	unidentified_serial: u32,
	attackers: RawField,
	defenders: RawField,
	original_attacker: RawField,
	original_defender: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDiplomacy {
	country_relations: RawField,
	relation_size: RawField,
	truce_flags: RawField,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlags {
	country_flags: RawField,
	flag_ids: RawField,
	flag_dates: RawField,
	string_size: RawField,
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
	mutex
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

static BINDINGS: LazyLock<Mutex<HashMap<(usize, usize), &'static LayoutProfile>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
static EXTRA: Mutex<Vec<&'static LayoutProfile>> = Mutex::new(Vec::new());

fn registry() -> &'static [LayoutProfile] {
	static REGISTRY: OnceLock<Vec<LayoutProfile>> = OnceLock::new();
	REGISTRY.get_or_init(|| {
		let mut profiles: Vec<LayoutProfile> = Vec::with_capacity(EMBEDDED.len());
		for text in EMBEDDED {
			let profile = parse_profile(text).expect("embedded EU4 layout profile is invalid");
			if profiles
				.iter()
				.any(|have| have.exe_sha256 == profile.exe_sha256)
			{
				panic!("embedded layout profiles repeat {}", profile.exe_sha256);
			}
			for path in FIELD_PATHS {
				if super::layout::profile_field_status(&profile, path).is_none() {
					panic!("embedded profile is missing {path}");
				}
			}
			profiles.push(profile);
		}
		assert!(!profiles.is_empty(), "no embedded layout profile");
		profiles
	})
}

pub(crate) fn default_profile() -> &'static LayoutProfile {
	&registry()[0]
}

pub(crate) fn profile_for_build(exe_sha256: &str) -> Option<&'static LayoutProfile> {
	if let Some(profile) = registry()
		.iter()
		.find(|profile| profile.exe_sha256 == exe_sha256)
	{
		return Some(profile);
	}
	#[cfg(test)]
	{
		return lock(&EXTRA)
			.iter()
			.copied()
			.find(|profile| profile.exe_sha256 == exe_sha256);
	}
	#[cfg(not(test))]
	None
}

pub(crate) fn bind_game(exe_base: usize, context: usize, profile: &'static LayoutProfile) {
	lock(&*BINDINGS).insert((exe_base, context), profile);
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn unbind_game(exe_base: usize, context: usize) {
	lock(&*BINDINGS).remove(&(exe_base, context));
}

pub(crate) fn bound_profile(exe_base: usize, context: usize) -> &'static LayoutProfile {
	lock(&*BINDINGS)
		.get(&(exe_base, context))
		.copied()
		.unwrap_or_else(default_profile)
}

#[cfg(test)]
pub(crate) fn install_test_profile(profile: &'static LayoutProfile) {
	let mut extra = lock(&EXTRA);
	if extra
		.iter()
		.any(|have| have.exe_sha256 == profile.exe_sha256)
	{
		return;
	}
	extra.push(profile);
}

pub(crate) fn parse_profile(text: &str) -> Result<LayoutProfile, String> {
	let raw: RawProfile =
		serde_json::from_str(text).map_err(|error| format!("layout profile schema: {error}"))?;
	if raw.schema != 1 {
		return Err(format!("unsupported layout schema {}", raw.schema));
	}
	if raw.game != "eu4" {
		return Err(format!("unsupported game {}", raw.game));
	}
	if raw.version.is_empty() || raw.platform.is_empty() {
		return Err("version and platform must be non-empty".into());
	}
	if raw.abi.stl != "msvc_x64" {
		return Err(format!("unsupported abi.stl {}", raw.abi.stl));
	}
	if raw.abi.pointer_size != 8 {
		return Err("msvc_x64 requires pointer_size 8".into());
	}
	if raw.structs.country.treasury_clamp <= 0 {
		return Err("country.treasury_clamp must be positive".into());
	}
	if !(1..=4096).contains(&raw.structs.war.max_wars) {
		return Err("war.max_wars must stay inside 1..=4096".into());
	}
	if raw.hooks.monthly_tick.pattern.trim().is_empty() {
		return Err("hooks.monthly_tick pattern must be non-empty".into());
	}
	let profile = LayoutProfile {
		schema: raw.schema,
		game: raw.game,
		version: raw.version,
		platform: raw.platform,
		abi_stl: raw.abi.stl,
		pointer_size: raw.abi.pointer_size as usize,
		exe_sha256: parse_sha(&raw.exe_sha256)?,
		monthly_tick: hook("hooks.monthly_tick", raw.hooks.monthly_tick)?,
		context_global: field("globals.context", raw.globals.context)?,
		country_manager_global: field("globals.country_manager", raw.globals.country_manager)?,
		flag_names: field("globals.flag_names", raw.globals.flag_names)?,
		context_db: field("context.db", raw.structs.context.db)?,
		countries_begin: field(
			"context.countries_begin",
			raw.structs.context.countries_begin,
		)?,
		countries_count: field(
			"context.countries_count",
			raw.structs.context.countries_count,
		)?,
		db_nodes: field("db.nodes", raw.structs.db.nodes)?,
		db_node_count: field("db.node_count", raw.structs.db.node_count)?,
		node_entry_table: field("node.entry_table", raw.structs.node.entry_table)?,
		node_entry_size: field("node.entry_size", raw.structs.node.entry_size)?,
		node_weights: field("node.weights", raw.structs.node.weights)?,
		node_definition: field("node.definition", raw.structs.node.definition)?,
		node_value: field("node.value", raw.structs.node.value)?,
		node_local_value: field("node.local_value", raw.structs.node.local_value)?,
		node_transferred: field("node.transferred", raw.structs.node.transferred)?,
		node_power: field("node.power", raw.structs.node.power)?,
		entry_power: field("entry.power", raw.structs.entry.power)?,
		entry_mode_collect: field("entry.mode_collect", raw.structs.entry.mode_collect)?,
		entry_mode_transfer: field("entry.mode_transfer", raw.structs.entry.mode_transfer)?,
		def_name: field("def.name", raw.structs.definition.name)?,
		def_members: field("def.members", raw.structs.definition.members)?,
		def_routes: field("def.routes", raw.structs.definition.routes)?,
		def_routes_end: field("def.routes_end", raw.structs.definition.routes_end)?,
		route_size: field("route.size", raw.structs.route.size)?,
		route_target_key: field("route.target_key", raw.structs.route.target_key)?,
		route_target: field("route.target", raw.structs.route.target)?,
		province_cprovinces: field("province.cprovinces", raw.structs.province.cprovinces)?,
		province_cprovince_size: field(
			"province.cprovince_size",
			raw.structs.province.cprovince_size,
		)?,
		province_manpower: field("province.manpower", raw.structs.province.manpower)?,
		province_tax: field("province.tax", raw.structs.province.tax)?,
		country_manager_index_array: field(
			"country_manager.index_array",
			raw.structs.country_manager.index_array,
		)?,
		country_tag: field("country.tag", raw.structs.country.tag)?,
		country_monthly_income_bank: field(
			"country.monthly_income_bank",
			raw.structs.country.monthly_income_bank,
		)?,
		country_treasury: field("country.treasury", raw.structs.country.treasury)?,
		country_treasury_clamp: raw.structs.country.treasury_clamp,
		country_overlord: field("country.overlord", raw.structs.country.overlord)?,
		country_subjects_vec: field("country.subjects_vec", raw.structs.country.subjects_vec)?,
		country_tribute_type: field("country.tribute_type", raw.structs.country.tribute_type)?,
		ui_controller: field("ui.controller", raw.structs.ui.controller)?,
		ui_flags: field("ui.flags", raw.structs.ui.flags)?,
		ui_treasury_dirty: field("ui.treasury_dirty", raw.structs.ui.treasury_dirty)?,
		campaign_guid: field("campaign.guid", raw.structs.campaign.guid)?,
		campaign_current_hours: field(
			"campaign.current_hours",
			raw.structs.campaign.current_hours,
		)?,
		campaign_calendar_epoch_hours: raw.structs.campaign.calendar_epoch_hours,
		war_active_wars: field("war.active_wars", raw.structs.war.active_wars)?,
		war_array_counts: field("war.array_counts", raw.structs.war.array_counts)?,
		war_max_wars: raw.structs.war.max_wars,
		war_started: field("war.started", raw.structs.war.started)?,
		war_unset_hour: parse_i32_hex("war.unset_hour", &raw.structs.war.unset_hour)?,
		war_unidentified_serial: raw.structs.war.unidentified_serial,
		war_attackers: field("war.attackers", raw.structs.war.attackers)?,
		war_defenders: field("war.defenders", raw.structs.war.defenders)?,
		war_original_attacker: field("war.original_attacker", raw.structs.war.original_attacker)?,
		war_original_defender: field("war.original_defender", raw.structs.war.original_defender)?,
		diplomacy_country_relations: field(
			"diplomacy.country_relations",
			raw.structs.diplomacy.country_relations,
		)?,
		diplomacy_relation_size: field(
			"diplomacy.relation_size",
			raw.structs.diplomacy.relation_size,
		)?,
		diplomacy_truce_flags: field("diplomacy.truce_flags", raw.structs.diplomacy.truce_flags)?,
		flags_country_flags: field("flags.country_flags", raw.structs.flags.country_flags)?,
		flags_flag_ids: field("flags.flag_ids", raw.structs.flags.flag_ids)?,
		flags_flag_dates: field("flags.flag_dates", raw.structs.flags.flag_dates)?,
		flags_string_size: field("flags.string_size", raw.structs.flags.string_size)?,
	};
	require_stride("node.entry_size", profile.node_entry_size.offset)?;
	require_stride("route.size", profile.route_size.offset)?;
	require_stride(
		"province.cprovince_size",
		profile.province_cprovince_size.offset,
	)?;
	require_stride(
		"diplomacy.relation_size",
		profile.diplomacy_relation_size.offset,
	)?;
	require_stride("flags.string_size", profile.flags_string_size.offset)?;
	require_stride("abi.pointer_size", profile.pointer_size)?;
	Ok(profile)
}

fn parse_sha(text: &str) -> Result<String, String> {
	if text.len() == 64
		&& text
			.bytes()
			.all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
	{
		Ok(text.to_string())
	} else {
		Err("exe_sha256 must be 64 lowercase hex characters".into())
	}
}

fn parse_hex(label: &str, text: &str) -> Result<usize, String> {
	let Some(digits) = text.strip_prefix("0x") else {
		return Err(format!("{label} must be a lowercase 0x hex offset"));
	};
	if digits.is_empty()
		|| !digits
			.bytes()
			.all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
	{
		return Err(format!("{label} must be a lowercase 0x hex offset"));
	}
	usize::from_str_radix(digits, 16).map_err(|_| format!("{label} hex offset overflows"))
}

fn parse_i32_hex(label: &str, text: &str) -> Result<i32, String> {
	let value = parse_hex(label, text)?;
	i32::try_from(value).map_err(|_| format!("{label} does not fit an i32"))
}

fn parse_status(label: &str, text: &str) -> Result<FieldStatus, String> {
	match text {
		"live_verified" => Ok(FieldStatus::LiveVerified),
		"static_only" => Ok(FieldStatus::StaticOnly),
		_ => Err(format!(
			"{label} status is not live_verified or static_only"
		)),
	}
}

fn field(label: &str, raw: RawField) -> Result<Field, String> {
	Ok(Field {
		offset: parse_hex(label, &raw.offset)?,
		status: parse_status(label, &raw.status)?,
		evidence: raw.evidence,
	})
}

fn hook(label: &str, raw: RawHook) -> Result<Hook, String> {
	Ok(Hook {
		rva: parse_hex(label, &raw.rva)?,
		pattern: raw.pattern,
		status: parse_status(label, &raw.status)?,
		evidence: raw.evidence,
	})
}

fn require_stride(label: &str, value: usize) -> Result<(), String> {
	if value == 0 {
		Err(format!("{label} stride must be non-zero"))
	} else {
		Ok(())
	}
}

pub(crate) fn profile_field_status(profile: &LayoutProfile, path: &str) -> Option<FieldStatus> {
	if path == "hooks.monthly_tick" {
		return Some(profile.monthly_tick.status);
	}
	Some(profile_field(profile, path)?.status)
}

fn profile_field<'a>(profile: &'a LayoutProfile, path: &str) -> Option<&'a Field> {
	Some(match path {
		"globals.context" => &profile.context_global,
		"globals.country_manager" => &profile.country_manager_global,
		"globals.flag_names" => &profile.flag_names,
		"context.db" => &profile.context_db,
		"context.countries_begin" => &profile.countries_begin,
		"context.countries_count" => &profile.countries_count,
		"db.nodes" => &profile.db_nodes,
		"db.node_count" => &profile.db_node_count,
		"node.entry_table" => &profile.node_entry_table,
		"node.entry_size" => &profile.node_entry_size,
		"node.weights" => &profile.node_weights,
		"node.definition" => &profile.node_definition,
		"node.value" => &profile.node_value,
		"node.local_value" => &profile.node_local_value,
		"node.transferred" => &profile.node_transferred,
		"node.power" => &profile.node_power,
		"entry.power" => &profile.entry_power,
		"entry.mode_collect" => &profile.entry_mode_collect,
		"entry.mode_transfer" => &profile.entry_mode_transfer,
		"def.name" => &profile.def_name,
		"def.members" => &profile.def_members,
		"def.routes" => &profile.def_routes,
		"def.routes_end" => &profile.def_routes_end,
		"route.size" => &profile.route_size,
		"route.target_key" => &profile.route_target_key,
		"route.target" => &profile.route_target,
		"province.cprovinces" => &profile.province_cprovinces,
		"province.cprovince_size" => &profile.province_cprovince_size,
		"province.manpower" => &profile.province_manpower,
		"province.tax" => &profile.province_tax,
		"country_manager.index_array" => &profile.country_manager_index_array,
		"country.tag" => &profile.country_tag,
		"country.monthly_income_bank" => &profile.country_monthly_income_bank,
		"country.treasury" => &profile.country_treasury,
		"country.overlord" => &profile.country_overlord,
		"country.subjects_vec" => &profile.country_subjects_vec,
		"country.tribute_type" => &profile.country_tribute_type,
		"ui.controller" => &profile.ui_controller,
		"ui.flags" => &profile.ui_flags,
		"ui.treasury_dirty" => &profile.ui_treasury_dirty,
		"campaign.guid" => &profile.campaign_guid,
		"campaign.current_hours" => &profile.campaign_current_hours,
		"war.active_wars" => &profile.war_active_wars,
		"war.array_counts" => &profile.war_array_counts,
		"war.started" => &profile.war_started,
		"war.attackers" => &profile.war_attackers,
		"war.defenders" => &profile.war_defenders,
		"war.original_attacker" => &profile.war_original_attacker,
		"war.original_defender" => &profile.war_original_defender,
		"diplomacy.country_relations" => &profile.diplomacy_country_relations,
		"diplomacy.relation_size" => &profile.diplomacy_relation_size,
		"diplomacy.truce_flags" => &profile.diplomacy_truce_flags,
		"flags.country_flags" => &profile.flags_country_flags,
		"flags.flag_ids" => &profile.flags_flag_ids,
		"flags.flag_dates" => &profile.flags_flag_dates,
		"flags.string_size" => &profile.flags_string_size,
		_ => return None,
	})
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;
