//! Typed views over the live EU4 structures.
//!
//! Thin by design: each view reads the layout's fields and nothing
//! more; semantics (what a retention rate means, which aggregate makes
//! a sink) belong to the plugins. All values keep the engine's units —
//! thousandths — and every fallible read is an `Option`.

use super::layout::{country, country_manager, db, def, node, province, route};
use super::{Memory, is_name_byte, read_sso};

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
			.read_i32(self.context + super::layout::COUNTRIES_COUNT)? as usize;
		(1..=1024).contains(&count).then_some(Countries {
			memory: self.memory,
			pointers,
			count,
		})
	}

	/// The trade node database embedded in the context.
	pub fn trade_nodes(&self) -> Option<TradeNodes> {
		let db = self.context + super::layout::DB_IN_CONTEXT;
		let nodes = self.memory.read_ptr(db + db::NODES)?;
		let count = self.memory.read_i32(db + db::NODE_COUNT)? as usize;
		(20..=512).contains(&count).then_some(TradeNodes {
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

/// One country.
pub struct CountryView {
	memory: Memory,
	address: usize,
}

impl CountryView {
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
