use crate::model::{GamePathBuf, GamePathError};
use crate::playset::{ParseError, ParseErrorKind};
use encoding_rs::WINDOWS_1252;
use jomini::JominiDeserialize;
use jomini::text::{ObjectReader, TextTape};
use serde::de::Error as _;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, Prefix};
use typed_path::{Utf8WindowsComponent, Utf8WindowsPath};

/// A `descriptor.mod` as the launcher writes it.
#[derive(Debug, Clone, Default)]
pub struct ModDescriptor {
	pub name: String,
	pub path: Option<String>,
	pub tags: Vec<String>,
	pub dependencies: Vec<String>,
	/// The directories this mod replaces, read with [`parse_replace_path`].
	pub replace_path: Vec<GamePathBuf>,
	pub version: Option<String>,
	pub remote_file_id: Option<String>,
	pub supported_version: Option<String>,
}

/// A launcher's copy of a mod's descriptor (`mod/ugc_<id>.mod` beside
/// `dlc_load.json`), read for what locates and names the mod. The mod's
/// `replace_path` is read from its own `descriptor.mod` only, so the copy's
/// value is never interpreted and cannot make the copy unreadable.
#[derive(Debug, Clone, Default)]
pub struct LauncherDescriptor {
	pub name: String,
	pub path: Option<String>,
	pub version: Option<String>,
	pub remote_file_id: Option<String>,
}

/// Every descriptor field except `replace_path`, decoded the way jomini
/// decodes text.
#[derive(JominiDeserialize)]
struct TextFields {
	#[jomini(default)]
	name: String,
	path: Option<String>,
	#[jomini(default)]
	tags: Vec<String>,
	#[jomini(default)]
	dependencies: Vec<String>,
	version: Option<String>,
	remote_file_id: Option<String>,
	supported_version: Option<String>,
}

/// The descriptor fields before any value is interpreted. Each
/// `replace_path` value holds the bytes between its quotes, decoded but
/// otherwise as written (see [`parse_replace_path`]).
struct RawModDescriptor {
	fields: TextFields,
	replace_path: Vec<String>,
}

impl TryFrom<RawModDescriptor> for ModDescriptor {
	type Error = InvalidReplacePath;

	fn try_from(raw: RawModDescriptor) -> Result<Self, InvalidReplacePath> {
		let replace_path = raw
			.replace_path
			.into_iter()
			.map(|value| {
				parse_replace_path(&value).map_err(|reason| InvalidReplacePath { value, reason })
			})
			.collect::<Result<_, _>>()?;
		let fields = raw.fields;
		Ok(Self {
			name: fields.name,
			path: fields.path,
			tags: fields.tags,
			dependencies: fields.dependencies,
			replace_path,
			version: fields.version,
			remote_file_id: fields.remote_file_id,
			supported_version: fields.supported_version,
		})
	}
}

impl From<RawModDescriptor> for LauncherDescriptor {
	fn from(raw: RawModDescriptor) -> Self {
		let fields = raw.fields;
		Self {
			name: fields.name,
			path: fields.path,
			version: fields.version,
			remote_file_id: fields.remote_file_id,
		}
	}
}

/// Why a `replace_path` value names no directory under the game root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacePathError {
	/// A drive, UNC or device prefix names a location outside the game root.
	Prefix,
	/// A `..` component would leave the directory the value names.
	ParentComponent,
	/// Nothing is left once separators and `.` components are dropped.
	Empty,
	/// A component is not a name a game path can hold.
	Name(GamePathError),
}

impl fmt::Display for ReplacePathError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Prefix => f.write_str("it has a drive, UNC or device prefix"),
			Self::ParentComponent => f.write_str("it has a `..` component"),
			Self::Empty => f.write_str("it names no directory"),
			Self::Name(error) => write!(f, "{error}"),
		}
	}
}

/// A `replace_path` value that [`parse_replace_path`] rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidReplacePath {
	/// The value as written in the descriptor.
	pub value: String,
	pub reason: ReplacePathError,
}

impl fmt::Display for InvalidReplacePath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"replace_path {:?} is not a directory under the game root: {}",
			self.value, self.reason
		)
	}
}

impl std::error::Error for InvalidReplacePath {}

/// Reads a `replace_path` value as the directory it names under the game
/// root.
///
/// The value is the text between its quotes exactly as written: escape
/// sequences are not interpreted, and nothing is trimmed. Descriptors are
/// written for the Windows game, so it is read in Windows path syntax: `/` and
/// every `\` both separate components. The value is always relative to the
/// game root, so a leading root separator, trailing or repeated separators and
/// `.` components add nothing. A drive, UNC or device prefix, a `..`
/// component, a value with no component left, or a component that is not a
/// valid game path name is an error. Whitespace is part of a name, as it is in
/// an inventory path, and case is kept as written.
///
/// Two consequences of the descriptor format: a value cannot end in `\`,
/// because the reader takes `\"` as an escaped quote and keeps reading, and
/// `\"` inside a value is a separator followed by a name starting with `"`.
pub fn parse_replace_path(value: &str) -> Result<GamePathBuf, ReplacePathError> {
	let mut directory: Option<GamePathBuf> = None;
	for component in Utf8WindowsPath::new(value).components() {
		match component {
			Utf8WindowsComponent::Prefix(_) => return Err(ReplacePathError::Prefix),
			Utf8WindowsComponent::RootDir | Utf8WindowsComponent::CurDir => {}
			Utf8WindowsComponent::ParentDir => return Err(ReplacePathError::ParentComponent),
			Utf8WindowsComponent::Normal(name) => {
				let name = GamePathBuf::parse(name).map_err(ReplacePathError::Name)?;
				directory = Some(match directory {
					Some(parent) => parent.join(&name),
					None => name,
				});
			}
		}
	}
	directory.ok_or(ReplacePathError::Empty)
}

/// Why descriptor bytes do not give a [`ModDescriptor`].
#[derive(Debug)]
pub(crate) enum DescriptorParseError {
	Syntax(jomini::Error),
	ReplacePath(InvalidReplacePath),
}

impl fmt::Display for DescriptorParseError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Syntax(error) => write!(f, "{error}"),
			Self::ReplacePath(error) => write!(f, "{error}"),
		}
	}
}

/// Reads a mod's own descriptor. A `replace_path` value that names no
/// directory under the game root is reported with
/// [`ParseErrorKind::InvalidReplacePath`], so a caller can tell it from a
/// descriptor that does not parse at all.
pub fn load_descriptor(path: &Path) -> Result<ModDescriptor, ParseError> {
	parse_descriptor_bytes(&read_descriptor_file(path)?).map_err(|error| match error {
		DescriptorParseError::Syntax(error) => {
			ParseError::format(path.to_path_buf(), error.to_string())
		}
		DescriptorParseError::ReplacePath(error) => ParseError {
			kind: ParseErrorKind::InvalidReplacePath,
			path: path.to_path_buf(),
			message: error.to_string(),
		},
	})
}

/// Reads a launcher's copy of a descriptor; see [`LauncherDescriptor`].
pub fn load_launcher_descriptor(path: &Path) -> Result<LauncherDescriptor, ParseError> {
	parse_raw_descriptor_bytes(&read_descriptor_file(path)?)
		.map(LauncherDescriptor::from)
		.map_err(|error| ParseError::format(path.to_path_buf(), error.to_string()))
}

fn read_descriptor_file(path: &Path) -> Result<Vec<u8>, ParseError> {
	let mut file = open_regular_descriptor_no_follow(path)
		.map_err(|err| ParseError::io(path.to_path_buf(), err))?;
	let mut data = Vec::new();
	file.read_to_end(&mut data)
		.map_err(|err| ParseError::io(path.to_path_buf(), err))?;
	Ok(data)
}

fn open_regular_descriptor_no_follow(path: &Path) -> io::Result<File> {
	let metadata = fs::symlink_metadata(path)?;
	if !metadata.file_type().is_file() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!(
				"descriptor must be a regular file, not a symlink: {}",
				path.display()
			),
		));
	}
	#[cfg(unix)]
	let file = {
		use rustix::fs::{Mode, OFlags, open};
		let descriptor = open(
			path,
			OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
			Mode::empty(),
		)
		.map_err(io::Error::from)?;
		File::from(descriptor)
	};
	#[cfg(not(unix))]
	let file = File::open(path)?;
	if !file.metadata()?.file_type().is_file() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("descriptor must be a regular file: {}", path.display()),
		));
	}
	Ok(file)
}

/// Parse `descriptor.mod` bytes into a [`ModDescriptor`], reading each
/// `replace_path` with [`parse_replace_path`].
pub(crate) fn parse_descriptor_bytes(data: &[u8]) -> Result<ModDescriptor, DescriptorParseError> {
	let raw = parse_raw_descriptor_bytes(data).map_err(DescriptorParseError::Syntax)?;
	ModDescriptor::try_from(raw).map_err(DescriptorParseError::ReplacePath)
}

/// Parse `descriptor.mod` bytes, sniffing UTF-8 vs. windows-1252.
///
/// Workshop descriptors come in both encodings: most are ASCII or
/// windows-1252, but a non-trivial slice (notably Chinese-authored mods such
/// as workshop id `2411504869 欧陆拓展-未知领域`) ship as UTF-8, sometimes
/// with a BOM. Decoding those as windows-1252 produces mojibake and breaks
/// dep-string identity matching in [`crate::playset::dependency`].
///
/// Strategy:
/// 1. UTF-8 BOM (`EF BB BF`) → strip and decode as UTF-8.
/// 2. Try UTF-8 first. If it succeeds, prefer it (windows-1252 is a
///    superset of ASCII, so any valid UTF-8 we'd misinterpret as
///    windows-1252 would otherwise mojibake silently).
/// 3. Fall back to windows-1252.
///
/// jomini's text decoding drops every `\` and trims trailing whitespace, so
/// `replace_path` values are decoded from the raw bytes between their quotes
/// instead, in the same encoding as the rest of the file.
fn parse_raw_descriptor_bytes(data: &[u8]) -> Result<RawModDescriptor, jomini::Error> {
	const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
	let payload = data.strip_prefix(UTF8_BOM).unwrap_or(data);
	let tape = TextTape::from_slice(payload)?;

	if std::str::from_utf8(payload).is_ok()
		&& let Ok(parsed) = read_raw_descriptor(&tape.utf8_reader(), |bytes| {
			std::str::from_utf8(bytes)
				.map(Cow::Borrowed)
				.map_err(jomini::Error::custom)
		}) {
		return Ok(parsed);
	}
	read_raw_descriptor(&tape.windows1252_reader(), |bytes| {
		Ok(WINDOWS_1252.decode_without_bom_handling(bytes).0)
	})
}

fn read_raw_descriptor<'data, E>(
	reader: &ObjectReader<'data, '_, E>,
	decode: impl Fn(&'data [u8]) -> Result<Cow<'data, str>, jomini::Error>,
) -> Result<RawModDescriptor, jomini::Error>
where
	E: jomini::Encoding + Clone,
{
	let fields: TextFields = reader.deserialize()?;
	let replace_path = reader
		.fields()
		.filter(|(key, _, _)| key.read_str() == "replace_path")
		.map(|(_, _, value)| Ok(decode(value.read_scalar()?.as_bytes())?.into_owned()))
		.collect::<Result<_, jomini::Error>>()?;
	Ok(RawModDescriptor {
		fields,
		replace_path,
	})
}

/// Why a host directory has no descriptor `path` text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnwritableDescriptorPath {
	/// A name is not valid UTF-8.
	NotUtf8,
	/// A name holds a `\`, which the descriptor format drops.
	Backslash,
	/// A device or verbatim prefix names no directory the launcher reads.
	DevicePrefix,
}

impl fmt::Display for UnwritableDescriptorPath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			Self::NotUtf8 => "the path is not valid UTF-8",
			Self::Backslash => "a descriptor value cannot hold a `\\`",
			Self::DevicePrefix => "a device or verbatim prefix has no descriptor spelling",
		})
	}
}

/// The descriptor `path` text naming the host directory `path`.
///
/// The launcher reads `path` as a host path, and the descriptor reader drops
/// every `\` from a value, so the text is built from the path's components
/// with `/` between them. On Windows `/` is read as the same separator as `\`,
/// and a verbatim (`\\?\`) drive or UNC prefix is written as the plain one.
/// Elsewhere `\` is an ordinary name character the format cannot carry. A name
/// that is not UTF-8, a name holding `\`, and a device or other verbatim
/// prefix are errors, never a text naming some other directory.
pub fn descriptor_path_text(path: &Path) -> Result<String, UnwritableDescriptorPath> {
	let mut text = String::new();
	let mut after_name = false;
	for component in path.components() {
		let name: &str = match component {
			Component::Prefix(prefix) => {
				match prefix.kind() {
					Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
						text.push(char::from(drive));
						text.push(':');
						after_name = false;
					}
					Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
						text.push_str("//");
						text.push_str(descriptor_name(server)?);
						text.push('/');
						text.push_str(descriptor_name(share)?);
						after_name = true;
					}
					Prefix::Verbatim(_) | Prefix::DeviceNS(_) => {
						return Err(UnwritableDescriptorPath::DevicePrefix);
					}
				}
				continue;
			}
			Component::RootDir => {
				text.push('/');
				after_name = false;
				continue;
			}
			Component::CurDir => ".",
			Component::ParentDir => "..",
			Component::Normal(name) => descriptor_name(name)?,
		};
		if after_name {
			text.push('/');
		}
		text.push_str(name);
		after_name = true;
	}
	Ok(text)
}

fn descriptor_name(name: &OsStr) -> Result<&str, UnwritableDescriptorPath> {
	let name = name.to_str().ok_or(UnwritableDescriptorPath::NotUtf8)?;
	if name.contains('\\') {
		return Err(UnwritableDescriptorPath::Backslash);
	}
	Ok(name)
}

/// A value quoted for a descriptor: `"` and `\` are escaped, so the value
/// cannot end the quotes early.
pub fn escape_descriptor_value(value: &str) -> String {
	value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
	use super::{
		DescriptorParseError, ModDescriptor, ReplacePathError, UnwritableDescriptorPath,
		descriptor_path_text, escape_descriptor_value, load_descriptor, load_launcher_descriptor,
		parse_descriptor_bytes, parse_replace_path,
	};
	use crate::model::GamePathErrorKind;
	use crate::playset::ParseErrorKind;
	use std::fs;
	use std::path::Path;

	fn parse(input: &str) -> ModDescriptor {
		parse_descriptor_bytes(input.as_bytes()).expect("failed to parse synthetic descriptor")
	}

	fn replace_texts(descriptor: &ModDescriptor) -> Vec<&str> {
		descriptor
			.replace_path
			.iter()
			.map(|path| path.as_str())
			.collect()
	}

	/// Descriptor `replace_path` text is read in the Windows syntax the
	/// launcher writes: either separator splits components, and a root
	/// separator, repeated or trailing separators and `.` add nothing.
	#[test]
	fn replace_path_is_read_as_a_directory_under_the_game_root() {
		for text in [
			"common/ideas",
			r"common\ideas",
			"/common/ideas/",
			r"\common\ideas\",
			"./common/ideas",
			"common/./ideas",
			"common//ideas",
		] {
			assert_eq!(
				parse_replace_path(text).expect(text).as_str(),
				"common/ideas",
				"{text}"
			);
		}
		// Names are kept as written: no case folding and no trimming.
		assert_eq!(
			parse_replace_path("Common/Ideas")
				.expect("mixed case")
				.as_str(),
			"Common/Ideas"
		);
		assert_eq!(
			parse_replace_path(" common/ideas")
				.expect("leading space")
				.as_str(),
			" common/ideas"
		);
	}

	#[test]
	fn replace_path_that_names_no_directory_under_the_game_root_is_an_error() {
		for (text, expected) in [
			(r"C:\x", ReplacePathError::Prefix),
			("C:/x", ReplacePathError::Prefix),
			(r"\\server\share\x", ReplacePathError::Prefix),
			(r"..\x", ReplacePathError::ParentComponent),
			("common/../x", ReplacePathError::ParentComponent),
			("", ReplacePathError::Empty),
			("/", ReplacePathError::Empty),
			(".", ReplacePathError::Empty),
		] {
			assert_eq!(parse_replace_path(text), Err(expected), "{text:?}");
		}
		let Err(ReplacePathError::Name(error)) = parse_replace_path("common/a:b") else {
			panic!("a `:` inside a name is not a game path name");
		};
		assert_eq!(
			error.kind,
			GamePathErrorKind::ReservedCharacter {
				component: "a:b".to_string(),
				character: ':',
			}
		);
	}

	/// A descriptor whose `replace_path` names no directory is reported as
	/// such, with the value, instead of as a descriptor that does not parse.
	#[test]
	fn an_invalid_replace_path_is_its_own_descriptor_error() {
		let DescriptorParseError::ReplacePath(invalid) =
			parse_descriptor_bytes(b"name=\"Bad\"\nreplace_path=\"common/../events\"\n")
				.expect_err("`..` leaves the game root")
		else {
			panic!("expected a replace_path error");
		};
		assert_eq!(invalid.value, "common/../events");
		assert_eq!(invalid.reason, ReplacePathError::ParentComponent);

		let root = tempfile::tempdir().expect("fixture root");
		let path = root.path().join("descriptor.mod");
		fs::write(&path, "name=\"Bad\"\nreplace_path=\"C:/events\"\n").expect("write");
		let error = load_descriptor(&path).expect_err("a drive prefix leaves the game root");
		assert_eq!(error.kind, ParseErrorKind::InvalidReplacePath);
		assert_eq!(error.path, path);
		assert!(error.message.contains(r#""C:/events""#), "{error}");
		assert!(error.message.contains("drive"), "{error}");
	}

	/// The Windows syntax is what a real descriptor delivers: `replace_path`
	/// is read from the bytes between its quotes, where jomini's text decoding
	/// would drop every `\` (`common\ideas` became `commonideas`) and trim
	/// trailing whitespace.
	#[test]
	fn replace_path_is_read_from_the_bytes_the_descriptor_holds() {
		for (value, expected) in [
			(r#""common\ideas""#, "common/ideas"),
			(r#""common\\ideas""#, "common/ideas"),
			(r#""\common\ideas""#, "common/ideas"),
			(r"common\ideas", "common/ideas"),
			(r#""common/ideas ""#, "common/ideas "),
		] {
			let descriptor =
				parse_descriptor_bytes(format!("name=\"x\"\nreplace_path={value}\n").as_bytes())
					.expect(value);
			assert_eq!(replace_texts(&descriptor), vec![expected], "{value}");
		}

		// A windows-1252 descriptor reads its values the same way.
		let descriptor =
			parse_descriptor_bytes(b"name=\"Caf\xe9\"\nreplace_path=\"common\\ideas\"\n")
				.expect("windows-1252 descriptor");
		assert_eq!(descriptor.name, "Café");
		assert_eq!(replace_texts(&descriptor), vec!["common/ideas"]);
	}

	#[test]
	fn replace_path_bytes_that_leave_the_game_root_are_an_error() {
		for (value, expected) in [
			(r"..\x", ReplacePathError::ParentComponent),
			(r"\\server\share\x", ReplacePathError::Prefix),
			(r"C:\x", ReplacePathError::Prefix),
		] {
			let DescriptorParseError::ReplacePath(invalid) = parse_descriptor_bytes(
				format!("name=\"x\"\nreplace_path=\"{value}\"\n").as_bytes(),
			)
			.expect_err(value) else {
				panic!("expected a replace_path error for {value}");
			};
			assert_eq!(invalid.value, value);
			assert_eq!(invalid.reason, expected, "{value}");
		}
	}

	/// A launcher's copy of a descriptor is read for its name, `path`, id and
	/// version; its `replace_path` is not interpreted, so a value the mod's own
	/// descriptor would reject cannot make the copy unreadable.
	#[test]
	fn a_launcher_descriptor_is_read_without_its_replace_path() {
		let root = tempfile::tempdir().expect("fixture root");
		let path = root.path().join("ugc_1001.mod");
		fs::write(
			&path,
			"name=\"Named\"\npath=\"/mods/named\"\nversion=\"2.0\"\n\
			 remote_file_id=\"1001\"\nreplace_path=\"common/../events\"\n",
		)
		.expect("write launcher descriptor");

		let descriptor = load_launcher_descriptor(&path).expect("launcher descriptor");
		assert_eq!(descriptor.name, "Named");
		assert_eq!(descriptor.path.as_deref(), Some("/mods/named"));
		assert_eq!(descriptor.version.as_deref(), Some("2.0"));
		assert_eq!(descriptor.remote_file_id.as_deref(), Some("1001"));
		assert_eq!(
			load_descriptor(&path)
				.expect_err("the mod's own reading")
				.kind,
			ParseErrorKind::InvalidReplacePath
		);
	}

	/// A descriptor `path` names a host directory exactly: reading the
	/// descriptor back yields that directory.
	#[test]
	fn descriptor_path_text_names_the_directory_exactly() {
		let temp = tempfile::tempdir().expect("temp dir");
		let directory = temp.path().join("merged \"quoted\" output");
		let text = descriptor_path_text(&directory).expect("a UTF-8 directory has text");
		let descriptor = temp.path().join("descriptor.mod");
		fs::write(
			&descriptor,
			format!("name=\"x\"\npath=\"{}\"\n", escape_descriptor_value(&text)),
		)
		.expect("write descriptor");

		let read_back = load_launcher_descriptor(&descriptor)
			.expect("read descriptor")
			.path
			.expect("descriptor path");
		assert_eq!(std::path::PathBuf::from(read_back), directory);
	}

	/// The descriptor reader drops `\`, and a name that is not UTF-8 has no
	/// text at all. Off Windows either can be part of a directory name, so such
	/// a directory is refused instead of being written as some other one.
	#[cfg(unix)]
	#[test]
	fn a_directory_a_descriptor_cannot_name_is_an_error() {
		use std::ffi::OsStr;
		use std::os::unix::ffi::OsStrExt;

		let backslash = Path::new("/merged").join(r"out\put");
		let non_utf8 = Path::new("/merged").join(OsStr::from_bytes(b"out\xffput"));
		assert_eq!(
			descriptor_path_text(&backslash),
			Err(UnwritableDescriptorPath::Backslash)
		);
		assert_eq!(
			descriptor_path_text(&non_utf8),
			Err(UnwritableDescriptorPath::NotUtf8)
		);
		assert_eq!(
			descriptor_path_text(Path::new("/merged/a:/b")).as_deref(),
			Ok("/merged/a:/b")
		);
	}

	/// Windows spells a verbatim drive or UNC directory with a `\\?\` prefix
	/// the launcher does not read; the descriptor names the plain directory.
	#[cfg(windows)]
	#[test]
	fn a_windows_directory_is_written_with_its_plain_prefix() {
		for (path, text) in [
			(r"D:\mods\merged", "D:/mods/merged"),
			(r"\\?\D:\mods\merged", "D:/mods/merged"),
			(r"\\server\share\merged", "//server/share/merged"),
			(r"\\?\UNC\server\share\merged", "//server/share/merged"),
		] {
			assert_eq!(descriptor_path_text(Path::new(path)).as_deref(), Ok(text));
		}
		assert_eq!(
			descriptor_path_text(Path::new(r"\\.\pipe\merged")),
			Err(UnwritableDescriptorPath::DevicePrefix)
		);
	}

	#[test]
	fn parses_descriptor_from_corpus() {
		let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
		let test_file = manifest_dir
			.join("tests")
			.join("corpus")
			.join("defines")
			.join("descriptor.mod");

		let descriptor = load_descriptor(&test_file).expect("failed to parse descriptor");
		assert_eq!(descriptor.name, "defines");
		assert_eq!(descriptor.version.as_deref(), Some("0.0.1"));
		assert_eq!(descriptor.remote_file_id.as_deref(), Some("2887527268"));
	}

	#[cfg(unix)]
	#[test]
	fn rejects_descriptor_symlinks_without_reading_the_target() {
		use std::os::unix::fs::symlink;

		let root = tempfile::tempdir().expect("fixture root");
		let target = root.path().join("outside.mod");
		let descriptor = root.path().join("descriptor.mod");
		fs::write(&target, "name=\"outside\"\n").expect("write target");
		symlink(&target, &descriptor).expect("link descriptor");

		let error = load_descriptor(&descriptor).expect_err("descriptor symlink must fail closed");
		assert!(error.to_string().contains("regular file"));
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
		assert_eq!(replace_texts(&descriptor), vec!["common/missions"]);
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
			replace_texts(&descriptor),
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
			replace_texts(&descriptor),
			vec!["common/missions", "history/countries"]
		);
	}
}
