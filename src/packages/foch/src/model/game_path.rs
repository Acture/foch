//! Paths in the game's virtual filesystem.
//!
//! A [`GamePath`] names a file the way the game loads it: relative to the game
//! root or to a mod root, `/`-separated, and spelled the same on every host. It
//! is a [`RelativePath`] that also satisfies the constraints of a game root, so
//! it is the identity of a file in inventories, parse caches, ASTs, rule
//! matching and provenance. Physical files stay [`Path`]/[`PathBuf`] with host
//! semantics; the two meet only at [`GamePathBuf::from_physical`] and
//! [`GamePath::to_path`], both of which take the root explicitly.
//!
//! Every constructor validates, including deserialization. A valid path is
//! non-empty and in canonical text form (no leading, trailing or repeated `/`),
//! and each component is a UTF-8 name other than `.` and `..` that contains no
//! character a supported host parses as path structure: `\` separates
//! components on Windows, `:` introduces a drive or stream prefix there, and
//! NUL terminates a name. A physical name that breaks these rules cannot be
//! given a portable identity without colliding with another file or escaping
//! the root, so conversion reports it instead of rewriting it. Case and Unicode
//! form are preserved exactly as the name is spelled.

use ref_cast::{RefCastCustom, ref_cast_custom};
use relative_path::{Component, FromPathErrorKind, RelativePath, RelativePathBuf};
use rkyv::rancor::{Fallible, Source};
use rkyv::string::{ArchivedString, StringResolver};
use rkyv::with::{ArchiveWith, DeserializeWith, SerializeWith};
use rkyv::{Place, SerializeUnsized};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// A validated path relative to a game or mod root. See the [module
/// documentation](self) for the invariants.
///
/// Equality and hashing compare the canonical text, which for a valid path is
/// the same as comparing components. Ordering is the byte order of that text,
/// not [`RelativePath`]'s component order: the two differ only when a name
/// sorts below `/` (for example `a-b` against `a/b`), and byte order is the
/// order inventories, definition load order and generated output already
/// follow.
#[derive(RefCastCustom)]
#[repr(transparent)]
pub struct GamePath(RelativePath);

/// An owned [`GamePath`].
#[derive(Clone)]
pub struct GamePathBuf(RelativePathBuf);

/// Why a value is not a valid [`GamePath`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GamePathErrorKind {
	Empty,
	NonUtf8,
	/// Absolute, rooted, or carrying a platform prefix.
	NotRelative,
	/// The physical path does not lie under the root it was taken against.
	OutsideRoot,
	/// A trailing or repeated `/`.
	EmptyComponent,
	CurrentComponent,
	ParentComponent,
	ReservedCharacter {
		component: String,
		character: char,
	},
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamePathError {
	/// The rejected input, rendered for diagnostics only.
	pub input: String,
	pub kind: GamePathErrorKind,
}

impl GamePathError {
	fn new(input: impl Into<String>, kind: GamePathErrorKind) -> Self {
		Self {
			input: input.into(),
			kind,
		}
	}
}

impl fmt::Display for GamePathErrorKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Empty => f.write_str("the path is empty"),
			Self::NonUtf8 => f.write_str("a component is not valid UTF-8"),
			Self::NotRelative => f.write_str("the path is absolute or has a platform prefix"),
			Self::OutsideRoot => f.write_str("the path is not under its root"),
			Self::EmptyComponent => f.write_str("the path has a trailing or repeated `/`"),
			Self::CurrentComponent => f.write_str("the path has a `.` component"),
			Self::ParentComponent => f.write_str("the path has a `..` component"),
			Self::ReservedCharacter {
				component,
				character,
			} => write!(
				f,
				"component `{component}` contains {character:?}, which a supported host parses as path structure"
			),
		}
	}
}

impl fmt::Display for GamePathError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "invalid game path `{}`: {}", self.input, self.kind)
	}
}

impl std::error::Error for GamePathError {}

fn validate(text: &str) -> Result<(), GamePathErrorKind> {
	if text.is_empty() {
		return Err(GamePathErrorKind::Empty);
	}
	if text.starts_with('/') {
		return Err(GamePathErrorKind::NotRelative);
	}
	if text.ends_with('/') || text.contains("//") {
		return Err(GamePathErrorKind::EmptyComponent);
	}
	for component in RelativePath::new(text).components() {
		match component {
			Component::CurDir => return Err(GamePathErrorKind::CurrentComponent),
			Component::ParentDir => return Err(GamePathErrorKind::ParentComponent),
			Component::Normal(name) => {
				if let Some(character) = name.chars().find(|c| matches!(c, '\\' | ':' | '\0')) {
					return Err(GamePathErrorKind::ReservedCharacter {
						component: name.to_string(),
						character,
					});
				}
			}
		}
	}
	Ok(())
}

impl GamePath {
	#[ref_cast_custom]
	const fn from_relative_path_unchecked(path: &RelativePath) -> &Self;

	/// Borrows `text` as a game path. `text` uses the portable `/` syntax.
	pub fn new(text: &str) -> Result<&Self, GamePathError> {
		validate(text).map_err(|kind| GamePathError::new(text, kind))?;
		Ok(Self::from_relative_path_unchecked(RelativePath::new(text)))
	}

	/// Re-borrows a sub-path that the relative-path API derived from a valid
	/// path. Parents, suffixes and joins of canonical paths are canonical.
	fn derived(path: &RelativePath) -> &Self {
		debug_assert_eq!(validate(path.as_str()), Ok(()), "{path}");
		Self::from_relative_path_unchecked(path)
	}

	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}

	pub fn as_relative_path(&self) -> &RelativePath {
		&self.0
	}

	/// The physical location of this path under `root`, spelled exactly as
	/// `root.join(..)` spells it: each component is pushed onto `root`, so a
	/// root that already ends in a separator does not gain a second one.
	/// Pushing is safe because every component is a plain name.
	pub fn to_path(&self, root: impl AsRef<Path>) -> PathBuf {
		let mut path = root.as_ref().to_path_buf();
		for component in self.0.iter() {
			path.push(component);
		}
		path
	}

	/// The directory holding this path, or `None` for a path directly under
	/// the root.
	pub fn parent(&self) -> Option<&Self> {
		self.0
			.parent()
			.filter(|parent| !parent.as_str().is_empty())
			.map(Self::derived)
	}

	/// The final component. Every valid path has one.
	pub fn file_name(&self) -> &str {
		self.0
			.file_name()
			.expect("a valid game path ends in a normal component")
	}

	pub fn join(&self, child: &Self) -> GamePathBuf {
		let joined = self.0.join(&child.0);
		debug_assert_eq!(validate(joined.as_str()), Ok(()), "{joined}");
		GamePathBuf(joined)
	}

	/// Whether `prefix` equals this path or is one of its ancestor directories,
	/// comparing whole components.
	pub fn starts_with(&self, prefix: &Self) -> bool {
		self.0.starts_with(&prefix.0)
	}

	/// Whether this path names an entry directly inside `directory`, not one
	/// nested deeper.
	pub fn is_child_of(&self, directory: &Self) -> bool {
		self.parent() == Some(directory)
	}

	/// Whether this path lies strictly inside the directory whose components
	/// are `directory`. `same_name(component, name)` compares one leading
	/// component with one directory name, which is where a caller states its
	/// case policy (`str::eq`, `str::eq_ignore_ascii_case`).
	pub fn is_inside(&self, directory: &[&str], same_name: impl Fn(&str, &str) -> bool) -> bool {
		let mut components = self.0.iter();
		directory.iter().all(|name| {
			components
				.next()
				.is_some_and(|component| same_name(component, name))
		}) && components.next().is_some()
	}

	/// The part of this path below `prefix`, or `None` when `prefix` is not a
	/// strict ancestor directory.
	pub fn strip_prefix(&self, prefix: &Self) -> Option<&Self> {
		self.0
			.strip_prefix(&prefix.0)
			.ok()
			.filter(|rest| !rest.as_str().is_empty())
			.map(Self::derived)
	}
}

impl GamePathBuf {
	/// Parses text in the portable `/` syntax.
	pub fn parse(text: &str) -> Result<Self, GamePathError> {
		GamePath::new(text).map(ToOwned::to_owned)
	}

	/// Converts a relative host path component by component. Host separators
	/// split components; every other character stays part of its name.
	pub fn from_native_relative(path: &Path) -> Result<Self, GamePathError> {
		let error = |kind| GamePathError::new(path.display().to_string(), kind);
		let relative = RelativePathBuf::from_path(path).map_err(|source| {
			error(match source.kind() {
				FromPathErrorKind::NonUtf8 => GamePathErrorKind::NonUtf8,
				_ => GamePathErrorKind::NotRelative,
			})
		})?;
		validate(relative.as_str()).map_err(error)?;
		Ok(Self(relative))
	}

	/// The game path of `physical`, a file under `root`.
	pub fn from_physical(root: &Path, physical: &Path) -> Result<Self, GamePathError> {
		let relative = physical.strip_prefix(root).map_err(|_| {
			GamePathError::new(
				physical.display().to_string(),
				GamePathErrorKind::OutsideRoot,
			)
		})?;
		Self::from_native_relative(relative)
	}

	pub fn as_game_path(&self) -> &GamePath {
		GamePath::from_relative_path_unchecked(&self.0)
	}

	pub fn into_string(self) -> String {
		self.0.into_string()
	}
}

impl Deref for GamePath {
	type Target = RelativePath;

	fn deref(&self) -> &RelativePath {
		&self.0
	}
}

impl Deref for GamePathBuf {
	type Target = GamePath;

	fn deref(&self) -> &GamePath {
		self.as_game_path()
	}
}

impl Borrow<GamePath> for GamePathBuf {
	fn borrow(&self) -> &GamePath {
		self.as_game_path()
	}
}

impl ToOwned for GamePath {
	type Owned = GamePathBuf;

	fn to_owned(&self) -> GamePathBuf {
		GamePathBuf(self.0.to_relative_path_buf())
	}
}

impl PartialEq for GamePath {
	fn eq(&self, other: &Self) -> bool {
		self.as_str() == other.as_str()
	}
}

impl Eq for GamePath {}

impl PartialOrd for GamePath {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for GamePath {
	fn cmp(&self, other: &Self) -> Ordering {
		self.as_str().cmp(other.as_str())
	}
}

impl Hash for GamePath {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.as_str().hash(state);
	}
}

impl PartialEq for GamePathBuf {
	fn eq(&self, other: &Self) -> bool {
		**self == **other
	}
}

impl Eq for GamePathBuf {}

impl PartialOrd for GamePathBuf {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for GamePathBuf {
	fn cmp(&self, other: &Self) -> Ordering {
		(**self).cmp(&**other)
	}
}

impl Hash for GamePathBuf {
	fn hash<H: Hasher>(&self, state: &mut H) {
		(**self).hash(state);
	}
}

impl PartialEq<GamePath> for GamePathBuf {
	fn eq(&self, other: &GamePath) -> bool {
		**self == *other
	}
}

impl PartialEq<GamePathBuf> for GamePath {
	fn eq(&self, other: &GamePathBuf) -> bool {
		*self == **other
	}
}

impl PartialEq<&GamePath> for GamePathBuf {
	fn eq(&self, other: &&GamePath) -> bool {
		**self == **other
	}
}

impl fmt::Display for GamePath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl fmt::Debug for GamePath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(self.as_str(), f)
	}
}

impl fmt::Display for GamePathBuf {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&**self, f)
	}
}

impl fmt::Debug for GamePathBuf {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(&**self, f)
	}
}

/// Groups of distinct paths in `paths` that differ only in letter case,
/// each group and the groups themselves in byte order.
///
/// Identity stays case-sensitive: EU4's own case behavior is not verified,
/// so such paths remain separate entries. They are reported because a
/// case-insensitive filesystem (default NTFS and APFS) holds only one of
/// them, so writing both lets one replace the other, and because a family or
/// vanilla ancestor matched by exact spelling misses the other spelling.
/// Case is folded per character by uppercasing then lowercasing, so letters
/// with several lowercase forms (`Σ`, `σ`, `ς`) share one key.
pub fn case_only_path_groups<'a>(
	paths: impl IntoIterator<Item = &'a GamePath>,
) -> Vec<Vec<&'a GamePath>> {
	let mut by_folded = std::collections::BTreeMap::<String, Vec<&'a GamePath>>::new();
	for path in paths {
		by_folded
			.entry(
				path.as_str()
					.chars()
					.flat_map(char::to_uppercase)
					.flat_map(char::to_lowercase)
					.collect(),
			)
			.or_default()
			.push(path);
	}
	let mut groups = by_folded
		.into_values()
		.filter_map(|mut group| {
			group.sort();
			group.dedup();
			(group.len() > 1).then_some(group)
		})
		.collect::<Vec<_>>();
	groups.sort();
	groups
}

impl FromStr for GamePathBuf {
	type Err = GamePathError;

	fn from_str(text: &str) -> Result<Self, GamePathError> {
		Self::parse(text)
	}
}

impl TryFrom<String> for GamePathBuf {
	type Error = GamePathError;

	fn try_from(text: String) -> Result<Self, GamePathError> {
		match validate(&text) {
			Ok(()) => Ok(Self(RelativePathBuf::from(text))),
			Err(kind) => Err(GamePathError::new(text, kind)),
		}
	}
}

impl TryFrom<&str> for GamePathBuf {
	type Error = GamePathError;

	fn try_from(text: &str) -> Result<Self, GamePathError> {
		Self::parse(text)
	}
}

impl Serialize for GamePath {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl Serialize for GamePathBuf {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		(**self).serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for GamePathBuf {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = String::deserialize(deserializer)?;
		Self::try_from(text).map_err(serde::de::Error::custom)
	}
}

/// An rkyv `with` adapter that archives a [`GamePathBuf`] as its canonical
/// text. The archived form is the plain string a `String` field archives to,
/// byte for byte, and reading it back validates the text like every other
/// constructor, so a corrupt archive fails to deserialize instead of yielding
/// a path outside the rooted namespace.
pub struct AsGamePathText;

impl ArchiveWith<GamePathBuf> for AsGamePathText {
	type Archived = ArchivedString;
	type Resolver = StringResolver;

	fn resolve_with(field: &GamePathBuf, resolver: StringResolver, out: Place<ArchivedString>) {
		ArchivedString::resolve_from_str(field.as_str(), resolver, out);
	}
}

impl<S> SerializeWith<GamePathBuf, S> for AsGamePathText
where
	S: Fallible + ?Sized,
	S::Error: Source,
	str: SerializeUnsized<S>,
{
	fn serialize_with(field: &GamePathBuf, serializer: &mut S) -> Result<StringResolver, S::Error> {
		ArchivedString::serialize_from_str(field.as_str(), serializer)
	}
}

impl<D> DeserializeWith<ArchivedString, GamePathBuf, D> for AsGamePathText
where
	D: Fallible + ?Sized,
	D::Error: Source,
{
	fn deserialize_with(field: &ArchivedString, _: &mut D) -> Result<GamePathBuf, D::Error> {
		GamePathBuf::parse(field.as_str()).map_err(D::Error::new)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeMap;

	fn kind(text: &str) -> GamePathErrorKind {
		GamePath::new(text).expect_err(text).kind
	}

	fn reserved(component: &str, character: char) -> GamePathErrorKind {
		GamePathErrorKind::ReservedCharacter {
			component: component.to_string(),
			character,
		}
	}

	/// Paths stay distinct identities; only spellings that differ solely in
	/// case are grouped, in byte order, with repeats counted once.
	#[test]
	fn case_only_groups_collect_spellings_that_fold_together() {
		let paths = [
			"common/ideas/x.txt",
			"Common/ideas/x.txt",
			"common/ideas/y.txt",
			"common/ideas/x.txt",
			"events/Ä.txt",
			"events/ä.txt",
			"events/a.txt",
			"events/AΣ1.txt",
			"events/aσ1.txt",
		]
		.map(|text| GamePath::new(text).expect("valid"));
		let groups = case_only_path_groups(paths)
			.into_iter()
			.map(|group| group.into_iter().map(GamePath::as_str).collect::<Vec<_>>())
			.collect::<Vec<_>>();
		assert_eq!(
			groups,
			[
				vec!["Common/ideas/x.txt", "common/ideas/x.txt"],
				vec!["events/AΣ1.txt", "events/aσ1.txt"],
				vec!["events/Ä.txt", "events/ä.txt"],
			]
		);
		assert_ne!(paths[0], paths[1], "identity stays case-sensitive");
	}

	#[test]
	fn portable_text_round_trips_and_resolves_under_an_explicit_root() {
		let path = GamePath::new("common/ideas/00_ideas.txt").expect("valid");
		assert_eq!(path.as_str(), "common/ideas/00_ideas.txt");
		assert_eq!(path.file_name(), "00_ideas.txt");
		assert_eq!(path.extension(), Some("txt"));
		assert_eq!(
			path.to_path(Path::new("/game")),
			Path::new("/game")
				.join("common")
				.join("ideas")
				.join("00_ideas.txt")
		);
	}

	#[test]
	fn resolving_under_a_root_spells_the_path_as_join_does() {
		let path = GamePath::new("common/ideas/00_ideas.txt").expect("valid");
		let root = std::env::temp_dir().join("game");
		let mut trailing = root.clone().into_os_string();
		trailing.push(std::path::MAIN_SEPARATOR_STR);
		for root in [root.clone(), PathBuf::from(trailing)] {
			assert_eq!(
				path.to_path(&root).as_os_str(),
				root.join("common")
					.join("ideas")
					.join("00_ideas.txt")
					.as_os_str(),
				"{}",
				root.display()
			);
		}
		assert_eq!(
			path.to_path("").as_os_str(),
			Path::new("common")
				.join("ideas")
				.join("00_ideas.txt")
				.as_os_str(),
			"an empty root gives the host-relative spelling"
		);
	}

	#[test]
	fn text_outside_the_canonical_rooted_namespace_is_rejected() {
		assert_eq!(kind(""), GamePathErrorKind::Empty);
		assert_eq!(kind("/common/x.txt"), GamePathErrorKind::NotRelative);
		assert_eq!(kind("common//x.txt"), GamePathErrorKind::EmptyComponent);
		assert_eq!(kind("common/"), GamePathErrorKind::EmptyComponent);
		assert_eq!(kind("./common/x.txt"), GamePathErrorKind::CurrentComponent);
		assert_eq!(kind("common/../x.txt"), GamePathErrorKind::ParentComponent);
		assert_eq!(kind(".."), GamePathErrorKind::ParentComponent);
	}

	#[test]
	fn names_a_supported_host_parses_as_structure_are_rejected() {
		assert_eq!(kind(r"common/a\b.txt"), reserved(r"a\b.txt", '\\'));
		assert_eq!(kind(r"common\x.txt"), reserved(r"common\x.txt", '\\'));
		assert_eq!(kind("C:/common/x.txt"), reserved("C:", ':'));
		assert_eq!(kind("common/x.txt:stream"), reserved("x.txt:stream", ':'));
		assert_eq!(kind("common/a\0.txt"), reserved("a\0.txt", '\0'));
	}

	#[test]
	fn names_that_are_only_unusual_are_kept_exactly() {
		for text in [
			"history/countries/P09 - P609.txt",
			"localisation/Ölände_l_english.yml",
			"common/Ideas/Mixed Case?.txt",
		] {
			assert_eq!(GamePath::new(text).expect(text).as_str(), text);
		}
	}

	#[cfg(unix)]
	#[test]
	fn a_literal_backslash_in_a_unix_filename_is_rejected_not_folded_into_a_directory() {
		let nested = GamePathBuf::from_native_relative(Path::new("common/a/b.txt"))
			.expect("nested path is valid");
		assert_eq!(nested.as_str(), "common/a/b.txt");
		let literal = GamePathBuf::from_native_relative(Path::new(r"common/a\b.txt"))
			.expect_err("a literal backslash has no portable identity");
		assert_eq!(literal.kind, reserved(r"a\b.txt", '\\'));
		assert_eq!(literal.input, r"common/a\b.txt");
	}

	#[cfg(windows)]
	#[test]
	fn windows_separators_split_native_components() {
		let path = GamePathBuf::from_native_relative(Path::new(r"common\ideas\x.txt"))
			.expect("native Windows separators are separators");
		assert_eq!(path.as_str(), "common/ideas/x.txt");
	}

	#[cfg(unix)]
	#[test]
	fn distinct_non_utf8_names_are_rejected_instead_of_sharing_a_replacement_name() {
		// In-memory paths: some filesystems refuse to create these names.
		use std::ffi::OsString;
		use std::os::unix::ffi::OsStringExt;

		for bytes in [b"common/\xff.txt".to_vec(), b"common/\xfe.txt".to_vec()] {
			let path = PathBuf::from(OsString::from_vec(bytes));
			let error = GamePathBuf::from_native_relative(&path).expect_err("non-UTF-8 name");
			assert_eq!(error.kind, GamePathErrorKind::NonUtf8);
		}
	}

	#[test]
	fn native_absolute_and_escaping_paths_are_rejected() {
		let absolute = std::env::temp_dir().join("x.txt");
		assert_eq!(
			GamePathBuf::from_native_relative(&absolute)
				.expect_err("absolute")
				.kind,
			GamePathErrorKind::NotRelative
		);
		assert_eq!(
			GamePathBuf::from_native_relative(Path::new("../x.txt"))
				.expect_err("parent")
				.kind,
			GamePathErrorKind::ParentComponent
		);
	}

	#[test]
	fn physical_paths_are_taken_against_their_root_and_resolved_back_to_the_same_file() {
		let root = std::env::temp_dir().join("mod");
		let physical = root.join("common").join("ideas").join("x.txt");
		let path = GamePathBuf::from_physical(&root, &physical).expect("under root");
		assert_eq!(path.as_str(), "common/ideas/x.txt");
		assert_eq!(path.to_path(&root), physical);

		let outside = std::env::temp_dir().join("other").join("x.txt");
		assert_eq!(
			GamePathBuf::from_physical(&root, &outside)
				.expect_err("outside")
				.kind,
			GamePathErrorKind::OutsideRoot
		);
	}

	#[test]
	fn serde_writes_plain_text_and_validates_when_reading() {
		let path = GamePathBuf::parse("events/Flavor.txt").expect("valid");
		let encoded = serde_json::to_string(&path).expect("serialize");
		assert_eq!(encoded, r#""events/Flavor.txt""#);
		let decoded: GamePathBuf = serde_json::from_str(&encoded).expect("deserialize");
		assert_eq!(decoded, path);

		for rejected in [
			r#""../outside.txt""#,
			r#""/events/x.txt""#,
			r#""events/a\\b.txt""#,
			r#""""#,
		] {
			let error = serde_json::from_str::<GamePathBuf>(rejected).expect_err(rejected);
			assert!(
				error.to_string().contains("invalid game path"),
				"{rejected}: {error}"
			);
		}
	}

	#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
	struct TextRecord {
		path: String,
		line: u32,
	}

	#[derive(Debug, PartialEq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
	struct TypedRecord {
		#[rkyv(with = AsGamePathText)]
		path: GamePathBuf,
		line: u32,
	}

	#[test]
	fn archived_game_paths_are_the_bytes_of_an_archived_string() {
		// Short and long texts cover the inline and out-of-line string forms.
		for text in [
			"events/a.txt",
			"common/scripted_effects/00_a_rather_long_name_that_is_stored_out_of_line.txt",
		] {
			let as_text = rkyv::to_bytes::<rkyv::rancor::Error>(&TextRecord {
				path: text.to_string(),
				line: 7,
			})
			.expect("archive text");
			let typed = TypedRecord {
				path: GamePathBuf::parse(text).expect("valid"),
				line: 7,
			};
			let as_path = rkyv::to_bytes::<rkyv::rancor::Error>(&typed).expect("archive path");
			assert_eq!(as_text.as_slice(), as_path.as_slice(), "{text}");
			let decoded =
				rkyv::from_bytes::<TypedRecord, rkyv::rancor::Error>(&as_path).expect("decode");
			assert_eq!(decoded, typed);
		}
	}

	#[test]
	fn archived_text_that_is_not_a_game_path_fails_to_deserialize() {
		for text in [
			r"events\a.txt",
			"../a.txt",
			"/events/a.txt",
			"events//a.txt",
			"",
		] {
			let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&TextRecord {
				path: text.to_string(),
				line: 1,
			})
			.expect("archive text");
			let error = rkyv::from_bytes::<TypedRecord, rkyv::rancor::Error>(&bytes)
				.expect_err(text)
				.to_string();
			assert!(
				error.contains(&format!("invalid game path `{text}`")),
				"{text}: {error}"
			);
		}
	}

	#[test]
	fn ordering_is_the_byte_order_of_the_canonical_text() {
		let dash = GamePath::new("common/a-b.txt").expect("valid");
		let nested = GamePath::new("common/a/b.txt").expect("valid");
		assert!(dash < nested);
		assert!(dash.as_relative_path() > nested.as_relative_path());
	}

	#[test]
	fn owned_keys_are_found_by_borrowed_paths() {
		let mut map = BTreeMap::new();
		map.insert(GamePathBuf::parse("events/a.txt").expect("valid"), 1);
		assert_eq!(
			map.get(GamePath::new("events/a.txt").expect("valid")),
			Some(&1)
		);
	}

	#[test]
	fn derived_paths_stay_within_the_rooted_namespace() {
		let path = GamePath::new("common/ideas/x.txt").expect("valid");
		let common = GamePath::new("common").expect("valid");
		assert_eq!(path.parent().map(GamePath::as_str), Some("common/ideas"));
		assert_eq!(common.parent(), None);
		assert_eq!(
			path.strip_prefix(common).map(GamePath::as_str),
			Some("ideas/x.txt")
		);
		assert_eq!(common.strip_prefix(common), None);
		assert!(path.starts_with(common));
		assert!(
			!GamePath::new("commonx/y.txt")
				.expect("valid")
				.starts_with(common)
		);
		let ideas = GamePath::new("common/ideas").expect("valid");
		assert!(path.is_child_of(ideas));
		assert!(!path.is_child_of(common), "a nested file is not a child");
		assert!(!ideas.is_child_of(ideas));
		assert!(!common.is_child_of(common));
		assert!(path.is_inside(&["common", "ideas"], str::eq));
		assert!(!path.is_inside(&["common", "Ideas"], str::eq));
		assert!(path.is_inside(&["COMMON", "Ideas"], str::eq_ignore_ascii_case));
		assert!(!path.is_inside(&["common", "ideas", "x.txt"], str::eq));
		assert!(!path.is_inside(&["common", "idea"], str::eq));
		assert!(!common.is_inside(&["common"], str::eq));
		assert_eq!(
			common.join(GamePath::new("ideas/x.txt").expect("valid")),
			GamePathBuf::parse("common/ideas/x.txt").expect("valid")
		);
	}
}
