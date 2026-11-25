//! Load functions in the installed EU4 1.37.5 `eu4.exe`.
//!
//! `1.37.5.json` records a different binary hash, so these RVAs belong only to
//! [`foch_runtime::dataplane::pinned_exe_sha256`]. [`LoadSite::load_rva`]
//! is the function that merges one directory into the database. When it returns,
//! the database holds one object per key. The name to copy is the string on that
//! object, at [`EntryName::offset`].
//!
//! Live-memory struct field offsets live in the shared dataplane views
//! (verified) and [`foch_eu4_state::candidates`] (candidate), never duplicated here.

use crate::LoadedDatabase;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadSite {
	pub database: LoadedDatabase,
	pub directory: &'static str,
	/// Receives the database singleton and the directory path.
	pub load_rva: u32,
	/// Receives the database and one file while that directory is loaded.
	/// The file is an input to the loader. The merged object is what remains
	/// after [`LoadSite::load_rva`] returns.
	pub file_rva: u32,
	pub load_prologue: &'static [u8],
	pub file_prologue: &'static [u8],
}

/// `std::string` on the object the database keeps after the loader returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryName {
	pub database: LoadedDatabase,
	pub offset: u32,
}

pub const ENTRY_NAMES: &[EntryName] = &[
	EntryName {
		database: LoadedDatabase::Ideas,
		offset: 0x158,
	},
	EntryName {
		database: LoadedDatabase::LeaderPersonalities,
		offset: 0x158,
	},
	EntryName {
		database: LoadedDatabase::GovernmentReforms,
		offset: 0x10,
	},
];

/// `std::string` on the wargoal object after [`WARGOAL_LOAD`] returns.
pub const WARGOAL_ENTRY_NAME: u32 = 0x18;

pub fn entry_offset(database: LoadedDatabase) -> Option<u32> {
	ENTRY_NAMES
		.iter()
		.find(|entry| entry.database == database)
		.map(|entry| entry.offset)
}

/// Read the MSVC `std::string` at `offset` on one database object.
///
/// The size qword is at `+0x10` and the capacity qword at `+0x18`, both relative
/// to `offset`. Capacity below 16 keeps the bytes in the object. A larger
/// capacity stores a pointer in the first qword; `heap` reads that many bytes.
pub fn read_entry_name(
	object: &[u8],
	offset: u32,
	mut heap: impl FnMut(u64, usize) -> Option<Vec<u8>>,
) -> Option<String> {
	let start = offset as usize;
	let header = object.get(start..start.checked_add(0x20)?)?;
	let size = usize::try_from(u64::from_le_bytes(header[0x10..0x18].try_into().ok()?)).ok()?;
	let capacity = usize::try_from(u64::from_le_bytes(header[0x18..0x20].try_into().ok()?)).ok()?;
	if size == 0 || size > capacity || size > 4_096 {
		return None;
	}
	let bytes = if capacity < 16 {
		header.get(..size)?.to_vec()
	} else {
		let pointer = u64::from_le_bytes(header[..8].try_into().ok()?);
		let read = heap(pointer, size)?;
		if read.len() != size {
			return None;
		}
		read
	};
	String::from_utf8(bytes).ok()
}

pub const SITES: &[LoadSite] = &[
	LoadSite {
		database: LoadedDatabase::Ideas,
		directory: "/ideas",
		load_rva: 0x0085_08f0,
		file_rva: 0x0085_0fb0,
		load_prologue: &[0x48, 0x89, 0x5c, 0x24, 0x20],
		file_prologue: &[0x48, 0x89, 0x4c, 0x24, 0x08],
	},
	LoadSite {
		database: LoadedDatabase::RulerPersonalities,
		directory: "/ruler_personalities",
		load_rva: 0x00ab_6d70,
		file_rva: 0x00ab_73b0,
		load_prologue: &[0x48, 0x89, 0x5c, 0x24, 0x18],
		file_prologue: &[0x44, 0x88, 0x44, 0x24, 0x18],
	},
	LoadSite {
		database: LoadedDatabase::LeaderPersonalities,
		directory: "/leader_personalities",
		load_rva: 0x0086_a290,
		file_rva: 0x0086_a8c0,
		load_prologue: &[0x48, 0x89, 0x5c, 0x24, 0x20],
		file_prologue: &[0x48, 0x89, 0x5c, 0x24, 0x08],
	},
	LoadSite {
		database: LoadedDatabase::GovernmentReforms,
		directory: "/government_reforms",
		load_rva: 0x007a_7520,
		file_rva: 0x007a_7e60,
		load_prologue: &[0x40, 0x55, 0x53, 0x56],
		file_prologue: &[0x48, 0x89, 0x5c, 0x24, 0x08],
	},
];

/// A function in this `eu4.exe`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameFn {
	pub name: &'static str,
	pub rva: u32,
	pub prologue: &'static [u8],
}

/// `AddEffectiveWarScore`. `rcx` is the war, `rdx` is the reason string, `r8` points at the score delta.
pub const ADD_EFFECTIVE_WAR_SCORE: GameFn = GameFn {
	name: "AddEffectiveWarScore",
	rva: 0x003d_8280,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x08],
};

/// Returns the clamped war score in `eax`. The caller logs `Warscore in %s vs. %s is %i`.
/// Battle and ticking lines are formatted here; occupation goes through [`OCCUPATION_WARSCORE`].
pub const WAR_SCORE_TOTAL: GameFn = GameFn {
	name: "Warscore in %s vs. %s is %i",
	rva: 0x00d9_1d70,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x20, 0x4c, 0x89, 0x44],
};

/// One side's score. `eax` is the accumulated total divided by 1000.
/// Province costs come from [`PROVINCE_WARSCORE_COST`].
pub const WAR_SCORE_SIDE: GameFn = GameFn {
	name: "war score side",
	rva: 0x00d9_01f0,
	prologue: &[0x48, 0x8b, 0xc4, 0x48, 0x89, 0x48, 0x08, 0x53],
};

/// Per-province warscore cost. Reads the defines `AE_PROVINCE_CAP`,
/// `PEACE_COST_DEMAND_CAPITAL_MULT`, `GREAT_PROJECT_WARSCORE_COST_MODIFIER`,
/// and `AUTONOMY_WARSCORE_COST_MODIFIER`; the `WARSCORE_COST_*` spellings in
/// its tooltip are localization keys, not defines.
pub const PROVINCE_WARSCORE_COST: GameFn = GameFn {
	name: "WARSCORE_COST_BASE",
	rva: 0x00a1_9330,
	prologue: &[0x44, 0x88, 0x44, 0x24, 0x18, 0x55, 0x53, 0x56],
};

/// Occupation and blockade breakdown. The log keys are `WARSCORE_OCCUPATION` and `BLOCKADED_PEACE`.
pub const OCCUPATION_WARSCORE: GameFn = GameFn {
	name: "WARSCORE_OCCUPATION",
	rva: 0x00d9_0580,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x20, 0x48, 0x89, 0x54],
};

/// Applies a land battle. The log is `SetupNonSiegeResults`. Losses are read by [`UNIT_LOSSES`].
pub const BATTLE_RESULTS: GameFn = GameFn {
	name: "SetupNonSiegeResults",
	rva: 0x0023_7d50,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x10, 0x55, 0x56, 0x57],
};

/// Loss count added into the battle result.
pub const UNIT_LOSSES: GameFn = GameFn {
	name: "battle losses",
	rva: 0x0022_09c0,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x20, 0x4c, 0x89, 0x44],
};

/// Decides the combat is a siege. The log is `Siege it is`.
pub const SIEGE_COMBAT: GameFn = GameFn {
	name: "Siege it is",
	rva: 0x0024_7ac0,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x10, 0x48, 0x89, 0x4c],
};

/// Writes current manpower. The log is `SetManpower, %s, %d`. The value is stored at
/// [`foch_eu4_state::candidates::country::MANPOWER`].
pub const SET_MANPOWER: GameFn = GameFn {
	name: "SetManpower",
	rva: 0x0034_ce80,
	prologue: &[
		0x48, 0x89, 0x5c, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20, 0x57, 0x48, 0x83, 0xec, 0x30,
		0x48, 0x8b, 0xf2,
	],
};

/// Monthly manpower recovery up to [`foch_eu4_state::candidates::country::MANPOWER_MAX`].
/// The log is `CalcManpowerIncrease, %s, %d`.
pub const CALC_MANPOWER_INCREASE: GameFn = GameFn {
	name: "CalcManpowerIncrease",
	rva: 0x0032_a2a0,
	prologue: &[
		0x48, 0x89, 0x5c, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20, 0x57, 0x48, 0x83, 0xec, 0x30,
		0x48, 0x8b, 0xf1,
	],
};

/// Writes the treasury. The log is `SetTreasury, %s, %d`. The value is the
/// country view's treasury ([`foch_runtime::dataplane::CountryView::treasury`]).
pub const SET_TREASURY: GameFn = GameFn {
	name: "SetTreasury",
	rva: 0x0033_84c0,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x18, 0x48, 0x89, 0x74],
};

/// Assigns one leader to one army. `rcx` is the army, `rdx` is the leader or null.
/// Field offsets live in [`foch_eu4_state::candidates::army`].
pub const ASSIGN_LEADER: GameFn = GameFn {
	name: "Setting leader",
	rva: 0x00d4_05b0,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x18],
};

/// Army id dword at [`foch_eu4_state::candidates::army::ID`].
pub fn army_id(object: &[u8]) -> Option<i32> {
	let start = foch_eu4_state::candidates::army::ID;
	let bytes = object.get(start..start.checked_add(4)?)?;
	Some(i32::from_le_bytes(bytes.try_into().ok()?))
}

/// Merges `/wargoal_types` into the wargoal database. The object's name is at [`WARGOAL_ENTRY_NAME`].
pub const WARGOAL_LOAD: GameFn = GameFn {
	name: "/wargoal_types",
	rva: 0x00da_4950,
	prologue: &[0x48, 0x89, 0x5c, 0x24, 0x08],
};

/// Applies `common/defines.lua` into the defines object. The merged numbers are that object when this returns.
pub const DEFINES_LOAD: GameFn = GameFn {
	name: "defines",
	rva: 0x0048_c940,
	prologue: &[0x88, 0x54, 0x24, 0x10],
};

/// Builds one database object and, when the name does not fit in the inline
/// buffer, its heap bytes. The offline trainer assembles its merged objects
/// with this; the game builds the same bytes with its loader.
#[derive(Default)]
pub struct EntryHeap {
	entries: Vec<(u64, Vec<u8>)>,
}

impl EntryHeap {
	pub fn store(&mut self, offset: u32, name: &str) -> Vec<u8> {
		let start = offset as usize;
		let mut object = vec![0u8; start + 0x20];
		let size = name.len() as u64;
		if name.len() < 16 {
			object[start..start + name.len()].copy_from_slice(name.as_bytes());
			object[start + 0x10..start + 0x18].copy_from_slice(&size.to_le_bytes());
			object[start + 0x18..start + 0x20].copy_from_slice(&15u64.to_le_bytes());
			return object;
		}
		let pointer = self.entries.len() as u64 + 1;
		object[start..start + 8].copy_from_slice(&pointer.to_le_bytes());
		object[start + 0x10..start + 0x18].copy_from_slice(&size.to_le_bytes());
		object[start + 0x18..start + 0x20].copy_from_slice(&size.to_le_bytes());
		self.entries.push((pointer, name.as_bytes().to_vec()));
		object
	}

	pub fn read(&self, pointer: u64, size: usize) -> Option<Vec<u8>> {
		let bytes = self
			.entries
			.iter()
			.find(|(id, _)| *id == pointer)?
			.1
			.clone();
		(bytes.len() == size).then_some(bytes)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use std::path::Path;

	#[test]
	fn four_databases_have_distinct_file_hooks() {
		assert_eq!(SITES.len(), 4);
		let files: Vec<u32> = SITES.iter().map(|site| site.file_rva).collect();
		assert_eq!(
			files
				.iter()
				.collect::<std::collections::BTreeSet<_>>()
				.len(),
			4
		);
	}

	#[test]
	fn an_object_name_is_the_string_at_the_entry_offset() {
		let mut heap = EntryHeap::default();
		let inline = heap.store(0x10, "take_province");
		assert_eq!(
			read_entry_name(&inline, 0x10, |pointer, size| heap.read(pointer, size)).as_deref(),
			Some("take_province")
		);
		let spilled = heap.store(0x158, "glory_seeker_personality");
		assert_eq!(
			read_entry_name(&spilled, 0x158, |pointer, size| heap.read(pointer, size)).as_deref(),
			Some("glory_seeker_personality")
		);
		assert_eq!(
			read_entry_name(&spilled, 0x10, |pointer, size| heap.read(pointer, size)),
			None
		);
		let mut army = vec![0u8; 0x28];
		army[0x24..0x28].copy_from_slice(&7i32.to_le_bytes());
		assert_eq!(army_id(&army), Some(7));
	}

	#[test]
	fn the_installed_exe_still_starts_with_these_bytes() {
		let Ok(root) = std::env::var("EU4_ROOT") else {
			return;
		};
		let bytes = fs::read(Path::new(&root).join("eu4.exe")).expect("eu4.exe");
		let digest = sha256(&bytes);
		assert_eq!(
			digest,
			foch_runtime::dataplane::pinned_exe_sha256(),
			"eu4.exe hash changed; RVAs are stale"
		);
		for site in SITES {
			let load = rva_bytes(&bytes, site.load_rva, site.load_prologue.len());
			let file = rva_bytes(&bytes, site.file_rva, site.file_prologue.len());
			assert_eq!(load, site.load_prologue, "{}", site.directory);
			assert_eq!(file, site.file_prologue, "{}", site.directory);
		}
		let war = rva_bytes(
			&bytes,
			ADD_EFFECTIVE_WAR_SCORE.rva,
			ADD_EFFECTIVE_WAR_SCORE.prologue.len(),
		);
		assert_eq!(war, ADD_EFFECTIVE_WAR_SCORE.prologue);
		let assign = rva_bytes(&bytes, ASSIGN_LEADER.rva, ASSIGN_LEADER.prologue.len());
		assert_eq!(assign, ASSIGN_LEADER.prologue);
		for function in [
			WARGOAL_LOAD,
			DEFINES_LOAD,
			WAR_SCORE_TOTAL,
			WAR_SCORE_SIDE,
			PROVINCE_WARSCORE_COST,
			OCCUPATION_WARSCORE,
			BATTLE_RESULTS,
			UNIT_LOSSES,
			SIEGE_COMBAT,
			SET_MANPOWER,
			CALC_MANPOWER_INCREASE,
			SET_TREASURY,
		] {
			let prologue = rva_bytes(&bytes, function.rva, function.prologue.len());
			assert_eq!(prologue, function.prologue, "{}", function.name);
		}
	}

	fn rva_bytes(exe: &[u8], rva: u32, len: usize) -> Vec<u8> {
		let offset = file_offset(exe, rva).unwrap_or_else(|| panic!("rva {rva:#x}"));
		exe[offset..offset + len].to_vec()
	}

	fn file_offset(exe: &[u8], rva: u32) -> Option<usize> {
		let e_lfanew = u32::from_le_bytes(exe[0x3c..0x40].try_into().ok()?) as usize;
		let size_opt =
			u16::from_le_bytes(exe[e_lfanew + 20..e_lfanew + 22].try_into().ok()?) as usize;
		let count = u16::from_le_bytes(exe[e_lfanew + 6..e_lfanew + 8].try_into().ok()?) as usize;
		let mut at = e_lfanew + 24 + size_opt;
		for _ in 0..count {
			let va = u32::from_le_bytes(exe[at + 12..at + 16].try_into().ok()?);
			let raw_size = u32::from_le_bytes(exe[at + 16..at + 20].try_into().ok()?);
			let raw = u32::from_le_bytes(exe[at + 20..at + 24].try_into().ok()?);
			let virtual_size = u32::from_le_bytes(exe[at + 8..at + 12].try_into().ok()?);
			let span = raw_size.max(virtual_size);
			if (va..va + span).contains(&rva) {
				return Some((raw + (rva - va)) as usize);
			}
			at += 40;
		}
		None
	}

	fn sha256(bytes: &[u8]) -> String {
		use sha2::{Digest, Sha256};
		hex_encode(Sha256::digest(bytes).as_slice())
	}

	fn hex_encode(bytes: &[u8]) -> String {
		const HEX: &[u8; 16] = b"0123456789abcdef";
		let mut out = String::with_capacity(bytes.len() * 2);
		for byte in bytes {
			out.push(HEX[(byte >> 4) as usize] as char);
			out.push(HEX[(byte & 0xf) as usize] as char);
		}
		out
	}
}
