//! One war laid out the way `WAR_SCORE_TOTAL` and `WAR_SCORE_SIDE` read it.
//!
//! Participant ids sit in the two lists on the war. A country's provinces are a
//! linked list at [`country::PROVINCES`]. Each province is
//! [`shared_province::CPROVINCE_SIZE`] bytes, which is the stride multiplied by
//! the index stored in the list node.

use foch_eu4_state::candidates::{country, province, war};
use foch_runtime::dataplane::layout::{country as shared_country, province as shared_province};

const WAR_BYTES: usize = 0x1c0;
const COUNTRY_BYTES: usize = 0x1900;
const LINK_BYTES: usize = 0x18;

/// Country id compared by [`WAR_SCORE_TOTAL`]: the index is bits 32..48.
pub fn country_id(index: u16) -> u64 {
	u64::from(index) << 32
}

pub fn country_index(id: u64) -> u16 {
	(id >> 32) as u16
}

struct Link {
	province: usize,
	node: Box<[u8]>,
}

/// One of the two participant lists on the war.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarSide {
	A,
	B,
}

/// Attacker, defender, their countries, and the provinces those countries own.
pub struct WarObjects {
	war: Vec<u8>,
	side_a: Box<[u64]>,
	side_b: Box<[u64]>,
	countries: Vec<Vec<u8>>,
	provinces: Vec<Vec<u8>>,
	owners: Vec<u16>,
	links: Vec<Link>,
}

impl WarObjects {
	pub fn new(attacker: u16, defender: u16) -> Self {
		let mut war = Self {
			war: vec![0; WAR_BYTES],
			side_a: Box::new([country_id(attacker)]),
			side_b: Box::new([country_id(defender)]),
			countries: Vec::new(),
			provinces: Vec::new(),
			owners: Vec::new(),
			links: Vec::new(),
		};
		war.ensure_country(attacker);
		war.ensure_country(defender);
		war.relink();
		war
	}

	pub fn war(&self) -> &[u8] {
		&self.war
	}

	pub fn country(&self, index: u16) -> &[u8] {
		&self.countries[usize::from(index)]
	}

	pub fn province(&self, index: usize) -> &[u8] {
		&self.provinces[index]
	}

	pub fn add_province(&mut self, owner: u16, controller: u16, flags: u8) -> usize {
		self.ensure_country(owner);
		self.ensure_country(controller);
		let mut province = vec![0; shared_province::CPROVINCE_SIZE];
		write_u32(&mut province, province::MARK, self.provinces.len() as u32);
		write_u64(&mut province, province::OWNER_INDEX, country_id(owner));
		write_u64(&mut province, province::CONTROLLER, country_id(controller));
		province[province::FLAGS] = flags;
		let index = self.provinces.len();
		self.provinces.push(province);
		self.owners.push(owner);
		self.relink();
		index
	}

	pub fn set_manpower(&mut self, country: u16, current: i32, max: i32) {
		self.ensure_country(country);
		let bytes = &mut self.countries[usize::from(country)];
		write_i32(bytes, country::MANPOWER, current);
		write_i32(bytes, country::MANPOWER_MAX, max);
	}

	pub fn set_treasury(&mut self, country: u16, value: i32) {
		self.ensure_country(country);
		write_i32(
			&mut self.countries[usize::from(country)],
			shared_country::TREASURY,
			value,
		);
	}

	/// Change one province's controller and occupied flags, the way a
	/// finished siege flips it.
	pub fn reoccupy(&mut self, province: usize, controller: u16, flags: u8) {
		let bytes = &mut self.provinces[province];
		write_u64(bytes, province::CONTROLLER, country_id(controller));
		bytes[province::FLAGS] = flags;
		self.relink();
	}

	pub fn set_mark(&mut self, country: u16, province: usize) {
		self.ensure_country(country);
		let mark = read_u32(self.province(province), province::MARK);
		write_u32(
			&mut self.countries[usize::from(country)],
			country::MARK,
			mark,
		);
	}

	pub fn set_overlord(&mut self, country: u16, overlord: u16) {
		self.ensure_country(country);
		self.ensure_country(overlord);
		write_u64(
			&mut self.countries[usize::from(country)],
			country::OVERLORD,
			country_id(overlord),
		);
	}

	pub fn province_is_occupied(&self, index: usize) -> bool {
		self.provinces[index][province::FLAGS] & province::OCCUPIED_MASK == province::OCCUPIED_MASK
	}

	/// The country ids on one side, in list order.
	pub fn participants(&self, side: WarSide) -> &[u64] {
		match side {
			WarSide::A => &self.side_a,
			WarSide::B => &self.side_b,
		}
	}

	pub fn owner(&self, index: usize) -> u16 {
		self.owners[index]
	}

	pub fn controller(&self, index: usize) -> u16 {
		let province = &self.provinces[index];
		country_index(read_u64(province, province::CONTROLLER))
	}

	/// The flags [`province::OCCUPIED_MASK`] and an owner that is not the controller,
	/// which is how `WAR_SCORE_SIDE` reads one occupied province.
	pub fn occupied(&self, index: usize) -> bool {
		self.province_is_occupied(index) && self.owner(index) != self.controller(index)
	}

	/// The country's mark names this province as its capital.
	pub fn is_capital(&self, country: u16, province: usize) -> bool {
		read_u32(self.country(country), country::MARK)
			== read_u32(&self.provinces[province], province::MARK)
	}

	/// A country fights on the side itself or through its overlord.
	pub fn on_side(&self, country: u16, side: WarSide) -> bool {
		let id = country_id(country);
		let participants = self.participants(side);
		participants.contains(&id)
			|| participants.contains(&read_u64(self.country(country), country::OVERLORD))
	}

	fn ensure_country(&mut self, index: u16) {
		let needed = usize::from(index) + 1;
		if self.countries.len() < needed {
			self.countries
				.resize_with(needed, || vec![0; COUNTRY_BYTES]);
		}
	}

	fn relink(&mut self) {
		self.links.clear();
		for province in 0..self.provinces.len() {
			self.links.push(Link {
				province,
				node: vec![0; LINK_BYTES].into_boxed_slice(),
			});
		}
		let addresses: Vec<u64> = self
			.links
			.iter()
			.map(|link| link.node.as_ptr() as u64)
			.collect();
		let mut next = vec![None; self.provinces.len()];
		let mut head = vec![None; self.countries.len()];
		for (slot, owner) in self.owners.iter().enumerate() {
			let owner = usize::from(*owner);
			next[slot] = head[owner];
			head[owner] = Some(slot);
		}
		for (slot, link) in self.links.iter_mut().enumerate() {
			write_i32(&mut link.node, 0, link.province as i32);
			let follow = next[slot].map(|index| addresses[index]).unwrap_or(0);
			write_u64(&mut link.node, province::LINK_NEXT, follow);
		}
		for (index, country) in self.countries.iter_mut().enumerate() {
			let pointer = head[index].map(|slot| addresses[slot]).unwrap_or(0);
			write_u64(country, country::PROVINCES, pointer);
		}
		write_list(&mut self.war, war::SIDE_A, &self.side_a);
		write_list(&mut self.war, war::SIDE_B, &self.side_b);
		write_i32(&mut self.war, war::SCORE_VALUE, 0);
	}
}

fn write_list(war: &mut [u8], offset: usize, ids: &[u64]) {
	let begin = ids.as_ptr() as u64;
	let end = unsafe { ids.as_ptr().add(ids.len()) } as u64;
	write_u64(war, offset, begin);
	write_u64(war, offset + 8, end);
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
	buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
	buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32(buf: &mut [u8], offset: usize, value: i32) {
	write_u32(buf, offset, value as u32);
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
	u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
fn read_i32(buf: &[u8], offset: usize) -> i32 {
	read_u32(buf, offset) as i32
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_war_has_two_sides_and_the_owners_provinces() {
		let mut war = WarObjects::new(1, 2);
		let province = war.add_province(1, 2, province::OCCUPIED_MASK);
		war.set_manpower(1, 12_000, 20_000);
		war.set_treasury(1, 350);
		war.set_mark(2, province);
		war.set_overlord(2, 1);

		let bytes = war.war();
		let side_a = read_u64(bytes, war::SIDE_A);
		let side_a_end = read_u64(bytes, war::SIDE_A + 8);
		assert_eq!(side_a_end - side_a, 8);
		let attacker = unsafe { *(side_a as *const u64) };
		assert_eq!(country_index(attacker), 1);
		let side_b = read_u64(bytes, war::SIDE_B);
		let defender = unsafe { *(side_b as *const u64) };
		assert_eq!(country_index(defender), 2);

		let head = read_u64(war.country(1), country::PROVINCES);
		assert_ne!(head, 0);
		let index = unsafe { *(head as *const i32) };
		assert_eq!(index, province as i32);
		let next = unsafe { *((head + province::LINK_NEXT as u64) as *const u64) };
		assert_eq!(next, 0);
		assert_eq!(
			country_index(read_u64(war.province(province), province::OWNER_INDEX)),
			1
		);
		assert_eq!(
			country_index(read_u64(war.province(province), province::CONTROLLER)),
			2
		);
		assert!(war.province_is_occupied(province));
		assert_eq!(read_i32(war.country(1), country::MANPOWER), 12_000);
		assert_eq!(read_i32(war.country(1), country::MANPOWER_MAX), 20_000);
		assert_eq!(read_i32(war.country(1), shared_country::TREASURY), 350);
		assert_eq!(read_u32(war.country(2), country::MARK), province as u32);
		assert_eq!(
			country_index(read_u64(war.country(2), country::OVERLORD)),
			1
		);
		assert_eq!(read_i32(bytes, war::SCORE_VALUE), 0);
	}
}
