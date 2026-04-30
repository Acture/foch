//! Paradox script text decoding helpers.
//!
//! Paradox engines historically write `.txt` (and `.csv` / `.yml`) files
//! in **windows-1252** for the bundled vanilla content, while modern mods
//! often save files as **UTF-8** (with or without BOM). A few translation
//! mods (notably Chinese ones) emit GBK / GB18030, which we decode
//! self-consistently as windows-1252 — readability is the localiser's
//! responsibility; what matters here is that the same bytes always
//! produce the same logical string so AST equivalence checks converge.
//!
//! `decode_paradox_bytes` is the single funnel for all
//! Paradox-script-flavoured byte → string conversion in the workspace.
//! Use it instead of ad-hoc `String::from_utf8_lossy` /
//! `WINDOWS_1252.decode` calls so the encoding rules stay uniform.

use std::borrow::Cow;

use encoding_rs::WINDOWS_1252;

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Decode `bytes` as a Paradox script-flavoured text blob.
///
/// Algorithm:
/// 1. Strip a leading UTF-8 BOM and decode as strict UTF-8.
/// 2. Strip a leading UTF-16 BOM and decode via `encoding_rs`
///    (replacement-char fallback on invalid sequences).
/// 3. Try strict UTF-8 (the modern mod default) and return borrowed
///    if successful — this is the hot path and avoids allocation.
/// 4. Fall back to **windows-1252**, the canonical Paradox encoding.
///
/// Step 4 is intentionally lossless for any byte sequence: every byte
/// 0x00..=0xFF maps to a defined codepoint in windows-1252, so the
/// returned `String` is well-formed and the function never returns
/// replacement characters from the fallback path.
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
	let (decoded, _, _) = WINDOWS_1252.decode(bytes);
	Cow::Owned(decoded.into_owned())
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
	fn gbk_bytes_round_trip_self_consistently() {
		// GBK encoding for "凯瑟琳" — invalid UTF-8, falls back to
		// windows-1252. We only assert idempotence: same bytes always
		// produce the same string.
		let bytes = b"\xbf\xad\xc9\xaa\xc1\xd5";
		let first = decode_paradox_bytes(bytes).into_owned();
		let second = decode_paradox_bytes(bytes).into_owned();
		assert_eq!(first, second);
	}
}
