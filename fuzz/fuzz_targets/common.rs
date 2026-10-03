use foch::game::eu4::script::parser::{ParsedStatements, ScriptSyntax, parse_clausewitz_statements};
use foch::game::eu4::text::decode_paradox_bytes;

pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

/// Parses fuzz bytes as plain Clausewitz script. The parser needs only the
/// syntax, so no file path is fabricated.
pub fn parse_clausewitz_bytes(bytes: &[u8]) -> ParsedStatements {
	let content = decode_paradox_bytes(bytes);
	parse_clausewitz_statements(ScriptSyntax::Clausewitz, &content)
}
