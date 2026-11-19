//! x64 instruction lengths for inline plugin hooks.
//!
//! Select the first complete window of 14 through 28 bytes. RIP-relative
//! operands are allowed as long as the caller relocates their disp32 in the
//! trampoline (see [`rip_operands`]); relative control flow, terminators,
//! unknown opcodes and truncated instructions are refused. The executable
//! entry gate has its own fixed window and does not use this decoder.

pub(crate) const MIN_STEAL: usize = 14;
pub(crate) const MAX_STEAL: usize = 28;

struct Insn {
	len: usize,
	rip_relative: bool,
	relative: bool,
	terminator: bool,
	/// Byte offset of the disp32 inside this instruction. Only meaningful
	/// when `rip_relative`; zero otherwise.
	rip_disp: usize,
}

pub(crate) fn steal_len(bytes: &[u8]) -> Option<usize> {
	let bytes = &bytes[..bytes.len().min(MAX_STEAL)];
	let mut at = 0;
	while at < MIN_STEAL {
		let insn = decode(bytes.get(at..)?)?;
		if insn.len == 0 || insn.relative || insn.terminator {
			return None;
		}
		at = at.checked_add(insn.len)?;
	}
	Some(at)
}

/// A RIP-relative disp32 inside a stolen window.
pub(crate) struct RipOperand {
	/// Byte offset of the disp32 within the window.
	pub disp_at: usize,
	/// Byte offset just past the instruction holding it: the RIP the disp
	/// is relative to, as a window offset.
	pub next: usize,
}

/// Every RIP-relative operand in the first `len` bytes of `bytes`, in order.
/// The window must be the one [`steal_len`] accepted; a decode failure or a
/// RIP operand past `len` yields `None`.
pub(crate) fn rip_operands(bytes: &[u8], len: usize) -> Option<Vec<RipOperand>> {
	let mut out = Vec::new();
	let mut at = 0;
	while at < len {
		let insn = decode(bytes.get(at..)?)?;
		if insn.len == 0 || at + insn.len > len {
			return None;
		}
		if insn.rip_relative {
			out.push(RipOperand {
				disp_at: at + insn.rip_disp,
				next: at + insn.len,
			});
		}
		at += insn.len;
	}
	if at != len { None } else { Some(out) }
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
	// Mixed 66/REX.W forms need opcode-specific width rules. This decoder
	// conservatively refuses them instead of splitting an immediate.
	if operand16 && rex_w {
		return None;
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
			rip_disp: 0,
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
		0x81 => with_modrm(code, i, iz(operand16), false),
		0xC7 => {
			let operand = modrm(code, i)?;
			// Only /0 is MOV. In particular, C7 F8 is relative XBEGIN.
			if operand.reg != 0 {
				return None;
			}
			finish(code, operand, iz(operand16), false, false)
		}
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
			rip_disp: 0,
		}),
		0x0B => Some(Insn {
			len: i,
			rip_relative: false,
			relative: false,
			terminator: true,
			rip_disp: 0,
		}),
		0x80..=0x8F => Some(Insn {
			len: take(code, i, 4)?,
			rip_relative: false,
			relative: true,
			terminator: false,
			rip_disp: 0,
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
	/// Offset of the disp32 within the instruction; only set when
	/// `rip_relative` (it directly follows the modrm byte).
	disp: usize,
	reg: u8,
}

fn modrm(code: &[u8], i: usize) -> Option<Modrm> {
	let byte = *code.get(i)?;
	let mut next = i + 1;
	let mode = byte >> 6;
	let reg = (byte >> 3) & 7;
	let rm = byte & 7;
	let mut rip_relative = false;
	let mut disp = 0;
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
			disp = next;
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
		disp,
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
		rip_disp: if modrm.rip_relative { modrm.disp } else { 0 },
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
	fn chooses_the_first_complete_relocatable_boundary() {
		assert_eq!(steal_len(&[0x90; 14]), Some(14));
		assert_eq!(
			steal_len(&[
				0x55, 0x48, 0x89, 0xE5, 0x48, 0x83, 0xEC, 0x20, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90
			]),
			Some(14)
		);
		let mut mov = [0x90; 14];
		mov[..10].copy_from_slice(&[0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]);
		mov[10..14].copy_from_slice(&[0x48, 0x83, 0xEC, 0x20]);
		assert_eq!(steal_len(&mov), Some(14));
		assert_eq!(
			steal_len(&[
				0xC3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90
			]),
			None
		);
		let mut rip = [0x90; 14];
		rip[..7].copy_from_slice(&[0x48, 0x8B, 0x05, 0, 0, 0, 0]);
		assert_eq!(steal_len(&rip), Some(14));
		let mut call = [0x90; 14];
		call[..5].copy_from_slice(&[0xE8, 0, 0, 0, 0]);
		assert_eq!(steal_len(&call), None);
		// Pinned EU4 1.37.5 cost rebuild at 0x978b20: three home-slot saves.
		let rebuild = [
			0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x6c, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24,
			0x18,
		];
		assert_eq!(steal_len(&rebuild), Some(15));
		assert!(rip_operands(&rebuild, 15).unwrap().is_empty());
		let mut split = [0x90; 16];
		split[..10].copy_from_slice(&[0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]);
		split[10..15].copy_from_slice(&[0xB8, 1, 0, 0, 0]);
		assert_eq!(steal_len(&split), Some(15));
		assert_eq!(steal_len(&[0x90; 13]), None);
	}

	#[test]
	fn rip_operand_positions_are_reported_for_relocation() {
		let mut bytes = [0x90u8; 15];
		// mov r14,[r13]; mov eax,[rip+8]; add [r14+0x18],eax
		bytes[..3].copy_from_slice(&[0x4D, 0x8B, 0x26]);
		bytes[3..9].copy_from_slice(&[0x8B, 0x05, 0x08, 0x00, 0x00, 0x00]);
		bytes[9..13].copy_from_slice(&[0x41, 0x01, 0x46, 0x18]);
		assert_eq!(steal_len(&bytes), Some(14));
		let operands = rip_operands(&bytes, 14).unwrap();
		assert_eq!(operands.len(), 1);
		assert_eq!(operands[0].disp_at, 5);
		assert_eq!(operands[0].next, 9);
		// A jmp [rip+0] stays a terminator even though it is RIP-relative.
		let mut jump = [0x90u8; 14];
		jump[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
		assert_eq!(steal_len(&jump), None);
	}

	#[test]
	fn complete_15_and_17_byte_entries_are_relocatable() {
		let heads: &[&[u8]] = &[
			// State entry: its stack adjustment ends at byte 17.
			&[
				0x40, 0x53, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48,
				0x83, 0xEC, 0x28,
			],
			// Active-country loop: the second load ends at byte 17.
			&[
				0x44, 0x8B, 0xE7, 0x48, 0x8B, 0x8D, 0xE8, 0x1C, 0, 0, 0x48, 0x8B, 0x85, 0xF0, 0x1C,
				0, 0,
			],
			// Monthly loop: the second movzx ends at byte 15.
			&[
				0x49, 0x8B, 0x87, 0xE8, 0x1C, 0, 0, 0x45, 0x0F, 0xB6, 0xC4, 0x41, 0x0F, 0xB6, 0xD5,
			],
			// Daily queue: its register-based SIB load fits, its store ends at 15.
			&[
				0x48, 0x8B, 0x50, 0x08, 0x4C, 0x8B, 0x3C, 0xCA, 0x4C, 0x89, 0xBD, 0xA8, 0, 0, 0,
			],
		];
		for (head, expected) in heads.iter().zip([17, 17, 15, 15]) {
			assert_eq!(
				steal_len(head),
				Some(expected),
				"complete relocatable instruction window refused: {head:02x?}"
			);
		}
	}
	#[test]
	fn rejects_control_flow_unknown_truncation_and_overlong_windows() {
		for bad in [
			vec![0xE8, 0, 0, 0, 0],
			vec![0xE9, 0, 0, 0, 0],
			vec![0xC3],
			vec![0x0F, 0x0B],
			vec![0xC5, 0xF8, 0x77],
			vec![0xFF, 0x25, 0, 0, 0, 0],
		] {
			let mut bytes = bad;
			bytes.resize(28, 0x90);
			assert_eq!(steal_len(&bytes), None);
		}
		let mut truncated = vec![0x90; 13];
		truncated.extend([0x48, 0xB8, 0]);
		assert_eq!(steal_len(&truncated), None);
		let mut longest = vec![0x90; 13];
		longest.extend([0x2E; 5]);
		longest.extend([0x48, 0xB8, 0, 0, 0, 0, 0, 0, 0, 0]);
		assert_eq!(steal_len(&longest), Some(28));
		longest.insert(13, 0x2E);
		assert_eq!(steal_len(&longest), None);
	}

	#[test]
	fn decoder_rejects_mixed_operand_rex_w_immediates() {
		// This valid mov rax,7 is eight bytes, not six. Treat this prefix
		// combination as unsupported rather than split its immediate.
		let mut bytes = vec![0x90; 11];
		bytes.extend([0x66, 0x48, 0xC7, 0xC0, 7, 0, 0, 0, 0xC3]);
		assert_eq!(steal_len(&bytes), None);
	}

	#[test]
	fn decoder_rejects_transaction_relative_control_flow() {
		for instruction in [
			&[0xC7, 0xF8, 0x10, 0, 0, 0][..],
			&[0x66, 0xC7, 0xF8, 0x10, 0][..],
		] {
			let mut bytes = vec![0x90; 9];
			bytes.extend(instruction);
			assert_eq!(steal_len(&bytes), None);
		}
	}
}
