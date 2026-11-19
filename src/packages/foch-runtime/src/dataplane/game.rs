//! Typed views over the live EU4 structures.
//!
//! Thin by design: each view reads the layout's fields and nothing
//! more; semantics (what a retention rate means, which aggregate makes
//! a sink) belong to the plugins. All values keep the engine's units —
//! thousandths — and every fallible read is an `Option`.

use super::layout::{country, country_manager, db, def, node, province, route};
use super::{Memory, StdVector, is_name_byte, read_sso};

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
		if exe_base <= 0x1_0000 {
			return None;
		}
		let stored = memory.read_ptr(exe_base + super::layout::CONTEXT_GLOBAL_RVA)?;
		(stored == candidate).then_some(stored)
	}

	/// The country registry: index-to-`CCountry*` array and count.
	pub fn countries(&self) -> Option<Countries> {
		let manager = self
			.memory
			.read_ptr(self.exe_base + super::layout::COUNTRY_MANAGER_GLOBAL_RVA)?;
		let pointers = self
			.memory
			.read_ptr(manager + country_manager::INDEX_ARRAY)?;
		let count = self
			.memory
			.read_i32(self.context + super::layout::COUNTRIES_COUNT)
			.and_then(valid_country_count)?;
		Some(Countries {
			memory: self.memory,
			pointers,
			count,
		})
	}

	/// The trade node database embedded in the context.
	pub fn trade_nodes(&self) -> Option<TradeNodes> {
		let db = self.context + super::layout::DB_IN_CONTEXT;
		let nodes = self.memory.read_ptr(db + db::NODES)?;
		let count = self
			.memory
			.read_i32(db + db::NODE_COUNT)
			.and_then(valid_trade_node_count)?;
		Some(TradeNodes {
			memory: self.memory,
			db,
			nodes,
			count,
		})
	}

	/// The province array, indexed by province id.
	pub fn province(&self, id: u32) -> Option<Province> {
		let base = self.memory.read_ptr(self.context + province::CPROVINCES)?;
		let address = base.checked_add(id as usize * province::CPROVINCE_SIZE)?;
		Some(Province {
			memory: self.memory,
			address,
		})
	}
}

/// The trade node database.
pub struct TradeNodes {
	memory: Memory,
	#[allow(dead_code)]
	db: usize,
	nodes: usize,
	count: usize,
}

impl TradeNodes {
	pub fn count(&self) -> usize {
		self.count
	}

	/// The heap `CTradeNode*` at the given engine index.
	pub fn node(&self, index: usize) -> Option<NodeView> {
		let address = self.memory.read_ptr(self.nodes + index * 8)?;
		let definition = self.memory.read_ptr(address + node::DEFINITION)?;
		Some(NodeView {
			memory: self.memory,
			address,
			definition,
		})
	}

	/// The route vector of a node definition: `(begin, end)`. An empty
	/// vector (`begin == end`, both plausibly null) is legal — vanilla
	/// end nodes have no outgoing routes at all.
	pub fn routes(&self, definition: usize) -> Option<(usize, usize)> {
		let begin = self.memory.read_ptr(definition + def::ROUTES)?;
		let end = self.memory.read_qword(definition + def::ROUTES_END)? as usize;
		if begin == end {
			return Some((begin, end));
		}
		(end >= begin && (end - begin).is_multiple_of(route::SIZE)).then_some((begin, end))
	}
}

/// One live trade node plus its definition.
pub struct NodeView {
	memory: Memory,
	pub address: usize,
	pub definition: usize,
}

impl NodeView {
	fn i32(&self, offset: usize) -> Option<i32> {
		self.memory.read_i32(self.address + offset)
	}

	/// Current total trade value (thousandths).
	pub fn value(&self) -> Option<i32> {
		self.i32(node::VALUE)
	}

	/// Local trade value: the production-side injection (goods x
	/// price), thousandths.
	pub fn local_value(&self) -> Option<i32> {
		self.i32(node::LOCAL_VALUE)
	}

	/// The amount transferred out this tick (thousandths); the engine's
	/// own retention rate is `value / (value + transferred)`.
	pub fn transferred(&self) -> Option<i32> {
		self.i32(node::TRANSFERRED)
	}

	/// Total trade power (thousandths).
	pub fn power(&self) -> Option<i32> {
		self.i32(node::POWER)
	}

	/// The per-outgoing-edge steering weights, in the live slot order.
	pub fn steering(&self) -> Vec<f64> {
		match self.memory.read_ptr(self.address + node::WEIGHTS) {
			Some(begin) => match self.memory.read_qword(self.address + node::WEIGHTS + 8) {
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
		read_sso(&self.memory, self.definition + def::NAME, is_name_byte)
	}

	/// Member province IDs (`members = { ... }`) read from
	/// `CTradeNodeDefinition + def::MEMBERS` (`std::vector<i32>`).
	///
	/// Statically identified for EU4 1.37.5; pending live Foch session
	/// verification. Returns `Some(Vec::new())` for an empty vector, and
	/// `None` if the vector header, buffer range, or any province ID
	/// (`<= 0`) fails validation.
	pub fn member_provinces(&self) -> Option<Vec<u32>> {
		let header = StdVector::read(&self.memory, self.definition.checked_add(def::MEMBERS)?)?;
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
		self.memory.read_ptr(slot + route::TARGET)
	}

	/// The per-country entry table: `entry = *(node+ENTRY_TABLE) +
	/// country_index * ENTRY_SIZE`.
	pub fn country_entry(&self, country_index: usize) -> Option<usize> {
		let table = self.memory.read_ptr(self.address + node::ENTRY_TABLE)?;
		table.checked_add(country_index * node::ENTRY_SIZE)
	}

	/// A country's trade power contribution inside this node
	/// (thousandths).
	pub fn entry_power(&self, country_index: usize) -> Option<i32> {
		let entry = self.country_entry(country_index)?;
		self.memory.read_i32(entry + super::layout::entry::POWER)
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
}

impl CountryView {
	/// Constructs a country view from a raw `CCountry*` address after checking
	/// basic pointer plausibility (above the null page and 8-byte aligned).
	pub fn from_ptr(memory: Memory, address: usize) -> Option<CountryView> {
		(address > 0x1_0000 && address.is_multiple_of(8)).then_some(CountryView { memory, address })
	}

	/// Read just the tag of the country at `address`, without the full
	/// view (the registry holds the addresses already).
	pub fn tag_at(memory: &Memory, address: usize) -> String {
		let Some(packed) = memory.read_qword(address + country::TAG) else {
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
		Self::tag_at(&self.memory, self.address)
	}

	/// Checked tag read from the packed identity qword (`country::TAG`).
	pub fn try_tag(&self) -> Option<String> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(country::TAG)?)?;
		decode_packed_tag(packed, false).map(|decoded| decoded.tag)
	}

	/// Whether the packed identity qword at `country::TAG` holds EU4's
	/// invalid/uncolonized country sentinel (`"---\0"`, typically slot 0).
	/// Returns `None` when the field cannot be read.
	pub fn is_sentinel_tag(&self) -> Option<bool> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(country::TAG)?)?;
		Some(&packed.to_le_bytes()[..4] == b"---\0")
	}

	/// Country index from the packed identity qword (`country::TAG`).
	pub fn index(&self) -> Option<u16> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(country::TAG)?)?;
		decode_packed_tag(packed, false).map(|decoded| decoded.index)
	}

	/// Monthly income bank (`i32`, thousandths) at `country::MONTHLY_INCOME_BANK`.
	pub fn monthly_income_bank(&self) -> Option<i32> {
		self.memory
			.read_i32(self.address.checked_add(country::MONTHLY_INCOME_BANK)?)
	}

	/// Tributary tribute-type discriminator (`u8`) at `country::TRIBUTE_TYPE`.
	pub fn tribute_type(&self) -> Option<u8> {
		self.memory
			.read_u8(self.address.checked_add(country::TRIBUTE_TYPE)?)
	}

	/// Decoded overlord packed country identity at `country::OVERLORD`, or
	/// `None` when independent (valid flag byte 7 is zero) or unreadable.
	pub fn overlord_tag(&self) -> Option<PackedCountryTag> {
		let packed = self
			.memory
			.read_qword(self.address.checked_add(country::OVERLORD)?)?;
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
		let packed = self
			.memory
			.read_qword(self.address.checked_add(country::OVERLORD)?)?;
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

	/// The treasury (int32, thousandths; the game's own clamp is
	/// +/-1e9).
	pub fn treasury(&self) -> Option<i32> {
		self.memory.read_i32(self.address + country::TREASURY)
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
		let treasury = self.treasury()? as i64;
		let updated = treasury
			.saturating_add(amount)
			.clamp(-country::TREASURY_CLAMP, country::TREASURY_CLAMP);
		// SAFETY: the target is the engine's own treasury field reached
		// through the validated country registry — no Rust object
		// aliases it — and the caller runs on the game thread.
		unsafe { memory.write_i32(self.address + country::TREASURY, updated as i32) }?;
		if let Some(controller) = memory.read_ptr(context + super::layout::ui::CONTROLLER)
			&& let Some(flags) = memory.read_i32(controller + super::layout::ui::FLAGS)
		{
			// SAFETY: the UI controller's flag word, same provenance and
			// thread contract as the treasury write above.
			unsafe {
				memory.write_i32(
					controller + super::layout::ui::FLAGS,
					flags | super::layout::ui::TREASURY_DIRTY,
				)
			}?;
		}
		Some(())
	}
}

/// The country registry.
pub struct Countries {
	memory: Memory,
	pointers: usize,
	count: usize,
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
		let address = self.memory.read_ptr(self.pointers + index * 8)?;
		Some(CountryView {
			memory: self.memory,
			address,
		})
	}
}

/// One province.
pub struct Province {
	memory: Memory,
	address: usize,
}

impl Province {
	/// Base tax (thousandths).
	pub fn tax(&self) -> Option<i32> {
		self.memory.read_i32(self.address + province::TAX)
	}

	/// Base manpower (thousandths).
	pub fn manpower(&self) -> Option<i32> {
		self.memory.read_i32(self.address + province::MANPOWER)
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

		raw[country::TAG / 8] = pack_tag(*b"KOR", 17, 0);
		raw[country::MONTHLY_INCOME_BANK / 8] = 123_456_i32 as u32 as u64;
		raw[country::OVERLORD / 8] = pack_tag(*b"MNG", 5, 0); // independent (flag == 0)
		let bytes =
			unsafe { core::slice::from_raw_parts_mut(raw.as_mut_ptr() as *mut u8, raw.len() * 8) };
		bytes[country::TRIBUTE_TYPE] = 2;

		let view = CountryView::from_ptr(memory, base).expect("valid aligned country pointer");
		assert_eq!(view.try_tag().as_deref(), Some("KOR"));
		assert_eq!(view.is_sentinel_tag(), Some(false));
		assert_eq!(view.index(), Some(17));
		assert_eq!(view.monthly_income_bank(), Some(123_456));
		assert_eq!(view.tribute_type(), Some(2));
		assert_eq!(view.overlord_tag(), None);
		assert_eq!(view.independence(), Some(true));

		// Mark overlord valid flag non-zero to simulate a subject.
		raw[country::OVERLORD / 8] = pack_tag(*b"MNG", 5, 1);
		assert_eq!(
			view.overlord_tag(),
			Some(PackedCountryTag {
				tag: "MNG".to_string(),
				index: 5,
			})
		);
		assert_eq!(view.independence(), Some(false));

		// Sentinel `---` tag (slot 0 in live EU4) is recognized explicitly.
		raw[country::TAG / 8] = pack_tag(*b"---", 0, 0);
		assert_eq!(view.try_tag(), None);
		assert_eq!(view.is_sentinel_tag(), Some(true));
	}

	#[test]
	#[cfg(windows)]
	fn node_view_reads_and_validates_member_provinces() {
		let memory = Memory;
		let mut def_buf = vec![0_u64; 0xc0 / 8];
		let def_addr = def_buf.as_mut_ptr() as usize;
		let node = NodeView {
			memory,
			address: def_addr,
			definition: def_addr,
		};

		// Empty null vector is legal (e.g. placeholder/custom trade node).
		assert_eq!(node.member_provinces(), Some(Vec::new()));

		// Populated vector with extra reserved capacity.
		let provinces: [i32; 4] = [1, 45, 4941, 0];
		let prov_base = provinces.as_ptr() as usize;
		def_buf[def::MEMBERS / 8] = prov_base as u64;
		def_buf[def::MEMBERS / 8 + 1] = (prov_base + 3 * 4) as u64;
		def_buf[def::MEMBERS / 8 + 2] = (prov_base + 4 * 4) as u64;
		assert_eq!(node.member_provinces(), Some(vec![1, 45, 4941]));

		// Empty vector with reserved capacity.
		def_buf[def::MEMBERS / 8 + 1] = prov_base as u64;
		assert_eq!(node.member_provinces(), Some(Vec::new()));

		// Non-positive province IDs (0 or negative) fail validation.
		let with_zero: [i32; 2] = [1, 0];
		let z_base = with_zero.as_ptr() as usize;
		def_buf[def::MEMBERS / 8] = z_base as u64;
		def_buf[def::MEMBERS / 8 + 1] = (z_base + 8) as u64;
		def_buf[def::MEMBERS / 8 + 2] = (z_base + 8) as u64;
		assert_eq!(node.member_provinces(), None);

		let with_neg: [i32; 2] = [1, -7];
		let n_base = with_neg.as_ptr() as usize;
		def_buf[def::MEMBERS / 8] = n_base as u64;
		def_buf[def::MEMBERS / 8 + 1] = (n_base + 8) as u64;
		def_buf[def::MEMBERS / 8 + 2] = (n_base + 8) as u64;
		assert_eq!(node.member_provinces(), None);

		// Corrupt vector bounds (end < begin) or oversize count are rejected.
		def_buf[def::MEMBERS / 8] = (prov_base + 8) as u64;
		def_buf[def::MEMBERS / 8 + 1] = prov_base as u64;
		def_buf[def::MEMBERS / 8 + 2] = (prov_base + 16) as u64;
		assert_eq!(node.member_provinces(), None);

		let oversize_bytes = (MAX_NODE_MEMBERS + 1) * size_of::<i32>();
		def_buf[def::MEMBERS / 8] = prov_base as u64;
		def_buf[def::MEMBERS / 8 + 1] = (prov_base + oversize_bytes) as u64;
		def_buf[def::MEMBERS / 8 + 2] = (prov_base + oversize_bytes) as u64;
		assert_eq!(node.member_provinces(), None);
	}
}
