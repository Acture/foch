#![cfg(windows)]

use foch_runtime::dataplane::{
	CountryFlag, CountryFlags, CountryView, Eu4Game, FlagReadError, Memory, layout,
};

// Synthetic allocations exercise the actual shared kernel reader. The exact
// digest is a fixture input, never evidence of a live game or verified layout.
struct Fixture {
	image: Vec<u64>,
	context: Vec<u64>,
	manager: Vec<u64>,
	addresses: Vec<u64>,
	country: Vec<u64>,
	names: Vec<[u64; 4]>,
	_heap_names: Vec<Vec<u8>>,
	ids: Vec<u16>,
	dates: Vec<i32>,
}

fn vector(buffer: &mut [u64], offset: usize, pointer: usize, count: usize, stride: usize) {
	let begin = if count == 0 { 0 } else { pointer as u64 };
	buffer[offset / 8] = begin;
	buffer[offset / 8 + 1] = begin + (count * stride) as u64;
	buffer[offset / 8 + 2] = buffer[offset / 8 + 1];
}

impl Fixture {
	fn new(keys: &[&str], ids: &[u16], dates: &[i32]) -> Self {
		let mut names = vec![];
		let mut heap_names = vec![];
		for name in keys {
			let mut string = [0u64; 4];
			if name.len() <= 15 {
				let mut inline = [0u8; 16];
				inline[..name.len()].copy_from_slice(name.as_bytes());
				string[0] = u64::from_le_bytes(inline[..8].try_into().unwrap());
				string[1] = u64::from_le_bytes(inline[8..].try_into().unwrap());
				string[3] = 15;
			} else {
				let mut heap = name.as_bytes().to_vec();
				heap.push(0);
				string[0] = heap.as_ptr() as u64;
				string[3] = name.len() as u64;
				heap_names.push(heap);
			}
			string[2] = name.len() as u64;
			names.push(string);
		}
		let mut fixture = Self {
			image: vec![0; (0x2357d80 + 24) / 8],
			context: vec![0; 0x1d20 / 8],
			manager: vec![0; 0x120 / 8],
			addresses: vec![0; 1],
			country: vec![0; 0x2b0 / 8],
			names,
			_heap_names: heap_names,
			ids: ids.into(),
			dates: dates.into(),
		};
		fixture.image[layout::COUNTRY_MANAGER_GLOBAL_RVA / 8] = fixture.manager.as_ptr() as u64;
		fixture.manager[layout::country_manager::INDEX_ARRAY / 8] =
			fixture.addresses.as_ptr() as u64;
		fixture.addresses[0] = fixture.country.as_ptr() as u64;
		fixture.context[layout::COUNTRIES_COUNT / 8] = 1u64 << 32;
		fixture.country[layout::country::TAG / 8] = u64::from_le_bytes(*b"FRA\0\0\0\0\x01");
		fixture.headers();
		fixture
	}

	fn headers(&mut self) {
		vector(
			&mut self.image,
			0x2357d80,
			self.names.as_ptr() as usize,
			self.names.len(),
			32,
		);
		vector(
			&mut self.country,
			0x280,
			self.ids.as_ptr() as usize,
			self.ids.len(),
			2,
		);
		vector(
			&mut self.country,
			0x298,
			self.dates.as_ptr() as usize,
			self.dates.len(),
			4,
		);
	}

	fn view(&self) -> CountryView {
		Eu4Game {
			memory: Memory,
			exe_base: self.image.as_ptr() as usize,
			context: self.context.as_ptr() as usize,
		}
		.countries()
		.unwrap()
		.country(0)
		.unwrap()
	}

	fn read(&self) -> Result<CountryFlags, FlagReadError> {
		// SAFETY: every simulated object and referenced buffer remains live and
		// unchanged during this synchronous synthetic read.
		unsafe {
			self.view()
				.flags(self.image.as_ptr() as usize, layout::EXPECTED_EXE_SHA256)
		}
	}
}

#[test]
fn country_registry_view_resolves_flag_ids_to_exact_names_and_parallel_dates() {
	let fixture = Fixture::new(
		&[
			"unused",
			"foch_dp_cmd_propose_SWE_HAB_defense_365",
			"accepted",
		],
		&[2, 1],
		&[90, 123],
	);
	let snapshot = fixture.read().unwrap();
	assert_eq!(snapshot.country_tag, "FRA");
	assert_eq!(
		snapshot.flags,
		vec![
			CountryFlag {
				name: "accepted".into(),
				set_at_hours: 90
			},
			CountryFlag {
				name: "foch_dp_cmd_propose_SWE_HAB_defense_365".into(),
				set_at_hours: 123
			},
		]
	);
}

#[test]
fn empty_flags_are_distinct_from_unsupported_or_unreadable_fields() {
	let mut fixture = Fixture::new(&[], &[], &[]);
	assert!(fixture.read().unwrap().flags.is_empty());
	for digest in ["", "c9ec55a4", "different"] {
		assert_eq!(
			unsafe { fixture.view().flags(usize::MAX, digest) },
			Err(FlagReadError::UnsupportedBuild)
		);
	}
	fixture.country[0x280 / 8] = 0x10000;
	fixture.country[0x288 / 8] = 0x10002;
	fixture.country[0x290 / 8] = 0x10002;
	assert!(fixture.read().is_err());
	assert!(matches!(
		unsafe {
			fixture
				.view()
				.flags(usize::MAX - 7, layout::EXPECTED_EXE_SHA256)
		},
		Err(FlagReadError::InvalidLayout(_))
	));
}

#[test]
fn invalid_counts_alignment_and_parallel_membership_are_refused() {
	let mut fixture = Fixture::new(&["one", "two"], &[0, 1], &[10, 20]);
	fixture.country[0x2a0 / 8] -= 4;
	assert_eq!(fixture.read(), Err(FlagReadError::Inconsistent));
	fixture.headers();
	fixture.country[0x290 / 8] = fixture.country[0x280 / 8] + 2;
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::InvalidLayout(_))
	));
	fixture.headers();
	fixture.country[0x280 / 8] += 1;
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::InvalidLayout(_))
	));
	fixture.headers();
	fixture.country[0x288 / 8] = fixture.country[0x280 / 8] + 4097 * 2;
	fixture.country[0x290 / 8] = fixture.country[0x288 / 8];
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::LimitExceeded(_))
	));
}

#[test]
fn invalid_or_duplicate_ids_and_malformed_names_never_become_commands() {
	let mut fixture = Fixture::new(&["one", "two"], &[0, 1], &[10, 20]);
	fixture.ids[0] = u16::MAX;
	assert!(fixture.read().is_err());
	fixture.ids[0] = 2;
	assert!(fixture.read().is_err());
	fixture.ids[0] = 1;
	assert_eq!(fixture.read(), Err(FlagReadError::Inconsistent));
	fixture.ids[0] = 0;
	fixture.names[0][2] = 16;
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::InvalidLayout(_))
	));
	fixture.names[0][3] = 100;
	fixture.names[0][2] = 65;
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::LimitExceeded(_))
	));
	fixture.names[0] = fixture.names[1];
	assert_eq!(fixture.read(), Err(FlagReadError::Inconsistent));
}

#[test]
fn allocated_empty_vectors_are_valid_and_global_registry_bounds_are_checked() {
	let mut fixture = Fixture::new(&["one"], &[0], &[10]);
	fixture.country[0x288 / 8] = fixture.country[0x280 / 8];
	fixture.country[0x2a0 / 8] = fixture.country[0x298 / 8];
	assert!(fixture.read().unwrap().flags.is_empty());
	fixture.headers();
	let names = fixture.image[0x2357d80 / 8];
	fixture.image[(0x2357d80 + 8) / 8] = names + 32769 * 32;
	fixture.image[(0x2357d80 + 16) / 8] = names + 32769 * 32;
	assert!(matches!(
		fixture.read(),
		Err(FlagReadError::LimitExceeded(_))
	));
	fixture.headers();
	fixture.country[0x298 / 8] = 0x20000;
	fixture.country[0x2a0 / 8] = 0x20004;
	fixture.country[0x2a8 / 8] = 0x20004;
	assert!(matches!(fixture.read(), Err(FlagReadError::Unavailable(_))));
}
