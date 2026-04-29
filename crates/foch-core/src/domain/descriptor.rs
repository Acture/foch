use crate::domain::ParseError;
use jomini::JominiDeserialize;
use std::path::Path;

#[derive(JominiDeserialize, Debug, Clone, Default)]
pub struct ModDescriptor {
	#[jomini(default)]
	pub name: String,
	pub path: Option<String>,
	#[jomini(default)]
	pub tags: Vec<String>,
	#[jomini(default)]
	pub dependencies: Vec<String>,
	#[jomini(default, duplicated)]
	pub replace_path: Vec<String>,
	pub version: Option<String>,
	pub remote_file_id: Option<String>,
	pub supported_version: Option<String>,
}

pub fn load_descriptor(path: &Path) -> Result<ModDescriptor, ParseError> {
	let data = std::fs::read(path).map_err(|err| ParseError::io(path.to_path_buf(), err))?;
	parse_descriptor_bytes(&data)
		.map_err(|err| ParseError::format(path.to_path_buf(), err.to_string()))
}

/// Parse `descriptor.mod` bytes, sniffing UTF-8 vs. windows-1252.
///
/// Workshop descriptors come in both encodings: most are ASCII or
/// windows-1252, but a non-trivial slice (notably Chinese-authored mods such
/// as workshop id `2411504869 欧陆拓展-未知领域`) ship as UTF-8, sometimes
/// with a BOM. Decoding those as windows-1252 produces mojibake and breaks
/// dep-string identity matching in [`crate::domain::dep_resolution`].
///
/// Strategy:
/// 1. UTF-8 BOM (`EF BB BF`) → strip and decode as UTF-8.
/// 2. Try UTF-8 first. If it succeeds, prefer it (windows-1252 is a
///    superset of ASCII, so any valid UTF-8 we'd misinterpret as
///    windows-1252 would otherwise mojibake silently).
/// 3. Fall back to windows-1252.
pub(crate) fn parse_descriptor_bytes(data: &[u8]) -> Result<ModDescriptor, jomini::Error> {
	const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
	let payload = data.strip_prefix(UTF8_BOM).unwrap_or(data);

	if std::str::from_utf8(payload).is_ok()
		&& let Ok(parsed) = jomini::text::de::from_utf8_slice::<ModDescriptor>(payload)
	{
		return Ok(parsed);
	}
	jomini::text::de::from_windows1252_slice::<ModDescriptor>(payload)
}

#[cfg(test)]
mod tests {
	use super::{ModDescriptor, load_descriptor, parse_descriptor_bytes};
	use std::path::Path;

	fn parse(input: &str) -> ModDescriptor {
		parse_descriptor_bytes(input.as_bytes()).expect("failed to parse synthetic descriptor")
	}

	#[test]
	fn parses_descriptor_from_corpus() {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		let test_file = manifest_dir
			.join("..")
			.join("..")
			.join("tests")
			.join("corpus")
			.join("defines")
			.join("descriptor.mod");

		let descriptor = load_descriptor(&test_file).expect("failed to parse descriptor");
		assert_eq!(descriptor.name, "defines");
		assert_eq!(descriptor.version.as_deref(), Some("0.0.1"));
		assert_eq!(descriptor.remote_file_id.as_deref(), Some("2887527268"));
	}

	#[test]
	fn parses_vanilla_style_descriptor() {
		let descriptor = parse(
			r#"
			name="Vanilla-ish Mod"
			version="1.0"
			tags={
				"Gameplay"
				"Balance"
			}
			supported_version="1.37.*"
			"#,
		);
		assert_eq!(descriptor.name, "Vanilla-ish Mod");
		assert_eq!(descriptor.version.as_deref(), Some("1.0"));
		assert_eq!(descriptor.tags, vec!["Gameplay", "Balance"]);
		assert_eq!(descriptor.supported_version.as_deref(), Some("1.37.*"));
		assert!(descriptor.dependencies.is_empty());
		assert!(descriptor.replace_path.is_empty());
		assert!(descriptor.path.is_none());
		assert!(descriptor.remote_file_id.is_none());
	}

	#[test]
	fn parses_multiple_dependencies() {
		let descriptor = parse(
			r#"
			name="With Deps"
			dependencies={
				"Extended Timeline"
				"Banner Flags"
				"Missions Expanded"
			}
			"#,
		);
		assert_eq!(
			descriptor.dependencies,
			vec!["Extended Timeline", "Banner Flags", "Missions Expanded"]
		);
	}

	#[test]
	fn parses_single_replace_path() {
		let descriptor = parse(
			r#"
			name="One Replace"
			replace_path="common/missions"
			"#,
		);
		assert_eq!(descriptor.replace_path, vec!["common/missions"]);
	}

	#[test]
	fn parses_multiple_replace_path_lines() {
		let descriptor = parse(
			r#"
			name="Multi Replace"
			replace_path="common/missions"
			replace_path="common/disasters"
			replace_path="events"
			"#,
		);
		assert_eq!(
			descriptor.replace_path,
			vec!["common/missions", "common/disasters", "events"]
		);
	}

	#[test]
	fn parses_utf8_descriptor_with_chinese_name() {
		// Pure UTF-8 (no BOM): name contains Chinese characters that would
		// mojibake under windows-1252.
		let raw = "name=\"欧陆拓展-未知领域\"\nversion=\"1.0\"\n".as_bytes();
		let descriptor =
			parse_descriptor_bytes(raw).expect("failed to parse UTF-8 descriptor without BOM");
		assert_eq!(descriptor.name, "欧陆拓展-未知领域");
		assert_eq!(descriptor.version.as_deref(), Some("1.0"));
	}

	#[test]
	fn parses_utf8_descriptor_with_bom() {
		let mut raw: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
		raw.extend_from_slice("name=\"欧陆拓展-未知领域\"\n".as_bytes());
		let descriptor =
			parse_descriptor_bytes(&raw).expect("failed to parse UTF-8 descriptor with BOM");
		assert_eq!(descriptor.name, "欧陆拓展-未知领域");
	}

	#[test]
	fn parses_windows1252_descriptor_with_high_bytes() {
		// `é` = 0xE9 in windows-1252; not valid UTF-8 on its own, so the
		// sniffer must fall back to windows-1252.
		let raw: Vec<u8> = b"name=\"Caf\xe9 Mod\"\nversion=\"1.0\"\n".to_vec();
		let descriptor = parse_descriptor_bytes(&raw).expect("failed to parse windows-1252");
		assert_eq!(descriptor.name, "Café Mod");
	}

	#[test]
	fn missing_optional_fields_fall_back_to_defaults() {
		let descriptor = parse(r#"name="Bare Minimum""#);
		assert_eq!(descriptor.name, "Bare Minimum");
		assert!(descriptor.version.is_none());
		assert!(descriptor.supported_version.is_none());
		assert!(descriptor.tags.is_empty());
		assert!(descriptor.dependencies.is_empty());
		assert!(descriptor.replace_path.is_empty());
	}

	#[test]
	fn parses_mixed_real_world_descriptor() {
		let descriptor = parse(
			r#"
			name="Holy Roman Empire Expanded"
			dependencies={
				"Extended Timeline"
				"Banner Flags"
			}
			tags={
				"Historical"
				"Gameplay"
			}
			picture="thumbnail.png"
			supported_version="v1.37.*"
			remote_file_id="1352521684"
			replace_path="common/missions"
			replace_path="history/countries"
			"#,
		);
		assert_eq!(descriptor.name, "Holy Roman Empire Expanded");
		assert_eq!(
			descriptor.dependencies,
			vec!["Extended Timeline", "Banner Flags"]
		);
		assert_eq!(descriptor.tags, vec!["Historical", "Gameplay"]);
		assert_eq!(descriptor.supported_version.as_deref(), Some("v1.37.*"));
		assert_eq!(descriptor.remote_file_id.as_deref(), Some("1352521684"));
		assert_eq!(
			descriptor.replace_path,
			vec!["common/missions", "history/countries"]
		);
	}
}
