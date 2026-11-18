//! Exact-build boundary for the native peace editor.
use foch_plugin_sdk::FochHostApi;

pub const EXE_SHA256: &str = "c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e";

#[derive(Clone, Copy)]
pub struct CodeSite {
	pub rva: usize,
	pub original: &'static [u8],
}

impl CodeSite {
	pub fn matches(self, image: &[u8]) -> bool {
		image.get(self.rva..self.rva.saturating_add(self.original.len())) == Some(self.original)
	}
}

pub const SUMMARY_UPDATE: CodeSite = CodeSite {
	rva: 0x814d23,
	original: &[0xe8, 0x98, 0x58, 0xaf, 0x00],
};
pub const SUMMARY_SUGGEST: CodeSite = CodeSite {
	rva: 0x130fb84,
	original: &[0xe8, 0x37, 0xaa, 0xff, 0xff],
};
pub const SUMMARY_SURRENDER: CodeSite = CodeSite {
	rva: 0x13112d7,
	original: &[0xe8, 0xe4, 0x92, 0xff, 0xff],
};
pub const SEND_CALLBACK: CodeSite = CodeSite {
	rva: 0x13096b6,
	original: &[0x48, 0x8d, 0x05, 0xf3, 0x5a, 0x00, 0x00],
};
pub const ACTION_CONSTRUCTOR_CALL: CodeSite = CodeSite {
	rva: 0x130f22f,
	original: &[0xe8, 0xbc, 0x98, 0x28, 0xff],
};
pub const EXECUTE_CALL: CodeSite = CodeSite {
	rva: 0x59a6ea,
	original: &[0xe8, 0x41, 0xc0, 0x3d, 0x00],
};
pub const SUMMARY: CodeSite = CodeSite {
	rva: 0x130a5c0,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56,
		0x41,
	],
};
pub const SEND: CodeSite = CodeSite {
	rva: 0x130f1b0,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x18, 0x57, 0x48, 0x83, 0xec, 0x30,
		0x48,
	],
};
pub const SET_TEXT: CodeSite = CodeSite {
	rva: 0x152aea0,
	original: &[
		0x40, 0x53, 0x48, 0x83, 0xec, 0x40, 0x48, 0x8b, 0xd9, 0x48, 0xc7, 0x44, 0x24, 0x20, 0x00,
		0x00,
	],
};
pub const OFFER_CONSTRUCTOR: CodeSite = CodeSite {
	rva: 0x975d40,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x10, 0x48, 0x89, 0x6c, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20,
		0x48,
	],
};
pub const OFFER_COPY: CodeSite = CodeSite {
	rva: 0x975f40,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x53, 0x55, 0x56, 0x57, 0x41, 0x56, 0x48, 0x83, 0xec, 0x20,
		0x48,
	],
};
pub const OFFER_DESTROY: CodeSite = CodeSite {
	rva: 0x9761c0,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8d, 0x05, 0xe7, 0x16,
		0x2e,
	],
};
pub const CLONE_CLAUSE: CodeSite = CodeSite {
	rva: 0x9a1040,
	original: &[
		0x40, 0x53, 0x48, 0x83, 0xec, 0x30, 0x48, 0x8b, 0x01, 0x48, 0x8d, 0x54, 0x24, 0x20, 0x48,
		0x8b,
	],
};
pub const VECTOR_APPEND: CodeSite = CodeSite {
	rva: 0xcc220,
	original: &[
		0x40, 0x53, 0x55, 0x41, 0x55, 0x41, 0x57, 0x48, 0x83, 0xec, 0x28, 0x48, 0x8b, 0x41, 0x08,
		0x4c,
	],
};
pub const REBUILD_COST: CodeSite = CodeSite {
	rva: 0x978b20,
	original: &[
		0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x6c, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18,
		0x57,
	],
};
pub const ACTION_CONSTRUCTOR: CodeSite = CodeSite {
	rva: 0x598af0,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x53, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0xd9, 0xc7, 0x41,
		0x08,
	],
};
pub const EXECUTE: CodeSite = CodeSite {
	rva: 0x976730,
	original: &[
		0x48, 0x89, 0x4c, 0x24, 0x08, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56,
		0x41,
	],
};

const CHECKS: &[CodeSite] = &[
	SUMMARY_UPDATE,
	SUMMARY_SUGGEST,
	SUMMARY_SURRENDER,
	SEND_CALLBACK,
	ACTION_CONSTRUCTOR_CALL,
	EXECUTE_CALL,
	SUMMARY,
	SEND,
	SET_TEXT,
	OFFER_CONSTRUCTOR,
	OFFER_COPY,
	OFFER_DESTROY,
	CLONE_CLAUSE,
	VECTOR_APPEND,
	REBUILD_COST,
	ACTION_CONSTRUCTOR,
	EXECUTE,
];

/// # Safety
/// The host and its image/string ranges must be valid for this ABI call.
pub unsafe fn verify(host: &FochHostApi) -> Result<(), &'static str> {
	if host.abi_major != foch_plugin_sdk::ABI_MAJOR
		|| (host.size as usize) < std::mem::size_of::<FochHostApi>()
	{
		return Err("incompatible Foch plugin host");
	}
	if unsafe { host.exe_sha256.as_str() } != Some(EXE_SHA256) {
		return Err("the peace editor supports only its verified EU4 executable");
	}
	if host.exe_base.is_null() || host.exe_size == 0 {
		return Err("the game image is absent");
	}
	if host.exe_size > isize::MAX as usize
		|| (host.exe_base as usize)
			.checked_add(host.exe_size)
			.is_none()
	{
		return Err("invalid game image extent");
	}
	for site in CHECKS {
		let end = site
			.rva
			.checked_add(site.original.len())
			.ok_or("invalid code site")?;
		if end > host.exe_size {
			return Err("native peace editor code lies outside the game image");
		}
		// Read only the checked code range, not discarded PE sections or gaps.
		let bytes = unsafe {
			std::slice::from_raw_parts(
				host.exe_base.cast::<u8>().add(site.rva),
				site.original.len(),
			)
		};
		if bytes != site.original {
			return Err("native peace editor code differs from the supported profile");
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn requires_the_whole_instruction_and_its_exact_operands() {
		let site = CodeSite {
			rva: 1,
			original: &[0xe8, 1, 2, 3, 4],
		};
		assert!(site.matches(&[0, 0xe8, 1, 2, 3, 4]));
		assert!(!site.matches(&[0, 0xe8, 1, 2, 3, 5]));
		assert!(!site.matches(&[0, 0xe8, 1, 2, 3]));
		assert!(!site.matches(&[]));
	}

	#[test]
	fn overflowing_rva_fails_closed() {
		let site = CodeSite {
			rva: usize::MAX - 1,
			original: &[1, 2, 3],
		};
		assert!(!site.matches(&[0; 8]));
	}
}
