//! Bounded country-flag snapshots through the shared reader.
//!
//! The candidate fields here follow static engine serialization and flag lookup
//! paths for the pinned executable. They have not passed a Foch game probe.
//! Reading a snapshot grants no permission to set, remove or consume flags.

use super::{CountryView, Memory, StdVector, is_name_byte, layout, read_sso, supports_build};
use std::collections::BTreeSet;

// Keep static candidates out of layout's live-verified field set.
const COUNTRY_FLAGS: usize = 0x270;
const FLAG_IDS: usize = 0x10;
const FLAG_DATES: usize = 0x28;
const FLAG_NAMES_RVA: usize = 0x2357d80;
const MAX_FLAGS: usize = 4096;
const MAX_NAMES: usize = 32768; // The engine's IDs are signed 16-bit values.
const STRING_SIZE: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountryFlag {
	pub name: String,
	/// Engine calendar hours, preserved without an invented date conversion.
	pub set_at_hours: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountryFlags {
	pub country_tag: String,
	/// Original engine order; an empty vector is a successful empty snapshot.
	pub flags: Vec<CountryFlag>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlagReadError {
	UnsupportedBuild,
	UnsupportedPlatform,
	Unavailable(&'static str),
	InvalidLayout(&'static str),
	LimitExceeded(&'static str),
	Inconsistent,
}

impl std::fmt::Display for FlagReadError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::UnsupportedBuild => {
				formatter.write_str("unsupported executable identity for flags")
			}
			Self::UnsupportedPlatform => formatter.write_str("country flags require Windows x64"),
			Self::Unavailable(field) => {
				write!(formatter, "country flag field unavailable: {field}")
			}
			Self::InvalidLayout(field) => write!(formatter, "invalid country flag layout: {field}"),
			Self::LimitExceeded(field) => write!(formatter, "country flag limit exceeded: {field}"),
			Self::Inconsistent => formatter.write_str("country flag snapshot is inconsistent"),
		}
	}
}

impl std::error::Error for FlagReadError {}

fn add(address: usize, offset: usize) -> Result<usize, FlagReadError> {
	address
		.checked_add(offset)
		.ok_or(FlagReadError::InvalidLayout("address overflow"))
}

fn qword(memory: &Memory, address: usize, offset: usize) -> Result<u64, FlagReadError> {
	memory
		.read_qword(add(address, offset)?)
		.ok_or(FlagReadError::Unavailable("qword"))
}

#[derive(Debug, Eq, PartialEq)]
struct Vector {
	header: StdVector,
	count: usize,
}

impl Vector {
	fn read(
		memory: &Memory,
		address: usize,
		stride: usize,
		limit: usize,
	) -> Result<Self, FlagReadError> {
		let header = StdVector {
			begin: qword(memory, address, 0)? as usize,
			end: qword(memory, address, 8)? as usize,
			capacity: qword(memory, address, 16)? as usize,
		};
		let count = header
			.len(stride, usize::MAX)
			.ok_or(FlagReadError::InvalidLayout("vector bounds"))?;
		if (header.capacity - header.begin) / stride > limit {
			return Err(FlagReadError::LimitExceeded("vector capacity"));
		}
		Ok(Self { header, count })
	}
}

#[derive(Debug, Eq, PartialEq)]
struct Sample {
	identity: u64,
	ids: Vector,
	flag_ids: Vec<i16>,
	dates: Vector,
	names: Vector,
	string_headers: Vec<[u64; 4]>,
	snapshot: CountryFlags,
}

fn string_header(memory: &Memory, address: usize) -> Result<[u64; 4], FlagReadError> {
	Ok([
		qword(memory, address, 0)?,
		qword(memory, address, 8)?,
		qword(memory, address, 16)?,
		qword(memory, address, 24)?,
	])
}

fn sample(memory: &Memory, address: usize, names_address: usize) -> Result<Sample, FlagReadError> {
	let identity = qword(memory, address, layout::country::TAG)?;
	let country_tag = CountryView::tag_at(memory, address);
	if country_tag.len() != 3
		|| !country_tag
			.bytes()
			.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
	{
		return Err(FlagReadError::InvalidLayout("country tag"));
	}
	let flags_address = add(address, COUNTRY_FLAGS)?;
	let ids = Vector::read(memory, add(flags_address, FLAG_IDS)?, 2, MAX_FLAGS)?;
	let dates = Vector::read(memory, add(flags_address, FLAG_DATES)?, 4, MAX_FLAGS)?;
	let names = Vector::read(memory, names_address, STRING_SIZE, MAX_NAMES)?;
	if ids.count != dates.count {
		return Err(FlagReadError::Inconsistent);
	}
	let mut flags = Vec::with_capacity(ids.count);
	let mut flag_ids = Vec::with_capacity(ids.count);
	let mut string_headers = Vec::with_capacity(ids.count);
	let mut seen_ids = BTreeSet::new();
	let mut seen_names = BTreeSet::new();
	for index in 0..ids.count {
		let id_address = add(
			ids.header.begin,
			index
				.checked_mul(2)
				.ok_or(FlagReadError::InvalidLayout("ID stride"))?,
		)?;
		let low = memory
			.read_u8(id_address)
			.ok_or(FlagReadError::Unavailable("flag ID"))?;
		let high = memory
			.read_u8(add(id_address, 1)?)
			.ok_or(FlagReadError::Unavailable("flag ID"))?;
		let id = i16::from_le_bytes([low, high]);
		if id < 0 || id as usize >= names.count {
			return Err(FlagReadError::InvalidLayout("flag ID"));
		}
		if !seen_ids.insert(id) {
			return Err(FlagReadError::Inconsistent);
		}
		let string_address = add(
			names.header.begin,
			(id as usize)
				.checked_mul(STRING_SIZE)
				.ok_or(FlagReadError::InvalidLayout("name stride"))?,
		)?;
		let header = string_header(memory, string_address)?;
		if header[2] > 64 || header[3] > 1_048_576 {
			return Err(FlagReadError::LimitExceeded("flag name"));
		}
		if header[2] == 0 || header[3] < 15 || header[2] > header[3] {
			return Err(FlagReadError::InvalidLayout("flag name capacity"));
		}
		let name = read_sso(memory, string_address, is_name_byte)
			.ok_or(FlagReadError::Unavailable("flag name"))?;
		if string_header(memory, string_address)? != header || !seen_names.insert(name.clone()) {
			return Err(FlagReadError::Inconsistent);
		}
		let date_address = add(
			dates.header.begin,
			index
				.checked_mul(4)
				.ok_or(FlagReadError::InvalidLayout("date stride"))?,
		)?;
		let set_at_hours = memory
			.read_i32(date_address)
			.ok_or(FlagReadError::Unavailable("flag date"))?;
		flags.push(CountryFlag { name, set_at_hours });
		flag_ids.push(id);
		string_headers.push(header);
	}
	Ok(Sample {
		identity,
		ids,
		flag_ids,
		dates,
		names,
		string_headers,
		snapshot: CountryFlags { country_tag, flags },
	})
}

fn consistent(
	mut read: impl FnMut() -> Result<Sample, FlagReadError>,
) -> Result<CountryFlags, FlagReadError> {
	let first = read()?;
	if read()? != first {
		return Err(FlagReadError::Inconsistent);
	}
	Ok(first.snapshot)
}

pub(super) fn read(
	memory: &Memory,
	address: usize,
	exe_base: usize,
	host_exe_sha256: &str,
) -> Result<CountryFlags, FlagReadError> {
	// Callers pass FochHostApi.exe_sha256, never a substitute expected digest.
	if !supports_build(host_exe_sha256) {
		return Err(FlagReadError::UnsupportedBuild);
	}
	if !cfg!(all(windows, target_arch = "x86_64")) {
		return Err(FlagReadError::UnsupportedPlatform);
	}
	let names_address = add(exe_base, FLAG_NAMES_RVA)?;
	consistent(|| sample(memory, address, names_address))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_bad_digest_precedes_even_address_calculation() {
		for digest in [
			"".into(),
			"c9ec55a4".into(),
			layout::EXPECTED_EXE_SHA256.to_uppercase(),
			format!("{}0", layout::EXPECTED_EXE_SHA256),
		] {
			assert_eq!(
				read(&Memory, usize::MAX, usize::MAX, &digest),
				Err(FlagReadError::UnsupportedBuild)
			);
		}
	}

	#[test]
	#[cfg(windows)]
	fn any_observed_change_between_samples_refuses_the_complete_snapshot() {
		let mut country = vec![0u64; 0x2b0 / 8];
		country[layout::country::TAG / 8] = u64::from_le_bytes(*b"FRA\0\0\0\0\x01");
		let mut ids = [0u16];
		let mut dates = [123i32];
		let mut names = [[u64::from_le_bytes(*b"flag\0\0\0\0"), 0, 4, 15]; 2];
		let registry = [
			names.as_ptr() as u64,
			names.as_ptr() as u64 + 64,
			names.as_ptr() as u64 + 64,
		];
		for (offset, begin, stride) in [
			(0x280, ids.as_ptr() as u64, 2),
			(0x298, dates.as_ptr() as u64, 4),
		] {
			country[offset / 8] = begin;
			country[(offset + 8) / 8] = begin + stride;
			country[(offset + 16) / 8] = begin + stride;
		}
		let read_once = |country: &[u64]| {
			sample(
				&Memory,
				country.as_ptr() as usize,
				registry.as_ptr() as usize,
			)
		};
		assert_eq!(
			consistent(|| read_once(&country)).unwrap().flags[0].set_at_hours,
			123
		);
		for change in 0..4 {
			let mut calls = 0;
			let result = consistent(|| {
				calls += 1;
				if calls == 2 {
					match change {
						0 => dates[0] += 1,
						1 => {
							for name in &mut names {
								name[0] = u64::from_le_bytes(*b"FLAG\0\0\0\0");
							}
						}
						2 => country[layout::country::TAG / 8] ^= 1,
						_ => ids[0] = 1, // Same printed name at another interned ID.
					}
				}
				std::hint::black_box((&ids, &dates, &names));
				read_once(&country)
			});
			assert_eq!(result, Err(FlagReadError::Inconsistent));
			assert_eq!(calls, 2);
		}
	}
}
