//! Synthetic EU4 memory for Windows tests.
//!
//! Enabled only with the `test-support` feature, and only on Windows, so a
//! production plugin DLL that depends on `foch-runtime` without that feature
//! does not link this module. The builder does not publish image offsets.
//! [`FakeGame`] owns every allocation; views taken from it are raw addresses
//! and must not be used after the scene is dropped.

use super::profile::{LayoutProfile, default_profile};
use super::{CountryView, Eu4Game, is_name_byte};

fn layout() -> &'static LayoutProfile {
	default_profile()
}

const COUNTRY_BYTES: usize = 0x1900;

/// One owned synthetic campaign.
pub struct FakeGame {
	specs: Specs,
	live: Option<Live>,
	error: Option<&'static str>,
}

struct Specs {
	countries: Vec<CountrySpec>,
	provinces: Vec<ProvinceSpec>,
	nodes: Vec<NodeSpec>,
}

struct CountrySpec {
	tag: [u8; 3],
	index: u16,
	treasury: i32,
	overlord: Option<([u8; 3], u16)>,
	flags: Vec<(String, i32)>,
}

struct ProvinceSpec {
	id: u32,
	tax: i32,
	manpower: i32,
	patches: Vec<(usize, Vec<u8>)>,
}

struct NodeSpec {
	routes: Vec<usize>,
	members: Vec<u32>,
}

struct Live {
	image: Vec<u64>,
	context: Vec<u64>,
	manager: Vec<u64>,
	pointers: Vec<u64>,
	countries: Vec<Vec<u64>>,
	flag_ids: Vec<Vec<u16>>,
	flag_dates: Vec<Vec<i32>>,
	flag_names: Vec<[u64; 4]>,
	flag_payloads: Vec<Vec<u8>>,
	provinces: Vec<u64>,
	node_ptrs: Vec<u64>,
	nodes: Vec<Vec<u64>>,
	defs: Vec<Vec<u64>>,
	routes: Vec<Vec<u64>>,
	members: Vec<Vec<i32>>,
	/// Shared anchor for an empty route vector (`begin == end`).
	empty_route: Box<u64>,
}

impl Default for FakeGame {
	fn default() -> Self {
		Self::new()
	}
}

impl FakeGame {
	pub fn new() -> Self {
		Self {
			specs: Specs {
				countries: Vec::new(),
				provinces: Vec::new(),
				nodes: Vec::new(),
			},
			live: None,
			error: None,
		}
	}

	/// Add a registry country.
	///
	/// `index` is the engine slot. `overlord` is `(tag, index)` when the
	/// country is a subject; `None` leaves it independent. `flags` are
	/// `(name, set_at_hours)` in engine order. Names are 1..=64 display bytes.
	pub fn add_country(
		&mut self,
		tag: &str,
		index: u16,
		treasury: i32,
		overlord: Option<(&str, u16)>,
		flags: &[(&str, i32)],
	) -> Result<(), &'static str> {
		self.ensure_open()?;
		if index >= i16::MAX as u16 {
			return Err("country index does not fit the registry");
		}
		if self
			.specs
			.countries
			.iter()
			.any(|country| country.index == index)
		{
			return Err("country index is already present");
		}
		let tag = tag_bytes(tag)?;
		let overlord = match overlord {
			None => None,
			Some((tag, index)) => {
				if index > i16::MAX as u16 {
					return Err("overlord index does not fit a packed identity");
				}
				Some((tag_bytes(tag)?, index))
			}
		};
		let mut stored = Vec::with_capacity(flags.len());
		for (name, hours) in flags {
			if stored.iter().any(|(have, _)| have == name) {
				return Err("country flag name is duplicated");
			}
			valid_flag_name(name)?;
			stored.push(((*name).to_string(), *hours));
		}
		self.specs.countries.push(CountrySpec {
			tag,
			index,
			treasury,
			overlord,
			flags: stored,
		});
		Ok(())
	}

	/// Add a province record. `tax` and `manpower` are thousandths.
	pub fn add_province(&mut self, id: u32, tax: i32, manpower: i32) -> Result<(), &'static str> {
		self.ensure_open()?;
		if self
			.specs
			.provinces
			.iter()
			.any(|province| province.id == id)
		{
			return Err("province id is already present");
		}
		let slots = usize::try_from(id)
			.ok()
			.and_then(|id| id.checked_add(1))
			.ok_or("province id overflows")?;
		if slots
			.checked_mul(layout().province_cprovince_size.offset)
			.is_none()
		{
			return Err("province map overflows");
		}
		self.specs.provinces.push(ProvinceSpec {
			id,
			tax,
			manpower,
			patches: Vec::new(),
		});
		Ok(())
	}

	/// Overlay candidate bytes on a province already added.
	///
	/// `offset` is relative to that province record. The pinned field table
	/// stays inside the data plane; a patch that would leave the record is
	/// refused. Patches are applied after tax and manpower, so they can cover
	/// those fields when a test asks them to.
	pub fn write_province(
		&mut self,
		id: u32,
		offset: usize,
		bytes: &[u8],
	) -> Result<(), &'static str> {
		self.ensure_open()?;
		let province = self
			.specs
			.provinces
			.iter_mut()
			.find(|province| province.id == id)
			.ok_or("province was not added")?;
		let end = offset
			.checked_add(bytes.len())
			.ok_or("province patch overflows")?;
		if end > layout().province_cprovince_size.offset {
			return Err("province patch leaves the record");
		}
		province.patches.push((offset, bytes.to_vec()));
		Ok(())
	}

	/// Append a trade node.
	///
	/// `routes` are indexes into the nodes added so far, including this call's
	/// predecessors and forward indexes that exist by the time [`game`](Self::game)
	/// is called. `members` are positive province ids.
	pub fn add_trade_node(
		&mut self,
		routes: &[usize],
		members: &[u32],
	) -> Result<(), &'static str> {
		self.ensure_open()?;
		if self.specs.nodes.len() >= super::game::MAX_TRADE_NODES {
			return Err("too many trade nodes");
		}
		if members.iter().any(|id| *id == 0 || *id > i32::MAX as u32) {
			return Err("trade node member province id must be positive");
		}
		self.specs.nodes.push(NodeSpec {
			routes: routes.to_vec(),
			members: members.to_vec(),
		});
		Ok(())
	}

	/// The image base of the sealed scene.
	///
	/// Pass it to [`Eu4Game::from_exe_base`] the way a hook that is not given
	/// a context argument would.
	pub fn exe_base(&mut self) -> Result<usize, &'static str> {
		self.materialize()?;
		Ok(self.live.as_ref().unwrap().image.as_ptr() as usize)
	}

	/// Seal the scene and return the live game view.
	pub fn game(&mut self) -> Result<Eu4Game, &'static str> {
		let base = self.exe_base()?;
		Eu4Game::from_exe_base(base).ok_or("global context did not resolve")
	}

	/// Registry view for `index`, after the scene is sealed.
	pub fn country(&mut self, index: usize) -> Result<CountryView, &'static str> {
		let game = self.game()?;
		game.countries()
			.and_then(|countries| countries.country(index))
			.ok_or("country is absent")
	}

	fn ensure_open(&self) -> Result<(), &'static str> {
		if let Some(error) = self.error {
			return Err(error);
		}
		if self.live.is_some() {
			return Err("scene is already live");
		}
		Ok(())
	}

	fn materialize(&mut self) -> Result<(), &'static str> {
		if let Some(error) = self.error {
			return Err(error);
		}
		if self.live.is_some() {
			return Ok(());
		}
		match assemble(&self.specs) {
			Ok(live) => {
				self.live = Some(live);
				Ok(())
			}
			Err(error) => {
				self.error = Some(error);
				Err(error)
			}
		}
	}
}

fn assemble(specs: &Specs) -> Result<Live, &'static str> {
	for node in &specs.nodes {
		if node
			.routes
			.iter()
			.any(|target| *target >= specs.nodes.len())
		{
			return Err("trade route target is not a node");
		}
	}
	let slots = specs
		.countries
		.iter()
		.map(|country| usize::from(country.index) + 1)
		.max()
		.unwrap_or(0);
	if slots > i16::MAX as usize {
		return Err("country registry does not fit");
	}
	let mut live = Live {
		image: vec![0; (layout().flag_names.offset + 24).div_ceil(8)],
		context: vec![0; 0x2200 / 8],
		manager: vec![0; (layout().country_manager_index_array.offset + 8) / 8],
		pointers: vec![0; slots],
		countries: Vec::with_capacity(specs.countries.len()),
		flag_ids: Vec::with_capacity(specs.countries.len()),
		flag_dates: Vec::with_capacity(specs.countries.len()),
		flag_names: Vec::new(),
		flag_payloads: Vec::new(),
		provinces: Vec::new(),
		node_ptrs: vec![0; specs.nodes.len()],
		nodes: Vec::with_capacity(specs.nodes.len()),
		defs: Vec::with_capacity(specs.nodes.len()),
		routes: Vec::with_capacity(specs.nodes.len()),
		members: Vec::with_capacity(specs.nodes.len()),
		empty_route: Box::new(0),
	};
	plant_countries(&mut live, specs)?;
	plant_provinces(&mut live, specs)?;
	plant_trade(&mut live, specs)?;
	plant_roots(&mut live, slots, specs)?;
	// Kernel copies are the only later readers of these bytes.
	std::hint::black_box(&live);
	Ok(live)
}

fn plant_countries(live: &mut Live, specs: &Specs) -> Result<(), &'static str> {
	let mut names = Vec::new();
	let mut per_country_ids = Vec::with_capacity(specs.countries.len());
	for country in &specs.countries {
		let mut ids = Vec::with_capacity(country.flags.len());
		for (name, _) in &country.flags {
			let id = if let Some(found) = names.iter().position(|have: &String| have == name) {
				found
			} else {
				names.push(name.clone());
				names.len() - 1
			};
			ids.push(u16::try_from(id).map_err(|_| "too many flag names")?);
		}
		per_country_ids.push(ids);
	}
	if names.len() > 32_768 {
		return Err("too many flag names");
	}
	live.flag_payloads.reserve(names.len());
	live.flag_names.reserve(names.len());
	for name in &names {
		live.flag_names
			.push(name_slot(name, &mut live.flag_payloads)?);
	}
	// Payload pointers were taken only after every long name was pushed.
	for (country, ids) in specs.countries.iter().zip(per_country_ids) {
		let mut raw = vec![0u64; COUNTRY_BYTES.div_ceil(8)];
		poke_u64(
			&mut raw,
			layout().country_tag.offset,
			pack_tag(country.tag, country.index, 1),
		)?;
		poke_i32(&mut raw, layout().country_treasury.offset, country.treasury)?;
		if let Some((tag, index)) = country.overlord {
			poke_u64(
				&mut raw,
				layout().country_overlord.offset,
				pack_tag(tag, index, 1),
			)?;
		}
		let dates: Vec<i32> = country.flags.iter().map(|(_, hours)| *hours).collect();
		if !ids.is_empty() {
			write_vector(
				&mut raw,
				layout().flags_country_flags.offset + layout().flags_flag_ids.offset,
				ids.as_ptr() as u64,
				ids.len() * 2,
			)?;
			write_vector(
				&mut raw,
				layout().flags_country_flags.offset + layout().flags_flag_dates.offset,
				dates.as_ptr() as u64,
				dates.len() * 4,
			)?;
		}
		live.pointers[usize::from(country.index)] = raw.as_ptr() as u64;
		live.countries.push(raw);
		live.flag_ids.push(ids);
		live.flag_dates.push(dates);
	}
	if !live.flag_names.is_empty() {
		write_vector(
			&mut live.image,
			layout().flag_names.offset,
			live.flag_names.as_ptr() as u64,
			live.flag_names.len() * layout().flags_string_size.offset,
		)?;
	}
	Ok(())
}

fn plant_provinces(live: &mut Live, specs: &Specs) -> Result<(), &'static str> {
	let Some(max_id) = specs.provinces.iter().map(|province| province.id).max() else {
		return Ok(());
	};
	let slots = usize::try_from(max_id).unwrap() + 1;
	let bytes = slots
		.checked_mul(layout().province_cprovince_size.offset)
		.ok_or("province map overflows")?;
	live.provinces = vec![0; bytes.div_ceil(8)];
	for province_spec in &specs.provinces {
		let base =
			usize::try_from(province_spec.id).unwrap() * layout().province_cprovince_size.offset;
		poke_i32(
			&mut live.provinces,
			base + layout().province_tax.offset,
			province_spec.tax,
		)?;
		poke_i32(
			&mut live.provinces,
			base + layout().province_manpower.offset,
			province_spec.manpower,
		)?;
		for (offset, bytes) in &province_spec.patches {
			poke_bytes(&mut live.provinces, base + offset, bytes)?;
		}
	}
	poke_u64(
		&mut live.context,
		layout().province_cprovinces.offset,
		live.provinces.as_ptr() as u64,
	)
}

fn plant_trade(live: &mut Live, specs: &Specs) -> Result<(), &'static str> {
	for spec in &specs.nodes {
		live.nodes
			.push(vec![0; (layout().node_definition.offset + 8).div_ceil(8)]);
		live.defs
			.push(vec![0; (layout().def_routes_end.offset + 8).div_ceil(8)]);
		live.members.push(
			spec.members
				.iter()
				.copied()
				.map(i32::try_from)
				.collect::<Result<Vec<_>, _>>()
				.map_err(|_| "trade node member province id must be positive")?,
		);
		live.routes
			.push(vec![0; spec.routes.len() * layout().route_size.offset / 8]);
	}
	let empty = &*live.empty_route as *const u64 as u64;
	for (index, spec) in specs.nodes.iter().enumerate() {
		let definition = live.defs[index].as_ptr() as u64;
		poke_u64(
			&mut live.nodes[index],
			layout().node_definition.offset,
			definition,
		)?;
		if !spec.members.is_empty() {
			write_vector(
				&mut live.defs[index],
				layout().def_members.offset,
				live.members[index].as_ptr() as u64,
				spec.members.len() * 4,
			)?;
		}
		if spec.routes.is_empty() {
			poke_u64(&mut live.defs[index], layout().def_routes.offset, empty)?;
			poke_u64(&mut live.defs[index], layout().def_routes_end.offset, empty)?;
		} else {
			let begin = live.routes[index].as_ptr() as u64;
			for (slot, target) in spec.routes.iter().enumerate() {
				let target_def = live.defs[*target].as_ptr() as u64;
				poke_u64(
					&mut live.routes[index],
					slot * layout().route_size.offset + layout().route_target.offset,
					target_def,
				)?;
			}
			poke_u64(&mut live.defs[index], layout().def_routes.offset, begin)?;
			poke_u64(
				&mut live.defs[index],
				layout().def_routes_end.offset,
				begin + (spec.routes.len() * layout().route_size.offset) as u64,
			)?;
		}
		live.node_ptrs[index] = live.nodes[index].as_ptr() as u64;
	}
	if specs.nodes.is_empty() {
		return Ok(());
	}
	let db_at = layout().context_db.offset;
	poke_u64(
		&mut live.context,
		db_at + layout().db_nodes.offset,
		live.node_ptrs.as_ptr() as u64,
	)?;
	poke_i32(
		&mut live.context,
		db_at + layout().db_node_count.offset,
		i32::try_from(specs.nodes.len()).map_err(|_| "too many trade nodes")?,
	)
}

fn plant_roots(live: &mut Live, slots: usize, specs: &Specs) -> Result<(), &'static str> {
	if !specs.countries.is_empty() {
		poke_u64(
			&mut live.manager,
			layout().country_manager_index_array.offset,
			live.pointers.as_ptr() as u64,
		)?;
		poke_u64(
			&mut live.image,
			layout().country_manager_global.offset,
			live.manager.as_ptr() as u64,
		)?;
		poke_i32(
			&mut live.context,
			layout().countries_count.offset,
			i32::try_from(slots).map_err(|_| "country registry does not fit")?,
		)?;
	}
	poke_u64(
		&mut live.image,
		layout().context_global.offset,
		live.context.as_ptr() as u64,
	)
}

fn name_slot(name: &str, payloads: &mut Vec<Vec<u8>>) -> Result<[u64; 4], &'static str> {
	let bytes = name.as_bytes();
	let mut slot = [0u64; 4];
	slot[2] = bytes.len() as u64;
	if bytes.len() <= 15 {
		let mut inline = [0u8; 16];
		inline[..bytes.len()].copy_from_slice(bytes);
		slot[0] = u64::from_le_bytes(inline[..8].try_into().unwrap());
		slot[1] = u64::from_le_bytes(inline[8..].try_into().unwrap());
		slot[3] = 15;
	} else {
		let mut owned = bytes.to_vec();
		owned.push(0);
		payloads.push(owned);
		slot[0] = payloads.last().unwrap().as_ptr() as u64;
		slot[3] = bytes.len() as u64;
	}
	Ok(slot)
}

fn tag_bytes(tag: &str) -> Result<[u8; 3], &'static str> {
	let bytes = tag.as_bytes();
	if bytes.len() != 3
		|| !bytes
			.iter()
			.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
	{
		return Err("country tag must be three uppercase letters or digits");
	}
	Ok([bytes[0], bytes[1], bytes[2]])
}

fn valid_flag_name(name: &str) -> Result<(), &'static str> {
	let bytes = name.as_bytes();
	if bytes.is_empty() || bytes.len() > 64 || !bytes.iter().copied().all(is_name_byte) {
		return Err("flag name must be 1..=64 display-name bytes");
	}
	Ok(())
}

fn pack_tag(tag: [u8; 3], index: u16, valid_flag: u8) -> u64 {
	let idx = (index as i16).to_le_bytes();
	u64::from_le_bytes([tag[0], tag[1], tag[2], 0, idx[0], idx[1], 0, valid_flag])
}

fn write_vector(
	buf: &mut [u64],
	offset: usize,
	begin: u64,
	bytes: usize,
) -> Result<(), &'static str> {
	let len = u64::try_from(bytes).map_err(|_| "vector is too large")?;
	poke_u64(buf, offset, begin)?;
	poke_u64(buf, offset + 8, begin + len)?;
	poke_u64(buf, offset + 16, begin + len)
}

fn poke_u64(buf: &mut [u64], offset: usize, value: u64) -> Result<(), &'static str> {
	if !offset.is_multiple_of(8) {
		return Err("internal pointer field is misaligned");
	}
	let index = offset / 8;
	*buf.get_mut(index)
		.ok_or("synthetic buffer is shorter than the field")? = value;
	Ok(())
}

fn poke_i32(buf: &mut [u64], offset: usize, value: i32) -> Result<(), &'static str> {
	if !offset.is_multiple_of(4) {
		return Err("internal i32 field is misaligned");
	}
	let index = offset / 8;
	let shift = (offset % 8) * 8;
	let word = buf
		.get_mut(index)
		.ok_or("synthetic buffer is shorter than the field")?;
	let mask = !(u64::from(u32::MAX) << shift);
	*word = (*word & mask) | (u64::from(value as u32) << shift);
	Ok(())
}

fn poke_bytes(buf: &mut [u64], offset: usize, bytes: &[u8]) -> Result<(), &'static str> {
	let end = offset
		.checked_add(bytes.len())
		.ok_or("province patch overflows")?;
	if end > buf.len() * 8 {
		return Err("province patch leaves the record");
	}
	for (index, byte) in bytes.iter().enumerate() {
		let at = offset + index;
		let word = at / 8;
		let shift = (at % 8) * 8;
		let mask = !(0xffu64 << shift);
		buf[word] = (buf[word] & mask) | (u64::from(*byte) << shift);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dataplane::{Memory, Province, pinned_exe_sha256};

	#[test]
	fn synthetic_scene_reads_back_through_public_views() {
		let mut scene = FakeGame::new();
		scene
			.add_country("FRA", 1, 1_500, Some(("HAB", 2)), &[("foch_dp_flag", 90)])
			.unwrap();
		scene.add_country("HAB", 2, 50, None, &[]).unwrap();
		scene.add_province(4, 12_000, 6_000).unwrap();
		scene.write_province(4, 0x10, &[1, 2, 3, 4]).unwrap();
		scene.add_trade_node(&[1], &[4, 12]).unwrap();
		scene.add_trade_node(&[], &[]).unwrap();

		let game = scene.game().unwrap();
		let context = game.context;
		let base = scene.exe_base().unwrap();
		assert_eq!(Eu4Game::from_exe_base(base).unwrap().context, context);
		assert_eq!(Eu4Game::from_exe_base(base).unwrap().exe_base, base);

		let fra = scene.country(1).unwrap();
		assert_eq!(fra.tag(), "FRA");
		assert_eq!(fra.try_tag().as_deref(), Some("FRA"));
		assert_eq!(fra.index(), Some(1));
		assert_eq!(fra.treasury(), Some(1_500));
		assert!(fra.raw_address() > 0x1_0000);
		let overlord = fra.overlord_tag().unwrap();
		assert_eq!(overlord.tag, "HAB");
		assert_eq!(overlord.index, 2);
		assert_eq!(fra.independence(), Some(false));
		let hab = scene.country(2).unwrap();
		assert_eq!(hab.independence(), Some(true));
		assert_eq!(hab.treasury(), Some(50));
		assert!(scene.country(0).is_err());

		// SAFETY: the scene owns the country, its flag vectors, and the name
		// table for this synchronous read.
		let flags = unsafe { game.country_flags(&fra, pinned_exe_sha256()) }.unwrap();
		assert_eq!(flags.country_tag, "FRA");
		assert_eq!(flags.flags.len(), 1);
		assert_eq!(flags.flags[0].name, "foch_dp_flag");
		assert_eq!(flags.flags[0].set_at_hours, 90);

		let province = game.province(4).unwrap();
		assert_eq!(province.tax(), Some(12_000));
		assert_eq!(province.manpower(), Some(6_000));
		let raw = province.raw_address();
		assert_eq!(Memory.read_u8(raw + 0x10), Some(1));
		assert_eq!(Memory.read_u8(raw + 0x13), Some(4));

		let nodes = game.trade_nodes().unwrap();
		assert_eq!(nodes.count(), 2);
		let first = nodes.node(0).unwrap();
		let second = nodes.node(1).unwrap();
		assert_eq!(first.outgoing_targets(&nodes), Some(vec![1]));
		assert_eq!(first.member_provinces(), Some(vec![4, 12]));
		assert_eq!(first.live_route_count(&nodes), Some(1));
		assert_eq!(second.live_route_count(&nodes), Some(0));
		assert_eq!(second.outgoing_targets(&nodes), Some(vec![]));
		assert_eq!(second.member_provinces(), Some(vec![]));
		assert!(scene.add_country("ENG", 3, 0, None, &[]).is_err());
	}

	#[test]
	fn builder_rejects_tags_duplicates_and_patches_outside_the_record() {
		let mut scene = FakeGame::new();
		assert!(scene.add_country("fr", 0, 0, None, &[]).is_err());
		assert!(scene.add_country("fra", 0, 0, None, &[]).is_err());
		scene.add_country("FRA", 1, 0, None, &[]).unwrap();
		assert!(scene.add_country("ENG", 1, 0, None, &[]).is_err());
		assert!(
			scene
				.add_country("ENG", i16::MAX as u16, 0, None, &[])
				.is_err()
		);
		scene.add_province(1, 1, 1).unwrap();
		assert!(scene.add_province(1, 2, 2).is_err());
		assert!(scene.write_province(9, 0, &[1]).is_err());
		let span = Province::record_span();
		assert!(scene.write_province(1, span, &[1]).is_err());
		assert!(scene.write_province(1, span - 1, &[1, 2]).is_err());
		scene.add_trade_node(&[4], &[1]).unwrap();
		assert!(scene.game().is_err());
		assert!(scene.add_country("ENG", 2, 0, None, &[]).is_err());
	}
}
