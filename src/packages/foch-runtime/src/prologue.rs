//! x64 instruction lengths for a 14-byte hook prologue.
//!
//! A steal is accepted only when an instruction boundary lands on byte 14 and
//! every instruction in that window can be copied unchanged. RIP-relative
//! operands and relative branches need a displacement fixup, so they are
//! refused. A return or unconditional jump means the function is shorter than
//! the jump. Unknown opcodes are refused rather than split.

const WINDOW: usize = 14;

struct Insn {
	len: usize,
	rip_relative: bool,
	relative: bool,
	terminator: bool,
}

pub(crate) fn can_steal(bytes: &[u8]) -> bool {
	if bytes.len() < WINDOW {
		return false;
	}
	let bytes = &bytes[..WINDOW];
	let mut at = 0;
	while at < WINDOW {
		let Some(insn) = decode(&bytes[at..]) else {
			return false;
		};
		if insn.len == 0 || insn.rip_relative || insn.relative || insn.terminator {
			return false;
		}
		at += insn.len;
		if at > WINDOW {
			return false;
		}
	}
	true
}

fn decode(code: &[u8]) -> Option<Insn> {
	let mut i = 0;
	let mut operand16 = false;
	let mut rex_w = false;
	let mut saw_rex = false;
	while i < code.len() && i < 15 {
		match code[i] {
			0xF0 | 0xF2 | 0xF3 | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => {
				if saw_rex {
					return None;
				}
				i += 1;
			}
			0x66 => {
				if saw_rex {
					return None;
				}
				operand16 = true;
				i += 1;
			}
			0x67 => return None,
			0x40..=0x4F => {
				if saw_rex {
					return None;
				}
				saw_rex = true;
				rex_w = code[i] & 0x08 != 0;
				i += 1;
			}
			_ => break,
		}
	}
	if i >= code.len() {
		return None;
	}
	let op = code[i];
	i += 1;
	let insn = if op == 0x0F {
		decode_0f(code, i)?
	} else {
		decode_primary(op, rex_w, operand16, code, i)?
	};
	if insn.len > 15 { None } else { Some(insn) }
}

fn decode_primary(op: u8, rex_w: bool, operand16: bool, code: &[u8], i: usize) -> Option<Insn> {
	let end = |extra| -> Option<usize> { take(code, i, extra) };
	let plain = |extra, relative, terminator| -> Option<Insn> {
		Some(Insn {
			len: end(extra)?,
			rip_relative: false,
			relative,
			terminator,
		})
	};
	match op {
		0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E | 0x1F | 0x27 | 0x2F | 0x37 | 0x3F | 0x60
		| 0x61 | 0x62 | 0x82 | 0x9A | 0xC4 | 0xC5 | 0xCE | 0xD4 | 0xD5 | 0xD6 | 0xEA => None,
		0x50..=0x5F
		| 0x6C..=0x6F
		| 0x90..=0x9F
		| 0xA4..=0xA7
		| 0xAA..=0xAF
		| 0xC9
		| 0xCC
		| 0xD7
		| 0xEC..=0xEF
		| 0xF1
		| 0xF5
		| 0xF8..=0xFD => plain(0, false, false),
		0xC3 | 0xCB | 0xCF | 0xF4 => plain(0, false, true),
		0x04
		| 0x0C
		| 0x14
		| 0x1C
		| 0x24
		| 0x2C
		| 0x34
		| 0x3C
		| 0x6A
		| 0xA8
		| 0xB0..=0xB7
		| 0xCD
		| 0xE4..=0xE7 => plain(1, false, false),
		0x70..=0x7F | 0xE0..=0xE3 => plain(1, true, false),
		0xEB => plain(1, true, true),
		0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D | 0x68 | 0xA9 => {
			plain(iz(operand16), false, false)
		}
		0xE8 => plain(4, true, false),
		0xE9 => plain(4, true, true),
		0xC2 | 0xCA => plain(2, false, true),
		0xC8 => plain(3, false, false),
		0xA0..=0xA3 => plain(8, false, false),
		0xB8..=0xBF => plain(if rex_w { 8 } else { iz(operand16) }, false, false),
		0x00..=0x03
		| 0x08..=0x0B
		| 0x10..=0x13
		| 0x18..=0x1B
		| 0x20..=0x23
		| 0x28..=0x2B
		| 0x30..=0x33
		| 0x38..=0x3B
		| 0x63
		| 0x84..=0x8F
		| 0xD0..=0xD3
		| 0xD8..=0xDF
		| 0xFE => with_modrm(code, i, 0, false),
		0x69 => with_modrm(code, i, iz(operand16), false),
		0x6B | 0x80 | 0x83 | 0xC0 | 0xC1 | 0xC6 => with_modrm(code, i, 1, false),
		0x81 | 0xC7 => with_modrm(code, i, iz(operand16), false),
		0xF6 => {
			let modrm = modrm(code, i)?;
			let extra = if modrm.reg <= 1 { 1 } else { 0 };
			finish(code, modrm, extra, false, false)
		}
		0xF7 => {
			let modrm = modrm(code, i)?;
			let extra = if modrm.reg <= 1 { iz(operand16) } else { 0 };
			finish(code, modrm, extra, false, false)
		}
		0xFF => {
			let modrm = modrm(code, i)?;
			let terminator = modrm.reg == 4 || modrm.reg == 5;
			finish(code, modrm, 0, false, terminator)
		}
		_ => None,
	}
}

fn decode_0f(code: &[u8], i: usize) -> Option<Insn> {
	let op = *code.get(i)?;
	let i = i + 1;
	match op {
		0x05 | 0x31 | 0x77 | 0xA0 | 0xA1 | 0xA2 | 0xA8 | 0xA9 | 0xC8..=0xCF => Some(Insn {
			len: i,
			rip_relative: false,
			relative: false,
			terminator: false,
		}),
		0x0B => Some(Insn {
			len: i,
			rip_relative: false,
			relative: false,
			terminator: true,
		}),
		0x80..=0x8F => Some(Insn {
			len: take(code, i, 4)?,
			rip_relative: false,
			relative: true,
			terminator: false,
		}),
		0x00..=0x03
		| 0x10..=0x1F
		| 0x20..=0x23
		| 0x28..=0x2F
		| 0x40..=0x4F
		| 0x90..=0x9F
		| 0xA3
		| 0xA5
		| 0xAB
		| 0xAD
		| 0xAE
		| 0xAF
		| 0xB0..=0xB3
		| 0xB6
		| 0xB7
		| 0xBB..=0xBF
		| 0xC0
		| 0xC1
		| 0xC7 => with_modrm(code, i, 0, false),
		0xA4 | 0xAC | 0xBA | 0xC2 | 0xC4..=0xC6 | 0x70..=0x73 => with_modrm(code, i, 1, false),
		_ => None,
	}
}

#[derive(Clone, Copy)]
struct Modrm {
	next: usize,
	rip_relative: bool,
	reg: u8,
}

fn modrm(code: &[u8], i: usize) -> Option<Modrm> {
	let byte = *code.get(i)?;
	let mut next = i + 1;
	let mode = byte >> 6;
	let reg = (byte >> 3) & 7;
	let rm = byte & 7;
	let mut rip_relative = false;
	if mode != 3 {
		if rm == 4 {
			let sib = *code.get(next)?;
			next += 1;
			if mode == 0 && sib & 7 == 5 {
				next += 4;
			} else if mode == 1 {
				next += 1;
			} else if mode == 2 {
				next += 4;
			}
		} else if mode == 0 && rm == 5 {
			next += 4;
			rip_relative = true;
		} else if mode == 1 {
			next += 1;
		} else if mode == 2 {
			next += 4;
		}
	}
	if next > code.len() {
		return None;
	}
	Some(Modrm {
		next,
		rip_relative,
		reg,
	})
}

fn with_modrm(code: &[u8], i: usize, extra: usize, terminator: bool) -> Option<Insn> {
	finish(code, modrm(code, i)?, extra, false, terminator)
}

fn finish(
	code: &[u8],
	modrm: Modrm,
	extra: usize,
	relative: bool,
	terminator: bool,
) -> Option<Insn> {
	Some(Insn {
		len: take(code, modrm.next, extra)?,
		rip_relative: modrm.rip_relative,
		relative,
		terminator,
	})
}

fn take(code: &[u8], i: usize, extra: usize) -> Option<usize> {
	let end = i.checked_add(extra)?;
	(end <= code.len()).then_some(end)
}

fn iz(operand16: bool) -> usize {
	if operand16 { 2 } else { 4 }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decoded(bytes: &[u8]) -> Insn {
		decode(bytes).unwrap()
	}

	#[test]
	fn lengths_of_prologue_encodings() {
		assert_eq!(decoded(&[0x90]).len, 1);
		assert_eq!(decoded(&[0x55]).len, 1);
		assert_eq!(decoded(&[0x48, 0x89, 0xE5]).len, 3);
		assert_eq!(decoded(&[0x48, 0x83, 0xEC, 0x20]).len, 4);
		assert_eq!(decoded(&[0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]).len, 10);
		assert_eq!(decoded(&[0x0F, 0x1F, 0x44, 0x00, 0x00]).len, 5);
		assert_eq!(decoded(&[0x66, 0x0F, 0x1F, 0x84, 0x00, 0, 0, 0, 0]).len, 9);
		let rip = decoded(&[0x48, 0x8B, 0x05, 0, 0, 0, 0]);
		assert_eq!(rip.len, 7);
		assert!(rip.rip_relative);
		let jump = decoded(&[0xFF, 0x25, 0, 0, 0, 0]);
		assert_eq!(jump.len, 6);
		assert!(jump.rip_relative && jump.terminator);
		assert!(decoded(&[0xC3]).terminator);
		assert!(decoded(&[0xEB, 0x00]).relative && decoded(&[0xEB, 0x00]).terminator);
		assert!(decoded(&[0xE8, 0, 0, 0, 0]).relative);
		assert!(decoded(&[0x74, 0x00]).relative);
	}

	#[test]
	fn steals_only_a_relocatable_boundary_at_14() {
		assert!(can_steal(&[0x90; 14]));
		assert!(can_steal(&[
			0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x20, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90
		]));
		let mut mov = [0x90; 14];
		mov[..10].copy_from_slice(&[0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]);
		mov[10..14].copy_from_slice(&[0x48, 0x83, 0xEC, 0x20]);
		assert!(can_steal(&mov));
		assert!(!can_steal(&[
			0xC3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90
		]));
		let mut rip = [0x90; 14];
		rip[..7].copy_from_slice(&[0x48, 0x8B, 0x05, 0, 0, 0, 0]);
		assert!(!can_steal(&rip));
		let mut call = [0x90; 14];
		call[..5].copy_from_slice(&[0xE8, 0, 0, 0, 0]);
		assert!(!can_steal(&call));
		let mut split = [0x90; 16];
		split[..10].copy_from_slice(&[0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]);
		split[10..15].copy_from_slice(&[0xB8, 1, 0, 0, 0]);
		assert!(!can_steal(&split));
		assert!(!can_steal(&[0x90; 13]));
	}
}
