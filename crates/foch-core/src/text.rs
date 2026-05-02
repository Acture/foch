//! Paradox script text decoding helpers.
//!
//! Paradox engines historically write `.txt` (and `.csv` / `.yml`) files
//! in **windows-1252** for the bundled vanilla content, while modern mods
//! often save files as **UTF-8** (with or without BOM). Chinese
//! translation mods commonly emit **GBK** / **GB18030**; we detect those
//! before falling back to windows-1252 so the rendered output matches the
//! original characters in-game instead of producing mojibake.
//!
//! `decode_paradox_bytes` is the single funnel for all
//! Paradox-script-flavoured byte → string conversion in the workspace.
//! Use it instead of ad-hoc `String::from_utf8_lossy` /
//! `WINDOWS_1252.decode` calls so the encoding rules stay uniform.

use std::borrow::Cow;

use encoding_rs::{GB18030, WINDOWS_1252};

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Decode `bytes` as a Paradox script-flavoured text blob.
///
/// Algorithm:
/// 1. Strip a leading UTF-8 BOM and decode as strict UTF-8.
/// 2. Strip a leading UTF-16 BOM and decode via `encoding_rs`.
/// 3. Try strict UTF-8 (the modern mod default) — borrowed if successful.
/// 4. If the byte stream looks like GBK / GB18030 (plausible
///    double-byte CJK sequences without invalid combinations), decode
///    accordingly. GB18030 is a strict superset of GBK so the same
///    decoder handles both.
/// 5. Fall back to **windows-1252**, the canonical Paradox encoding.
pub fn decode_paradox_bytes(bytes: &[u8]) -> Cow<'_, str> {
	if let Some(rest) = bytes.strip_prefix(UTF8_BOM.as_slice()) {
		return Cow::Owned(String::from_utf8_lossy(rest).into_owned());
	}
	if bytes.starts_with(UTF16_LE_BOM.as_slice()) {
		let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
		return Cow::Owned(decoded.into_owned());
	}
	if bytes.starts_with(UTF16_BE_BOM.as_slice()) {
		let (decoded, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
		return Cow::Owned(decoded.into_owned());
	}
	if let Ok(text) = std::str::from_utf8(bytes) {
		return Cow::Borrowed(text);
	}
	if looks_like_gb18030(bytes) {
		let (decoded, _, had_errors) = GB18030.decode(bytes);
		if !had_errors {
			return Cow::Owned(decoded.into_owned());
		}
	}
	let (decoded, _, _) = WINDOWS_1252.decode(bytes);
	Cow::Owned(decoded.into_owned())
}

/// Heuristic: are the high-byte sequences in `bytes` overwhelmingly
/// plausible GBK / GB18030 double-byte CJK pairs? Requires every high byte
/// to start a valid double-byte (lead 0x81..=0xFE, trail 0x40..=0xFE
/// excluding 0x7F) AND at least two such pairs to be present, so a
/// windows-1252 sentence with a single accented character (e.g.
/// "Bragança" — bytes `\xe7\x61`) doesn't get mis-routed to GBK and
/// rendered as "閺癨".
fn looks_like_gb18030(bytes: &[u8]) -> bool {
	let mut i = 0;
	let mut double_byte_pairs = 0usize;
	while i < bytes.len() {
		let byte = bytes[i];
		if byte < 0x80 {
			i += 1;
			continue;
		}
		if !(0x81..=0xFE).contains(&byte) {
			return false;
		}
		let Some(&trail) = bytes.get(i + 1) else {
			return false;
		};
		if !(0x40..=0xFE).contains(&trail) || trail == 0x7F {
			return false;
		}
		double_byte_pairs += 1;
		i += 2;
	}
	double_byte_pairs >= 2
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pure_ascii_borrows() {
		let bytes = b"name = \"Catherine\"";
		let out = decode_paradox_bytes(bytes);
		assert!(matches!(out, Cow::Borrowed(_)));
		assert_eq!(out, "name = \"Catherine\"");
	}

	#[test]
	fn utf8_with_combining_chars_borrows() {
		// "de Bragança" in UTF-8: c3 a7 == ç
		let bytes = b"dynasty = \"de Bragan\xc3\xa7a\"";
		let out = decode_paradox_bytes(bytes);
		assert!(matches!(out, Cow::Borrowed(_)));
		assert!(out.contains("Bragança"));
	}

	#[test]
	fn windows_1252_high_byte_falls_back_losslessly() {
		// Same string, but in windows-1252: ç == single byte 0xe7.
		// Strict UTF-8 fails, fallback decodes to U+00E7.
		let bytes = b"dynasty = \"de Bragan\xe7a\"";
		let out = decode_paradox_bytes(bytes);
		assert_eq!(out, "dynasty = \"de Bragança\"");
	}

	#[test]
	fn utf8_and_windows_1252_converge_for_same_logical_text() {
		let utf8 = b"\xc3\xa7\xc3\xa9\xc3\xb1";
		let win1252 = b"\xe7\xe9\xf1";
		assert_eq!(decode_paradox_bytes(utf8), decode_paradox_bytes(win1252));
	}

	#[test]
	fn utf8_bom_is_stripped() {
		let mut bytes = vec![0xEF, 0xBB, 0xBF];
		bytes.extend_from_slice(b"name = yes");
		assert_eq!(decode_paradox_bytes(&bytes), "name = yes");
	}

	#[test]
	fn utf16_le_bom_decodes() {
		// "ab" in UTF-16LE with BOM
		let bytes = [0xFF, 0xFE, b'a', 0x00, b'b', 0x00];
		assert_eq!(decode_paradox_bytes(&bytes), "ab");
	}

	#[test]
	fn utf16_be_bom_decodes() {
		let bytes = [0xFE, 0xFF, 0x00, b'a', 0x00, b'b'];
		assert_eq!(decode_paradox_bytes(&bytes), "ab");
	}

	#[test]
	fn gbk_bytes_decode_to_chinese_characters() {
		// GBK encoding for "凯瑟琳". Strict UTF-8 fails; we now detect the
		// double-byte CJK shape and decode via GB18030 instead of letting
		// the windows-1252 fallback emit mojibake.
		let bytes = b"\xbf\xad\xc9\xaa\xc1\xd5";
		let decoded = decode_paradox_bytes(bytes);
		assert_eq!(decoded, "凯瑟琳");
		// Idempotence: re-decoding the same bytes always yields the same
		// string (downstream AST-equality checks rely on this).
		assert_eq!(decode_paradox_bytes(bytes), decoded);
	}

	#[test]
	fn windows_1252_only_path_still_falls_back() {
		// Bytes that are NOT plausible GBK leads (e.g. a single 0xE7 with
		// no trail byte fitting GBK) must still decode losslessly via
		// windows-1252.
		let bytes = b"\xe7";
		assert_eq!(decode_paradox_bytes(bytes), "ç");
	}
}
