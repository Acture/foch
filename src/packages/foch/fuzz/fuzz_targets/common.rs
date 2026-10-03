use std::path::PathBuf;

use foch::game::eu4::script::parser::{ParseResult, parse_clausewitz_content};
use foch::game::eu4::text::decode_paradox_bytes;

pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

pub fn parse_clausewitz_file_from_bytes(path: &str, bytes: &[u8]) -> ParseResult {
	let content = decode_paradox_bytes(bytes);
	parse_clausewitz_content(PathBuf::from(path), &content)
}
