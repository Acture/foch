//! Bounds-checked PE32+ headers used by the one-shot entry gate.

#[derive(Debug)]
pub(crate) struct Image {
	pub entry: usize,
	pub size: usize,
}

pub(crate) const GATE_BYTES: usize = 14;

pub(crate) fn gate_fits_page(address: usize, page_size: usize) -> bool {
	page_size >= GATE_BYTES && address % page_size <= page_size - GATE_BYTES
}

pub(crate) fn inspect(headers: &[u8]) -> Result<Image, &'static str> {
	let word = |at: usize| -> Result<u16, &'static str> {
		let bytes = headers.get(at..at + 2).ok_or("truncated PE header")?;
		Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
	};
	let dword = |at: usize| -> Result<usize, &'static str> {
		let bytes = headers.get(at..at + 4).ok_or("truncated PE header")?;
		Ok(u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
	};
	if headers.get(..2) != Some(b"MZ") {
		return Err("missing DOS header");
	}
	let nt = dword(60)?;
	if nt > headers.len().saturating_sub(24) || headers.get(nt..nt + 4) != Some(b"PE\0\0") {
		return Err("missing NT header");
	}
	if word(nt + 4)? != 0x8664 || word(nt + 24)? != 0x20b {
		return Err("host requires an AMD64 PE32+ image");
	}
	let count = word(nt + 6)? as usize;
	let optional_size = word(nt + 20)? as usize;
	if optional_size < 112 || count == 0 || count > 96 {
		return Err("invalid optional header or section count");
	}
	let size = dword(nt + 24 + 56)?;
	let entry = dword(nt + 24 + 16)?;
	let sections = nt + 24 + optional_size;
	if headers.get(sections..sections + count * 40).is_none() {
		return Err("truncated section table");
	}
	let gate_end = entry.checked_add(GATE_BYTES).ok_or("entry overflow")?;
	let mut executable_entry = false;
	for i in 0..count {
		let at = sections + i * 40;
		let len = dword(at + 8)?;
		let start = dword(at + 12)?;
		let end = start.checked_add(len).ok_or("section overflow")?;
		if end > size {
			return Err("section outside image");
		}
		if dword(at + 36)? & 0x20000000 != 0 && entry >= start && gate_end <= end {
			executable_entry = true;
		}
	}
	if entry == 0 || !executable_entry {
		return Err("entry gate is outside executable section bounds");
	}
	Ok(Image { entry, size })
}

#[cfg(test)]
mod tests {
	use super::*;

	fn headers() -> Vec<u8> {
		let mut b = vec![0; 512];
		b[..2].copy_from_slice(b"MZ");
		b[60..64].copy_from_slice(&64u32.to_le_bytes());
		b[64..68].copy_from_slice(b"PE\0\0");
		b[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
		b[70..72].copy_from_slice(&1u16.to_le_bytes());
		b[84..86].copy_from_slice(&240u16.to_le_bytes());
		b[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
		b[104..108].copy_from_slice(&0x1000u32.to_le_bytes());
		b[144..148].copy_from_slice(&0x3000u32.to_le_bytes());
		b[336..340].copy_from_slice(&0x100u32.to_le_bytes());
		b[340..344].copy_from_slice(&0x1000u32.to_le_bytes());
		b[364..368].copy_from_slice(&0x60000020u32.to_le_bytes());
		b
	}

	#[test]
	fn accepts_only_an_entry_with_space_inside_executable_code() {
		let mut b = headers();
		let image = inspect(&b).unwrap();
		assert_eq!(image.entry, 0x1000);
		assert_eq!(image.size, 0x3000);
		b[104..108].copy_from_slice(&0x10fbu32.to_le_bytes());
		assert!(inspect(&b).is_err(), "gate crosses section boundary");
	}

	#[test]
	fn refuses_truncated_wrong_arch_and_non_executable_images() {
		for len in [0, 63, 90, 150, 365] {
			assert!(inspect(&headers()[..len]).is_err());
		}
		let mut b = headers();
		b[68..70].copy_from_slice(&0x14cu16.to_le_bytes());
		assert!(inspect(&b).is_err());
		let mut b = headers();
		b[364..368].copy_from_slice(&0x40000040u32.to_le_bytes());
		assert!(inspect(&b).is_err());
		let mut b = headers();
		b[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(inspect(&b).is_err());
	}

	#[test]
	fn gate_must_fit_in_one_page_to_preserve_page_protection() {
		assert!(gate_fits_page(0x1000, 4096));
		assert!(gate_fits_page(0x2000 - GATE_BYTES, 4096));
		assert!(!gate_fits_page(0x2000 - GATE_BYTES + 1, 4096));
		assert!(!gate_fits_page(0x1fff, 4096));
		assert!(!gate_fits_page(0x1000, 0));
	}
}
