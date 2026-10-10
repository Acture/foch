//! Paradox script text decoding and encoding.
//!
//! EU4 reads script `.txt` (and `.csv`) bytes as **windows-1252**. Vanilla
//! files are written in it, and translation mods encode CJK text as escape
//! byte sequences inside that stream for their font patch to render, so the
//! bytes themselves carry the text. Modern mods may also save UTF-8.
//!
//! Decoding is therefore byte-faithful: valid UTF-8 (with or without a BOM)
//! reads as UTF-8, and anything else reads as windows-1252, which maps every
//! byte to one character and back. Guessing a legacy CJK codepage instead
//! rewrote those escape bytes into different ones on output.
//!
//! `decode_paradox_bytes` and `encode_paradox_text` are the single funnel for
//! Paradox-script-flavoured text. Localisation keeps its own UTF-8 reader.

use std::borrow::Cow;

use encoding_rs::WINDOWS_1252;

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Decode `bytes` as a Paradox script-flavoured text blob.
///
/// 1. Strip a leading UTF-8 BOM and decode as UTF-8.
/// 2. Strip a leading UTF-16 BOM and decode via `encoding_rs`.
/// 3. Strict UTF-8 (the modern mod default), borrowed if successful.
/// 4. Otherwise **windows-1252**, the encoding the game reads.
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

/// Encode generated script text the way the game reads it: windows-1252 when
/// every character has a windows-1252 byte, which restores the source bytes
/// of anything decoded from it, and UTF-8 only for text it cannot spell.
pub fn encode_paradox_text(text: &str) -> Cow<'_, [u8]> {
	if text.is_ascii() {
		return Cow::Borrowed(text.as_bytes());
	}
	let (encoded, _, unmappable) = WINDOWS_1252.encode(text);
	if unmappable {
		Cow::Borrowed(text.as_bytes())
	} else {
		Cow::Owned(encoded.into_owned())
	}
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
	fn translation_escape_bytes_round_trip_unchanged() {
		// A Chinese translation mod's dynasty name: 0x10-escaped code units
		// inside a windows-1252 stream, which a CJK guess would rewrite.
		let bytes = b"dynasty = \"\x10\xafQ \x10\xc8T\x10\x03^\x10\xafe\x10!X\"";
		let decoded = decode_paradox_bytes(bytes);
		assert_eq!(encode_paradox_text(&decoded).as_ref(), bytes);
	}

	#[test]
	fn legacy_cjk_bytes_round_trip_unchanged() {
		let bytes = b"name = \"\xbf\xad\xc9\xaa\xc1\xd5\"";
		let decoded = decode_paradox_bytes(bytes);
		assert_eq!(encode_paradox_text(&decoded).as_ref(), bytes);
	}

	#[test]
	fn text_without_a_windows_1252_spelling_is_written_as_utf8() {
		let text = "name = \"凯瑟琳\"";
		assert_eq!(encode_paradox_text(text).as_ref(), text.as_bytes());
		assert_eq!(
			encode_paradox_text("dynasty = \"de Bragança\"").as_ref(),
			b"dynasty = \"de Bragan\xe7a\""
		);
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
