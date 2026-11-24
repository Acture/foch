//! Typed views over the live EU4 structures.
//!
//! Thin by design: each view reads the layout's fields and nothing
//! more; semantics (what a retention rate means, which aggregate makes
//! a sink) belong to the plugins. All values keep the engine's units —
//! thousandths — and every fallible read is an `Option`.

use super::profile::{LayoutProfile, bind_game, bound_profile, default_profile, profile_for_build};
use super::war::{CampaignStamp, CountryRef, WarRoster};
use super::{Memory, StdVector, is_key_byte, is_name_byte, read_sso};
use crate::war::{JoinRefusal, World};

/// The live game: an image base and the global game-context pointer it
/// holds. Construct one from the host's reported base, or from any
/// hook's first argument after cross-checking it against the global.
pub struct Eu4Game {
	pub memory: Memory,
	pub exe_base: usize,
	pub context: usize,
}

impl Eu4Game {
	/// Cross-check a candidate context against the global pointer
	/// variable; `None` when the image base is unknown or the candidate
	/// is not the global.
	pub fn context_of(memory: &Memory, exe_base: usize, candidate: usize) -> Option<usize> {
		context_matches(memory, default_profile(), exe_base, candidate)
	}

	fn profile(&self) -> &'static LayoutProfile {
		bound_profile(self.exe_base, self.context)
	}

	/// Read the pinned global context pointer and build a game view.
	///
	/// Hooks that are not passed the game context use this instead of knowing
	/// the context slot's image offset. `None` when the image base is
	/// implausible or the stored pointer is not a plausible context address.
	pub fn from_exe_base(exe_base: usize) -> Option<Self> {
		Self::from_exe_base_for_build(default_profile().exe_sha256.as_str(), exe_base)
	}

	/// Read the global context pointer of one compiled-in profile.
	///
	/// `None` when `exe_sha256` is not an exact registry key, the image base
	/// is implausible, or the stored pointer is not a plausible context.
	pub fn from_exe_base_for_build(exe_sha256: &str, exe_base: usize) -> Option<Self> {
		let profile = profile_for_build(exe_sha256)?;
		if exe_base <= 0x1_0000 {
			return None;
		}
		let memory = Memory;
		let slot = exe_base.checked_add(profile.context_global.offset)?;
		let context = memory.read_ptr(slot)?;
		bind_game(exe_base, context, profile);
		Some(Self {
			memory,
			exe_base,
			context,
		})
	}

	/// A game view whose tick argument matches the pinned global context pointer.
	pub fn from_tick(exe_base: usize, candidate_context: usize) -> Option<Self> {
		Self::from_tick_for_build(
			default_profile().exe_sha256.as_str(),
			exe_base,
			candidate_context,
		)
	}

	/// A game view for one compiled-in profile, selected by exact executable digest.
	///
	/// `None` when the digest is unknown, the image base is implausible, or
	/// `candidate_context` is not that profile's global context pointer.
	/// Later reads on this view use that profile's offsets.
	pub fn from_tick_for_build(
		exe_sha256: &str,
		exe_base: usize,
		candidate_context: usize,
	) -> Option<Self> {
		let profile = profile_for_build(exe_sha256)?;
		let memory = Memory;
		let context = context_matches(&memory, profile, exe_base, candidate_context)?;
		bind_game(exe_base, context, profile);
		Some(Self {
			memory,
			exe_base,
			context,
		})
	}

	/// Campaign GUID and calendar day for this context.
	///
	/// # Safety
	/// As [`CampaignStamp::read`] for this view's live game context. The caller
	/// keeps that context stable on the game thread for the whole read. These
	/// fields are statically identified and have not passed a live Foch session
	/// probe.
	pub unsafe fn campaign_stamp(&self) -> Result<CampaignStamp, String> {
		// SAFETY: the caller upholds CampaignStamp::read for this context.
		unsafe { CampaignStamp::read_with(self.profile(), &self.memory, self.context) }
	}

	/// Active wars in this context. Ended-war history is not included.
	///
	/// # Safety
	/// As [`WarRoster::read_active`] for this view's live game context, its
	/// active-war array, and every `CWar` those pointers name. The caller runs
	/// on the game thread. War fields are statically identified and have not
	/// passed a live Foch session probe.
	pub unsafe fn active_wars(&self) -> Result<Vec<WarRoster>, String> {
		// SAFETY: the caller upholds WarRoster::read_active for this context.
		unsafe { WarRoster::read_active_with(self.profile(), &self.memory, self.context) }
	}

	/// One coherent campaign snapshot from this view's registry, wars, and stamp.
	///
	/// `revision` is the caller's monotonically increasing snapshot number.
	/// Slot 0's `---` sentinel is skipped. Country counts outside `1..=i16::MAX`
	/// and trade-node counts outside `1..=4096` stay refused by the registry
	/// readers this calls.
	///
	/// # Safety
	/// As [`read_world`](super::war::read_world) for this view's image base and
	/// live context. The caller holds the game thread, with the context, country
	/// registry, and every active `CWar` stable for the read.
	pub unsafe fn read_world(&self, revision: u64) -> Result<World, String> {
		// SAFETY: the caller upholds read_world for this image and context.
		unsafe {
			super::war::read_world_with(
				self.profile(),
				&self.memory,
				self.exe_base,
				self.context,
				revision,
			)
		}
	}

	/// Add `amount` thousandths to the treasury of a registry country.
	///
	/// # Safety
	/// As [`CountryView::pay`] for the country at `country_index` and this game
	/// context. The caller runs on the game thread and keeps that country and
	/// any UI controller alive for the write.
	pub unsafe fn pay_country(&self, country_index: usize, amount: i64) -> Option<()> {
		let country = self.countries()?.country(country_index)?;
		// SAFETY: the country comes from this view's registry and the context
		// is the one the caller already bound to the game thread.
		unsafe { country.pay(&self.memory, self.context, amount) }
	}

	/// Candidate country-flag snapshot for a registry country.
	///
	/// # Safety
	/// As [`CountryView::flags`]. `host_exe_sha256` is the host-computed digest,
	/// not a substitute constant. Flag fields are static-only and have not
	/// passed a live Foch session probe.
	pub unsafe fn country_flags(
		&self,
		country: &CountryView,
		host_exe_sha256: &str,
	) -> Result<super::CountryFlags, super::FlagReadError> {
		// SAFETY: the caller upholds CountryView::flags for this image base.
		unsafe { country.flags(self.exe_base, host_exe_sha256) }
	}

	/// The country registry: index-to-`CCountry*` array and count.
	pub fn countries(&self) -> Option<Countries> {
		self.countries_with(self.profile())
	}

	pub(crate) fn countries_with(&self, profile: &'static LayoutProfile) -> Option<Countries> {
		let manager = self.memory.read_ptr(
			self.exe_base
				.checked_add(profile.country_manager_global.offset)?,
		)?;
		let pointers = self
			.memory
			.read_ptr(manager.checked_add(profile.country_manager_index_array.offset)?)?;
		let count = self
			.memory
			.read_i32(self.context.checked_add(profile.countries_count.offset)?)
			.and_then(valid_country_count)?;
		Some(Countries {
			memory: self.memory,
			pointers,
			count,
			profile,
		})
	}

	/// The trade node database embedded in the context.
	pub fn trade_nodes(&self) -> Option<TradeNodes> {
		let profile = self.profile();
		let db = self.context.checked_add(profile.context_db.offset)?;
		let nodes = self
			.memory
			.read_ptr(db.checked_add(profile.db_nodes.offset)?)?;
		let count = self
			.memory
			.read_i32(db.checked_add(profile.db_node_count.offset)?)
			.and_then(valid_trade_node_count)?;
		Some(TradeNodes {
			memory: self.memory,
			db,
			nodes,
			count,
			profile,
		})
	}

	/// The province array, indexed by province id.
	pub fn province(&self, id: u32) -> Option<Province> {
		let profile = self.profile();
		let field = self
			.context
			.checked_add(profile.province_cprovinces.offset)?;
		let base = self.memory.read_ptr(field)?;
		let address = usize::try_from(id)
			.ok()?
			.checked_mul(profile.province_cprovince_size.offset)
			.and_then(|offset| base.checked_add(offset))?;
		Some(Province {
			memory: self.memory,
			address,
			profile,
		})
	}
}

fn context_matches(
	memory: &Memory,
	profile: &LayoutProfile,
	exe_base: usize,
	candidate: usize,
) -> Option<usize> {
	if exe_base <= 0x1_0000 {
		return None;
	}
	let slot = exe_base.checked_add(profile.context_global.offset)?;
	let stored = memory.read_ptr(slot)?;
	(stored == candidate).then_some(stored)
}

fn route_key(profile: &LayoutProfile, memory: &Memory, route_addr: usize) -> Option<String> {
	let key_at = route_addr.checked_add(profile.route_target_key.offset)?;
	read_sso(memory, key_at, is_key_byte)
}

/// The trade node database.
pub struct TradeNodes {
	memory: Memory,
	#[allow(dead_code)]
	db: usize,
	nodes: usize,
	count: usize,
	profile: &'static LayoutProfile,
}

impl TradeNodes {
	pub fn count(&self) -> usize {
		self.count
	}

	/// The heap `CTradeNode*` at the given engine index.
	pub fn node(&self, index: usize) -> Option<NodeView> {
		let address = self
			.memory
			.read_ptr(self.nodes + index * self.profile.pointer_size)?;
		let definition = self
			.memory
			.read_ptr(address.checked_add(self.profile.node_definition.offset)?)?;
		Some(NodeView {
			memory: self.memory,
			address,
			definition,
			profile: self.profile,
		})
	}

	/// The route vector of a node definition: `(begin, end)`. An empty
	/// vector (`begin == end`, both null or above the null page) is legal —
	/// vanilla end nodes have a default-constructed MSVC vector (`nullptr`,
	/// `nullptr`) with no outgoing routes at all.
	pub fn routes(&self, definition: usize) -> Option<(usize, usize)> {
		let stride = self.profile.route_size.offset;
		if stride == 0 {
			return None;
		}
		let begin =
			self.memory
				.read_qword(definition.checked_add(self.profile.def_routes.offset)?)? as usize;
		let end =
			self.memory
				.read_qword(definition.checked_add(self.profile.def_routes_end.offset)?)? as usize;
		if begin == end && (begin == 0 || begin > 0x1_0000) {
			return Some((begin, end));
		}
		(begin > 0x1_0000 && end >= begin && (end - begin).is_multiple_of(stride))
			.then_some((begin, end))
	}

	/// Internal node key recorded on the route entry at `route_addr`.
	///
	/// This is the MSVC SSO string the offline `node_members.txt` table is
	/// keyed by. Its verification status is `route.target_key` on the profile.
	pub fn route_target_key(&self, route_addr: usize) -> Option<String> {
		route_key(self.profile, &self.memory, route_addr)
	}
}

/// One live trade node plus its definition.
pub struct NodeView {
	memory: Memory,
	pub address: usize,
	pub definition: usize,
	profile: &'static LayoutProfile,
}

impl NodeView {
	fn i32(&self, offset: usize) -> Option<i32> {
		self.memory.read_i32(self.address + offset)
	}

	/// Current total trade value (thousandths).
	pub fn value(&self) -> Option<i32> {
		self.i32(self.profile.node_value.offset)
	}

	/// Local trade value: the production-side injection (goods x
	/// price), thousandths.
	pub fn local_value(&self) -> Option<i32> {
		self.i32(self.profile.node_local_value.offset)
	}

	/// The amount transferred out this tick (thousandths); the engine's
	/// own retention rate is `value / (value + transferred)`.
	pub fn transferred(&self) -> Option<i32> {
		self.i32(self.profile.node_transferred.offset)
	}

	/// Total trade power (thousandths).
	pub fn power(&self) -> Option<i32> {
		self.i32(self.profile.node_power.offset)
	}

	/// The per-outgoing-edge steering weights, in the live slot order.
	pub fn steering(&self) -> Vec<f64> {
		match self
			.memory
			.read_ptr(self.address + self.profile.node_weights.offset)
		{
			Some(begin) => match self
				.memory
				.read_qword(self.address + self.profile.node_weights.offset + 8)
			{
				Some(end) if end as usize >= begin => (0..(end as usize - begin) / 4)
					.filter_map(|i| self.memory.read_i32(begin + i * 4))
					.map(|v| v as f64)
					.collect(),
				_ => Vec::new(),
			},
			None => Vec::new(),
		}
	}

	/// The display name (`Great Lakes`, `Lübeck`).
	pub fn name(&self) -> Option<String> {
		read_sso(
			&self.memory,
			self.definition + self.profile.def_name.offset,
			is_name_byte,
		)
	}

	/// Member province IDs (`members = { ... }`) read from
	/// `CTradeNodeDefinition`'s member-province vector (`std::vector<i32>`).
	///
	/// Statically identified for EU4 1.37.5; pending live Foch session
	/// verification. Returns `Some(Vec::new())` for an empty vector, and
	/// `None` if the vector header, buffer range, or any province ID
	/// (`<= 0`) fails validation.
	pub fn member_provinces(&self) -> Option<Vec<u32>> {
		let header = StdVector::read(
			&self.memory,
			self.definition
				.checked_add(self.profile.def_members.offset)?,
		)?;
		let count = header.len(size_of::<i32>(), MAX_NODE_MEMBERS)?;
		if count == 0 {
			return Some(Vec::new());
		}
		let byte_len = count.checked_mul(size_of::<i32>())?;
		let mut raw = vec![0_u8; byte_len];
		self.memory.read_bytes(header.begin, &mut raw)?;
		let mut ids = Vec::with_capacity(count);
		for chunk in raw.chunks_exact(size_of::<i32>()) {
			let id = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
			let province_id = u32::try_from(id).ok().filter(|&p| p > 0)?;
			ids.push(province_id);
		}
		Some(ids)
	}

	/// The target definition pointer of the route entry at `slot`.
	pub fn route_target(&self, slot: usize) -> Option<usize> {
		self.memory
			.read_ptr(slot.checked_add(self.profile.route_target.offset)?)
	}

	/// Internal node key of the route entry at `slot`.
	///
	/// Same string as [`TradeNodes::route_target_key`]. `node_members.txt`
	/// rows are keyed by it.
	pub fn route_target_key(&self, slot: usize) -> Option<String> {
		route_key(self.profile, &self.memory, slot)
	}

	/// Internal keys of this node's live outgoing routes, in vector order.
	pub fn outgoing_target_keys(&self, nodes: &TradeNodes) -> Option<Vec<String>> {
		let (begin, end) = nodes.routes(self.definition)?;
		let stride = self.profile.route_size.offset;
		if stride == 0 {
			return None;
		}
		let count = end.checked_sub(begin)? / stride;
		let mut keys = Vec::with_capacity(count);
		for slot in 0..count {
			let route_addr = begin.checked_add(slot.checked_mul(stride)?)?;
			keys.push(self.route_target_key(route_addr)?);
		}
		Some(keys)
	}

	/// The per-country entry table: `entry = *(node+ENTRY_TABLE) +
	/// country_index * ENTRY_SIZE`.
	pub fn country_entry(&self, country_index: usize) -> Option<usize> {
		let table = self.memory.read_ptr(
			self.address
				.checked_add(self.profile.node_entry_table.offset)?,
		)?;
		table.checked_add(country_index.checked_mul(self.profile.node_entry_size.offset)?)
	}

	/// A country's trade power contribution inside this node
	/// (thousandths).
	pub fn entry_power(&self, country_index: usize) -> Option<i32> {
		let entry = self.country_entry(country_index)?;
		self.memory
			.read_i32(entry.checked_add(self.profile.entry_power.offset)?)
	}

	/// Object address of this live trade node.
	///
	/// Candidate experiments anchor unverified fields here. Route and entry
	/// offsets stay inside the data plane.
	pub fn raw_address(&self) -> usize {
		self.address
	}

	/// Live outgoing route count, from the definition's route vector.
	///
	/// An empty vector is `Some(0)`. A corrupt header is `None`.
	pub fn live_route_count(&self, nodes: &TradeNodes) -> Option<usize> {
		let (begin, end) = nodes.routes(self.definition)?;
		let stride = self.profile.route_size.offset;
		if stride == 0 {
			return None;
		}
		Some(end.checked_sub(begin)? / stride)
	}

	/// Engine indexes of the nodes this node's live routes steer toward.
	///
	/// Indexes follow the live route vector. `None` when a route header is
	/// corrupt or a target definition is not one of `nodes`.
	pub fn outgoing_targets(&self, nodes: &TradeNodes) -> Option<Vec<usize>> {
		let (begin, end) = nodes.routes(self.definition)?;
		let stride = self.profile.route_size.offset;
		if stride == 0 {
			return None;
		}
		let count = end.checked_sub(begin)? / stride;
		let mut targets = Vec::with_capacity(count);
		for slot in 0..count {
			let route_addr = begin.checked_add(slot.checked_mul(stride)?)?;
			let target = self.route_target(route_addr)?;
			let index = (0..nodes.count).find(|&index| {
				nodes
					.node(index)
					.is_some_and(|node| node.definition == target)
			})?;
			targets.push(index);
		}
		Some(targets)
	}

	/// Registry countries whose entry power reads, including zero.
	///
	/// Each pair is `(country index, power thousandths)`. An index whose entry
	/// cannot be read is omitted; the rest keep registry order.
	pub fn participants(&self, countries: &Countries) -> Vec<(usize, i32)> {
		(0..countries.count())
			.filter_map(|index| self.entry_power(index).map(|power| (index, power)))
			.collect()
	}

	/// Capture this node's vanilla routes without the caller supplying
	/// route-vector offsets.
	pub fn capture_routes(&self, nodes: &TradeNodes) -> Option<super::RouteMutation> {
		let routes = nodes.routes(self.definition)?;
		super::RouteMutation::capture_with(
			self.profile,
			&self.memory,
			self.address,
			self.definition,
			routes,
		)
	}
}

/// Decoded EU4 packed country identity (`[tag; 3], 0, index: i16, _, valid_flag`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedCountryTag {
	pub tag: String,
	pub index: u16,
}

/// Decodes an EU4 packed country identity qword.
///
/// The low three bytes must be ASCII uppercase letters or digits, byte 3 must
/// be NUL, and bytes 4..6 must hold a non-negative signed 16-bit country index.
/// When `require_valid_flag` is `true`, byte 7 must also be non-zero (used for
/// optional references such as `country::OVERLORD` and war participants).
pub fn decode_packed_tag(packed: u64, require_valid_flag: bool) -> Option<PackedCountryTag> {
	let bytes = packed.to_le_bytes();
	let index = i16::from_le_bytes([bytes[4], bytes[5]]);
	if (require_valid_flag && bytes[7] == 0)
		|| bytes[3] != 0
		|| index < 0
		|| !bytes[..3]
			.iter()
			.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
	{
		return None;
	}
	let tag = String::from_utf8(bytes[..3].to_vec()).ok()?;
	Some(PackedCountryTag {
		tag,
		index: index as u16,
	})
}

/// One country.
pub struct CountryView {
	memory: Memory,
	address: usize,
	profile: &'static LayoutProfile,
}

impl CountryView {
	/// Constructs a country view from a raw `CCountry*` address after checking
	/// basic pointer plausibility (above the null page and 8-byte aligned).
	pub fn from_ptr(memory: Memory, address: usize) -> Option<CountryView> {
		(address > 0x1_0000 && address.is_multiple_of(8)).then_some(CountryView {
			memory,
			address,
			profile: default_profile(),
		})
	}

	/// Read just the tag of the country at `address`, without the full
	/// view (the registry holds the addresses already).
	///
	/// Uses the default compiled-in profile. A view built from
	/// [`Eu4Game::from_tick_for_build`] reads its own profile via [`Self::tag`].
	pub fn tag_at(memory: &Memory, address: usize) -> String {
		Self::tag_text(default_profile(), memory, address)
	}

	pub(crate) fn tag_text(profile: &LayoutProfile, memory: &Memory, address: usize) -> String {
		let Some(slot) = address.checked_add(profile.country_tag.offset) else {
			return "?".to_string();
		};
		let Some(packed) = memory.read_qword(slot) else {
			return "?".to_string();
		};
		let bytes = packed.to_le_bytes();
		let len = bytes
			.iter()
			.position(|b| !b.is_ascii_uppercase() && !b.is_ascii_digit())
			.unwrap_or(4);
		String::from_utf8_lossy(&bytes[..len.min(4)]).into_owned()
	}
}

impl CountryView {
	/// The tag from the packed identity qword's low bytes.
	pub fn tag(&self) -> String {
		Self::tag_text(self.profile, &self.memory, self.address)
	}

	/// Checked tag read from the packed identity qword (`country::TAG`).
	pub fn try_tag(&self) -> Option<String> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(self.profile.country_tag.offset)?)?;
		decode_packed_tag(packed, false).map(|decoded| decoded.tag)
	}

	/// Whether the packed identity qword at `country::TAG` holds EU4's
	/// invalid/uncolonized country sentinel (`"---\0"`, typically slot 0).
	/// Returns `None` when the field cannot be read.
	pub fn is_sentinel_tag(&self) -> Option<bool> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(self.profile.country_tag.offset)?)?;
		Some(&packed.to_le_bytes()[..4] == b"---\0")
	}

	/// Country index from the packed identity qword (`country::TAG`).
	pub fn index(&self) -> Option<u16> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(self.profile.country_tag.offset)?)?;
		decode_packed_tag(packed, false).map(|decoded| decoded.index)
	}

	/// Monthly income bank (`i32`, thousandths) at `country::MONTHLY_INCOME_BANK`.
	pub fn monthly_income_bank(&self) -> Option<i32> {
		self.memory.read_i32(
			self.address
				.checked_add(self.profile.country_monthly_income_bank.offset)?,
		)
	}

	/// Tributary tribute-type discriminator (`u8`) at `country::TRIBUTE_TYPE`.
	pub fn tribute_type(&self) -> Option<u8> {
		self.memory.read_u8(
			self.address
				.checked_add(self.profile.country_tribute_type.offset)?,
		)
	}

	/// Decoded overlord packed country identity at `country::OVERLORD`, or
	/// `None` when independent (valid flag byte 7 is zero) or unreadable.
	pub fn overlord_tag(&self) -> Option<PackedCountryTag> {
		let packed = self.memory.read_qword(
			self.address
				.checked_add(self.profile.country_overlord.offset)?,
		)?;
		decode_packed_tag(packed, true)
	}

	/// Independence from the overlord qword: `Some(false)` when the valid
	/// flag marks a subject, `Some(true)` when the record is readable and
	/// no overlord is set, `None` when the field cannot be read (so
	/// independence stays unverified rather than assumed).
	///
	/// A non-zero flag byte is a subject for tributaries and custom subject
	/// types alike. Separating those through `CSubjectType` or `copy_from`
	/// is later work; callers keep the conservative refusal.
	pub fn independence(&self) -> Option<bool> {
		let packed = self.memory.read_qword(
			self.address
				.checked_add(self.profile.country_overlord.offset)?,
		)?;
		Some(packed >> 56 == 0)
	}

	/// Read candidate country-flag fields, using the actual host's executable
	/// digest. These fields have static evidence and are not game-validated.
	/// Successful empty flags differ from unsupported, unreadable or inconsistent
	/// snapshots. Names are bounded to the shared SSO reader's 64-byte limit.
	/// No setting, removal, command consumption or save-payload writing is exposed.
	///
	/// # Safety
	/// The caller supplies the real host's image base and computed digest and
	/// holds this registry-derived country, its flag vectors and the global name
	/// table live and stable on the game thread throughout the read. Two matching
	/// samples detect some races but cannot establish lifetime, type or atomicity.
	pub unsafe fn flags(
		&self,
		exe_base: usize,
		host_exe_sha256: &str,
	) -> Result<super::CountryFlags, super::FlagReadError> {
		super::flags::read(&self.memory, self.address, exe_base, host_exe_sha256)
	}

	/// Subject and truce restrictions recorded on this country.
	///
	/// # Safety
	/// As [`check_country_restrictions`](crate::dataplane::war::check_country_restrictions)
	/// for this live `CCountry` and its diplomacy array. The caller has
	/// reconciled `expected` and `enemies` with the same registry and holds
	/// those objects stable on the game thread. The overlord flag refuses every
	/// subject, including tributaries and custom types. These diplomacy fields
	/// are statically identified and have not passed a live Foch session probe.
	pub unsafe fn check_war_restrictions(
		&self,
		expected: &CountryRef,
		enemies: &[CountryRef],
	) -> Result<(), JoinRefusal> {
		// SAFETY: the caller upholds the free function's contract for this object.
		unsafe {
			super::war::check_country_restrictions_with(
				self.profile,
				&self.memory,
				self.address,
				expected,
				enemies,
			)
		}
	}

	/// Address of this `CCountry` allocation.
	///
	/// Candidate experiments anchor unverified fields here. The offset table
	/// stays inside the data plane.
	pub fn raw_address(&self) -> usize {
		self.address
	}

	/// The treasury (int32, thousandths; the game's own clamp is
	/// +/-1e9).
	pub fn treasury(&self) -> Option<i32> {
		self.memory.read_i32(
			self.address
				.checked_add(self.profile.country_treasury.offset)?,
		)
	}

	/// Treasury thousandths from an owned country record.
	///
	/// The field offset stays inside the data plane. `None` means
	/// `record` is shorter than the field.
	pub fn treasury_record(record: &[u8]) -> Option<i32> {
		let start = default_profile().country_treasury.offset;
		let end = start.checked_add(4)?;
		let bytes: [u8; 4] = record.get(start..end)?.try_into().ok()?;
		Some(i32::from_le_bytes(bytes))
	}

	/// Store treasury thousandths in an owned country record.
	///
	/// The field offset stays inside the data plane. `None` means
	/// `record` is shorter than the field.
	pub fn write_treasury_record(record: &mut [u8], value: i32) -> Option<()> {
		let start = default_profile().country_treasury.offset;
		let end = start.checked_add(4)?;
		let slot = record.get_mut(start..end)?;
		slot.copy_from_slice(&value.to_le_bytes());
		Some(())
	}

	/// Add to the treasury, mirroring the game's own ChangeTreasury:
	/// the clamp, and the player's treasury-label refresh through the
	/// UI controller's dirty bit.
	///
	/// # Safety
	///
	/// This writes two engine-owned targets: the treasury field at this
	/// view's address (reached through the country registry) and, when
	/// the caller's `context` resolves a UI controller, its flag word —
	/// so `context` must be the real game context, and the caller must
	/// guarantee both objects are still alive, that the call runs on the
	/// engine thread where the structures are stable, and that no Rust
	/// reference or validity invariant aliases either target.
	pub unsafe fn pay(&self, memory: &Memory, context: usize, amount: i64) -> Option<()> {
		let treasury_at = self
			.address
			.checked_add(self.profile.country_treasury.offset)?;
		let treasury = self.memory.read_i32(treasury_at)? as i64;
		let clamp = self.profile.country_treasury_clamp;
		let updated = treasury.saturating_add(amount).clamp(-clamp, clamp);
		// SAFETY: the target is the engine's own treasury field reached
		// through the validated country registry — no Rust object
		// aliases it — and the caller runs on the game thread.
		unsafe { memory.write_i32(treasury_at, updated as i32) }?;
		let Some(controller_at) = context.checked_add(self.profile.ui_controller.offset) else {
			return Some(());
		};
		if let Some(controller) = memory.read_ptr(controller_at)
			&& let Some(flags_at) = controller.checked_add(self.profile.ui_flags.offset)
			&& let Some(flags) = memory.read_i32(flags_at)
			&& let Ok(dirty) = i32::try_from(self.profile.ui_treasury_dirty.offset)
		{
			// SAFETY: the UI controller's flag word, same provenance and
			// thread contract as the treasury write above.
			unsafe { memory.write_i32(flags_at, flags | dirty) }?;
		}
		Some(())
	}
}

/// The country registry.
pub struct Countries {
	memory: Memory,
	pointers: usize,
	count: usize,
	profile: &'static LayoutProfile,
}

impl Countries {
	pub fn count(&self) -> usize {
		self.count
	}

	/// The `CCountry*` for an engine country index.
	pub fn country(&self, index: usize) -> Option<CountryView> {
		if index >= self.count {
			return None;
		}
		let address = self
			.memory
			.read_ptr(self.pointers + index * self.profile.pointer_size)?;
		Some(CountryView {
			memory: self.memory,
			address,
			profile: self.profile,
		})
	}
}

/// One province.
pub struct Province {
	memory: Memory,
	address: usize,
	profile: &'static LayoutProfile,
}

impl Province {
	/// Byte length of one province record in the pinned build.
	///
	/// Candidate offsets must stay inside this span. It is the record size,
	/// not a field offset.
	pub const fn record_span() -> usize {
		// Pinned 1.37.5 stride. Instance reads use the bound profile; the
		// unit test locks this const to that profile's `cprovince_size`.
		0x2e10
	}

	/// Address of this province record.
	///
	/// Candidate experiments anchor unverified fields here.
	pub fn raw_address(&self) -> usize {
		self.address
	}

	/// Verified base tax (thousandths) at an address from [`raw_address`](Self::raw_address).
	pub fn tax_at(memory: &Memory, address: usize) -> Option<i32> {
		memory.read_i32(address.checked_add(default_profile().province_tax.offset)?)
	}

	/// Verified base manpower (thousandths) at an address from [`raw_address`](Self::raw_address).
	pub fn manpower_at(memory: &Memory, address: usize) -> Option<i32> {
		memory.read_i32(address.checked_add(default_profile().province_manpower.offset)?)
	}

	/// Base tax (thousandths).
	pub fn tax(&self) -> Option<i32> {
		self.memory
			.read_i32(self.address.checked_add(self.profile.province_tax.offset)?)
	}

	/// Base manpower (thousandths).
	pub fn manpower(&self) -> Option<i32> {
		self.memory.read_i32(
			self.address
				.checked_add(self.profile.province_manpower.offset)?,
		)
	}
}

/// Maximum trade-node count accepted by [`Eu4Game::trade_nodes`].
///
/// Overhaul mods can replace the trade map with either fewer than 20 nodes
/// (small regional scenarios) or more than 512 nodes; only zero, negative, and
/// clearly corrupt counts are rejected.
pub const MAX_TRADE_NODES: usize = 4096;

/// Maximum member provinces accepted for a single trade node by
/// [`NodeView::member_provinces`].
///
/// Generous enough for total-conversion and single-node test maps while still
/// rejecting corrupt `std::vector` headers before allocating.
pub const MAX_NODE_MEMBERS: usize = 65_536;

fn valid_country_count(raw: i32) -> Option<usize> {
	let count = usize::try_from(raw).ok()?;
	(1..=i16::MAX as usize).contains(&count).then_some(count)
}

fn valid_trade_node_count(raw: i32) -> Option<usize> {
	let count = usize::try_from(raw).ok()?;
	(1..=MAX_TRADE_NODES).contains(&count).then_some(count)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn entity_count_gates_support_small_and_large_overhaul_mods() {
		assert_eq!(valid_country_count(0), None);
		assert_eq!(valid_country_count(-5), None);
		assert_eq!(valid_country_count(1), Some(1));
		assert_eq!(valid_country_count(1500), Some(1500));
		assert_eq!(
			valid_country_count(i16::MAX as i32),
			Some(i16::MAX as usize)
		);
		assert_eq!(valid_country_count(i16::MAX as i32 + 1), None);

		assert_eq!(valid_trade_node_count(0), None);
		assert_eq!(valid_trade_node_count(-1), None);
		assert_eq!(valid_trade_node_count(5), Some(5));
		assert_eq!(valid_trade_node_count(80), Some(80));
		assert_eq!(valid_trade_node_count(1024), Some(1024));
		assert_eq!(
			valid_trade_node_count(MAX_TRADE_NODES as i32),
			Some(MAX_TRADE_NODES)
		);
		assert_eq!(valid_trade_node_count(MAX_TRADE_NODES as i32 + 1), None);
	}

	fn pack_tag(tag: [u8; 3], index: i16, valid_flag: u8) -> u64 {
		let idx = index.to_le_bytes();
		u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, valid_flag])
	}

	#[test]
	fn decode_packed_tag_validates_format_index_and_valid_flag() {
		let valid = pack_tag(*b"MNG", 42, 1);
		let expected = PackedCountryTag {
			tag: "MNG".to_string(),
			index: 42,
		};
		assert_eq!(decode_packed_tag(valid, true), Some(expected.clone()));
		assert_eq!(decode_packed_tag(valid, false), Some(expected));

		// Valid flag unset: accepted when optional, rejected when required.
		let unset_flag = pack_tag(*b"D01", 0, 0);
		assert_eq!(decode_packed_tag(unset_flag, true), None);
		assert_eq!(
			decode_packed_tag(unset_flag, false),
			Some(PackedCountryTag {
				tag: "D01".to_string(),
				index: 0,
			})
		);

		// Non-zero NUL slot at byte 3 is rejected.
		let bad_nul = u64::from_le_bytes([b'F', b'R', b'A', b'X', 1, 0, 0, 1]);
		assert_eq!(decode_packed_tag(bad_nul, false), None);

		// Lowercase / non-alphanumeric tag bytes are rejected.
		assert_eq!(decode_packed_tag(pack_tag(*b"fra", 1, 1), false), None);
		assert_eq!(decode_packed_tag(pack_tag(*b"FR\0", 1, 1), false), None);

		// Negative signed index is rejected.
		assert_eq!(decode_packed_tag(pack_tag(*b"FRA", -1, 1), false), None);
	}

	#[test]
	#[cfg(windows)]
	fn country_view_from_ptr_and_readers() {
		let memory = Memory;
		assert!(CountryView::from_ptr(memory, 0).is_none());
		assert!(CountryView::from_ptr(memory, 0x1000).is_none());
		assert!(CountryView::from_ptr(memory, 0x1_0000).is_none());

		let mut raw = vec![0_u64; 0x2900 / 8];
		let base = raw.as_mut_ptr() as usize;
		assert!(CountryView::from_ptr(memory, base + 1).is_none());

		let profile = crate::dataplane::profile::default_profile();
		raw[profile.country_tag.offset / 8] = pack_tag(*b"KOR", 17, 0);
		raw[profile.country_monthly_income_bank.offset / 8] = 123_456_i32 as u32 as u64;
		raw[profile.country_overlord.offset / 8] = pack_tag(*b"MNG", 5, 0); // independent (flag == 0)
		let bytes =
			unsafe { core::slice::from_raw_parts_mut(raw.as_mut_ptr() as *mut u8, raw.len() * 8) };
		bytes[profile.country_tribute_type.offset] = 2;

		let view = CountryView::from_ptr(memory, base).expect("valid aligned country pointer");
		assert_eq!(view.raw_address(), base);
		assert_eq!(view.try_tag().as_deref(), Some("KOR"));
		assert_eq!(view.is_sentinel_tag(), Some(false));
		assert_eq!(view.index(), Some(17));
		assert_eq!(view.monthly_income_bank(), Some(123_456));
		assert_eq!(view.tribute_type(), Some(2));
		assert_eq!(view.overlord_tag(), None);
		assert_eq!(view.independence(), Some(true));

		// Mark overlord valid flag non-zero to simulate a subject.
		raw[profile.country_overlord.offset / 8] = pack_tag(*b"MNG", 5, 1);
		assert_eq!(
			view.overlord_tag(),
			Some(PackedCountryTag {
				tag: "MNG".to_string(),
				index: 5,
			})
		);
		assert_eq!(view.independence(), Some(false));

		// Sentinel `---` tag (slot 0 in live EU4) is recognized explicitly.
		raw[profile.country_tag.offset / 8] = pack_tag(*b"---", 0, 0);
		assert_eq!(view.try_tag(), None);
		assert_eq!(view.is_sentinel_tag(), Some(true));
	}

	#[test]
	#[cfg(windows)]
	fn node_view_reads_and_validates_member_provinces() {
		let memory = Memory;
		let mut def_buf = vec![0_u64; 0xc0 / 8];
		let def_addr = def_buf.as_mut_ptr() as usize;
		let members = crate::dataplane::profile::default_profile()
			.def_members
			.offset;
		let node = NodeView {
			memory,
			address: def_addr,
			definition: def_addr,
			profile: crate::dataplane::profile::default_profile(),
		};

		// Empty null vector is legal (e.g. placeholder/custom trade node).
		assert_eq!(node.member_provinces(), Some(Vec::new()));

		// Populated vector with extra reserved capacity.
		let provinces: [i32; 4] = [1, 45, 4941, 0];
		let prov_base = provinces.as_ptr() as usize;
		def_buf[members / 8] = prov_base as u64;
		def_buf[members / 8 + 1] = (prov_base + 3 * 4) as u64;
		def_buf[members / 8 + 2] = (prov_base + 4 * 4) as u64;
		assert_eq!(node.member_provinces(), Some(vec![1, 45, 4941]));

		// Empty vector with reserved capacity.
		def_buf[members / 8 + 1] = prov_base as u64;
		assert_eq!(node.member_provinces(), Some(Vec::new()));

		// Non-positive province IDs (0 or negative) fail validation.
		let with_zero: [i32; 2] = [1, 0];
		let z_base = with_zero.as_ptr() as usize;
		def_buf[members / 8] = z_base as u64;
		def_buf[members / 8 + 1] = (z_base + 8) as u64;
		def_buf[members / 8 + 2] = (z_base + 8) as u64;
		assert_eq!(node.member_provinces(), None);

		let with_neg: [i32; 2] = [1, -7];
		let n_base = with_neg.as_ptr() as usize;
		def_buf[members / 8] = n_base as u64;
		def_buf[members / 8 + 1] = (n_base + 8) as u64;
		def_buf[members / 8 + 2] = (n_base + 8) as u64;
		assert_eq!(node.member_provinces(), None);

		// Corrupt vector bounds (end < begin) or oversize count are rejected.
		def_buf[members / 8] = (prov_base + 8) as u64;
		def_buf[members / 8 + 1] = prov_base as u64;
		def_buf[members / 8 + 2] = (prov_base + 16) as u64;
		assert_eq!(node.member_provinces(), None);

		let oversize_bytes = (MAX_NODE_MEMBERS + 1) * size_of::<i32>();
		def_buf[members / 8] = prov_base as u64;
		def_buf[members / 8 + 1] = (prov_base + oversize_bytes) as u64;
		def_buf[members / 8 + 2] = (prov_base + oversize_bytes) as u64;
		assert_eq!(node.member_provinces(), None);
		assert_eq!(node.raw_address(), def_addr);
	}

	#[test]
	fn province_record_span_is_the_pinned_stride() {
		let profile = crate::dataplane::profile::default_profile();
		assert_eq!(
			Province::record_span(),
			profile.province_cprovince_size.offset
		);
		assert!(Province::record_span() > profile.province_tax.offset);
		assert!(Province::record_span() > profile.province_manpower.offset);
	}

	#[test]
	fn from_tick_rejects_an_unknown_image_base() {
		assert!(Eu4Game::from_tick(0, 0x100_000).is_none());
		assert!(Eu4Game::from_tick(0x1_0000, 0x100_000).is_none());
		assert!(Eu4Game::from_exe_base(0).is_none());
		assert!(Eu4Game::from_exe_base(0x1_0000).is_none());
	}

	#[test]
	#[cfg(windows)]
	fn game_views_cover_treasury_routes_and_participants() {
		let profile = crate::dataplane::profile::default_profile();

		fn write_i32(buffer: &mut [u64], offset: usize, value: i32) {
			let index = offset / 8;
			let shift = (offset % 8) * 8;
			let mask = !(u64::from(u32::MAX) << shift);
			buffer[index] = (buffer[index] & mask) | (u64::from(value as u32) << shift);
		}

		let mut image = vec![0u64; profile.context_global.offset / 8 + 1];
		let mut context = vec![0u64; 0x2200 / 8];
		let mut manager = [0u64; 0x120 / 8];
		let mut pointers = [0u64; 3];
		let mut country_buf = [0u64; 0xc00 / 8];
		let controller = [0u64; 0x390 / 8];
		let mut node_ptrs = [0u64; 2];
		let mut node0 = [0u64; 0xc0 / 8];
		let mut node1 = [0u64; 0xc0 / 8];
		let mut def0 = [0u64; 0xc0 / 8];
		let mut def1 = [0u64; 0xc0 / 8];
		let mut route_bytes = vec![0u8; profile.route_size.offset];
		let mut entries = vec![0u8; profile.node_entry_size.offset * 3];
		let mut provinces = vec![0u8; profile.province_cprovince_size.offset * 2];
		let empty = [0u8; 8];

		country_buf[profile.country_tag.offset / 8] = u64::from_le_bytes(*b"FRA\0\0\0\0\x01");
		write_i32(&mut country_buf, profile.country_treasury.offset, 1_000);
		write_i32(&mut context, profile.countries_count.offset, 3);
		pointers[0] = country_buf.as_ptr() as u64;
		manager[profile.country_manager_index_array.offset / 8] = pointers.as_ptr() as u64;
		image[profile.country_manager_global.offset / 8] = manager.as_ptr() as u64;
		image[profile.context_global.offset / 8] = context.as_ptr() as u64;
		context[profile.ui_controller.offset / 8] = controller.as_ptr() as u64;

		let db_at = profile.context_db.offset;
		context[(db_at + profile.db_nodes.offset) / 8] = node_ptrs.as_ptr() as u64;
		write_i32(&mut context, db_at + profile.db_node_count.offset, 2);
		node_ptrs[0] = node0.as_ptr() as u64;
		node_ptrs[1] = node1.as_ptr() as u64;
		node0[profile.node_definition.offset / 8] = def0.as_ptr() as u64;
		node1[profile.node_definition.offset / 8] = def1.as_ptr() as u64;
		node0[profile.node_entry_table.offset / 8] = entries.as_ptr() as u64;
		for (index, power) in [1_000i32, 0, 250].into_iter().enumerate() {
			let at = index * profile.node_entry_size.offset + profile.entry_power.offset;
			entries[at..at + 4].copy_from_slice(&power.to_le_bytes());
		}

		let route_ptr = route_bytes.as_ptr() as u64;
		def0[profile.def_routes.offset / 8] = route_ptr;
		def0[profile.def_routes_end.offset / 8] = route_ptr + profile.route_size.offset as u64;
		let target = def1.as_ptr() as u64;
		let target_at = profile.route_target.offset;
		route_bytes[target_at..target_at + 8].copy_from_slice(&target.to_le_bytes());
		let empty_ptr = empty.as_ptr() as u64;
		def1[profile.def_routes.offset / 8] = empty_ptr;
		def1[profile.def_routes_end.offset / 8] = empty_ptr;

		let prov_base = provinces.as_ptr() as usize;
		context[profile.province_cprovinces.offset / 8] = prov_base as u64;
		let tax_at = profile.province_cprovince_size.offset + profile.province_tax.offset;
		provinces[tax_at..tax_at + 4].copy_from_slice(&12_000i32.to_le_bytes());
		let manpower_at = profile.province_cprovince_size.offset + profile.province_manpower.offset;
		provinces[manpower_at..manpower_at + 4].copy_from_slice(&6_000i32.to_le_bytes());
		// The readers below go through kernel copies of these addresses, which
		// the unused-assignment lint does not treat as a read.
		std::hint::black_box((
			&image,
			&context,
			&manager,
			&pointers,
			&country_buf,
			&controller,
			&node_ptrs,
			&node0,
			&node1,
			&def0,
			&def1,
			&route_bytes,
			&entries,
			&provinces,
			&empty,
		));

		let exe_base = image.as_ptr() as usize;
		let context_addr = context.as_ptr() as usize;
		assert!(Eu4Game::from_tick(exe_base, exe_base).is_none());
		let game = Eu4Game::from_tick(exe_base, context_addr).expect("context global");
		let from_base = Eu4Game::from_exe_base(exe_base).expect("global context slot");
		assert_eq!(from_base.context, context_addr);
		assert_eq!(from_base.exe_base, exe_base);
		// SAFETY: the synthetic objects above stay allocated for this call.
		unsafe { game.pay_country(0, 50) }.expect("pay");
		let paid = game.countries().unwrap().country(0).unwrap();
		assert_eq!(paid.raw_address(), country_buf.as_ptr() as usize);
		assert_eq!(paid.treasury(), Some(1_050));
		assert_eq!(
			Memory.read_i32(controller.as_ptr() as usize + profile.ui_flags.offset),
			Some(profile.ui_treasury_dirty.offset as i32)
		);
		assert!(unsafe { game.pay_country(3, 1) }.is_none());
		write_i32(
			&mut country_buf,
			profile.country_treasury.offset,
			1_000_000_000 - 10,
		);
		std::hint::black_box(&country_buf);
		unsafe { game.pay_country(0, 100) }.unwrap();
		assert_eq!(
			game.countries().unwrap().country(0).unwrap().treasury(),
			Some(1_000_000_000)
		);

		let province_row = game.province(1).unwrap();
		assert_eq!(
			province_row.raw_address(),
			prov_base + profile.province_cprovince_size.offset
		);
		assert_eq!(province_row.tax(), Some(12_000));
		assert_eq!(province_row.manpower(), Some(6_000));
		assert_eq!(
			Province::tax_at(&Memory, province_row.raw_address()),
			Some(12_000)
		);
		assert_eq!(
			Province::manpower_at(&Memory, province_row.raw_address()),
			Some(6_000)
		);

		let nodes = game.trade_nodes().unwrap();
		assert_eq!(nodes.count(), 2);
		let first = nodes.node(0).unwrap();
		let second = nodes.node(1).unwrap();
		assert_eq!(first.raw_address(), node0.as_ptr() as usize);
		assert_eq!(first.live_route_count(&nodes), Some(1));
		assert_eq!(second.live_route_count(&nodes), Some(0));
		assert_eq!(first.outgoing_targets(&nodes), Some(vec![1]));
		assert_eq!(second.outgoing_targets(&nodes), Some(vec![]));
		let countries = game.countries().unwrap();
		assert_eq!(
			first.participants(&countries),
			vec![(0, 1_000), (1, 0), (2, 250)]
		);
		let mutation = first.capture_routes(&nodes).expect("capture");
		assert!(!mutation.is_closed(0));
		assert!(!mutation.is_closed(3));

		route_bytes[target_at..target_at + 8].copy_from_slice(&1u64.to_le_bytes());
		std::hint::black_box(&route_bytes);
		assert_eq!(first.outgoing_targets(&nodes), None);

		// A default-constructed MSVC route vector is null/null, which live
		// end nodes use. It is an empty list, not a corrupt header.
		def1[profile.def_routes.offset / 8] = 0;
		def1[profile.def_routes_end.offset / 8] = 0;
		std::hint::black_box(&def1);
		assert_eq!(second.live_route_count(&nodes), Some(0));
		assert_eq!(second.outgoing_targets(&nodes), Some(vec![]));

		// Non-zero null-page pointers are rejected even when begin == end.
		def1[profile.def_routes.offset / 8] = 0x1000;
		def1[profile.def_routes_end.offset / 8] = 0x1000;
		std::hint::black_box(&def1);
		assert_eq!(second.live_route_count(&nodes), None);
	}

	#[test]
	fn treasury_record_round_trips_at_the_pinned_field() {
		let treasury = crate::dataplane::profile::default_profile()
			.country_treasury
			.offset;
		let mut record = vec![0u8; treasury + 4];
		CountryView::write_treasury_record(&mut record, 350).unwrap();
		assert_eq!(CountryView::treasury_record(&record), Some(350));
		let stored = i32::from_le_bytes(record[treasury..treasury + 4].try_into().unwrap());
		assert_eq!(stored, 350);
		assert!(CountryView::write_treasury_record(&mut record[..4], 1).is_none());
		assert!(CountryView::treasury_record(&record[..4]).is_none());
	}
}
