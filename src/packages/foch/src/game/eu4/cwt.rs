use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::script::is_clausewitz_date_key;
use super::script::parser::{AstStatement, AstValue, ScalarValue, unquoted_scalar};
use crate::game::schema::CwtSchema;
use crate::game::schema::bind::{
	SchemaDialect, SchemaNode, SchemaScalar, SchemaValue, ScopeKeyword,
};
use crate::game::schema::query::{CompiledAliasCategory, CwtQuery};

pub mod merge;

/// The rule pack `build.rs` compiled from the package's `vendor/cwtools-eu4-config`
/// and, after it, the supplementary rules in `src/game/eu4/cwt_supplement`.
static EMBEDDED_RULE_PACK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cwt-rules.pack"));

/// `cwt_schema_id` of the rule pack compiled into this binary.
pub const EMBEDDED_CWT_SCHEMA_ID: &str = env!("FOCH_CWT_SCHEMA_ID");

/// Maintainer override: compile the schema from this directory instead of
/// using the embedded pack. The directory replaces both the vendored rules and
/// the supplement. It changes output bytes, so a directory that does
/// not load is a fatal error rather than a quiet fallback. It is compiled in
/// every process and never cached, so an edit to the compiler shows at once.
pub const CWT_SCHEMA_OVERRIDE_ENV: &str = "FOCH_CWTOOLS_SCHEMA_DIR";

/// The directory [`CWT_SCHEMA_OVERRIDE_ENV`] names; set but empty is unset.
pub fn cwt_schema_override() -> Option<PathBuf> {
	override_dir(std::env::var_os(CWT_SCHEMA_OVERRIDE_ENV))
}

fn override_dir(value: Option<OsString>) -> Option<PathBuf> {
	value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Loads the active EU4 schema without changing EU4's global base-scope state.
pub(crate) fn rule_engine() -> &'static CwtQuery {
	active_schema_slot().facts()
}

pub(crate) fn active_schema() -> Arc<CwtSchema> {
	Arc::clone(active_schema_slot())
}

/// `cwt_schema_id` of the schema this process uses: the embedded pack unless
/// [`CWT_SCHEMA_OVERRIDE_ENV`] names another directory.
pub fn active_cwt_schema_id() -> &'static str {
	static ID: OnceLock<String> = OnceLock::new();
	ID.get_or_init(|| active_schema_slot().source_id().to_hex())
}

fn active_schema_slot() -> &'static Arc<CwtSchema> {
	static EU4_SCHEMA: OnceLock<Arc<CwtSchema>> = OnceLock::new();
	EU4_SCHEMA.get_or_init(|| Arc::new(load_schema(cwt_schema_override())))
}

fn load_schema(override_root: Option<PathBuf>) -> CwtSchema {
	match override_root {
		None => CwtSchema::from_compiled_bytes(EMBEDDED_RULE_PACK)
			.expect("build.rs embeds a rule pack this binary's compiler wrote"),
		Some(root) => CwtSchema::load_with_cache(&root, None).unwrap_or_else(|error| {
			panic!(
				"{CWT_SCHEMA_OVERRIDE_ENV}=`{}` is not a loadable CWT schema: {error}",
				root.display()
			)
		}),
	}
}

fn cwt_marker_parts(text: &str) -> Option<(&str, &str)> {
	let (head, rest) = text.split_once('[')?;
	Some((head, rest.strip_suffix(']')?))
}

pub fn is_country_file_reference(value: &str) -> bool {
	value.starts_with("countries/") && value.ends_with(".txt")
}

pub fn is_country_tag_text(value: &str) -> bool {
	is_country_tag_token(value)
}

/// EU4 country tags are exactly three ASCII characters: the first is an
/// uppercase letter and the remaining characters are uppercase letters or
/// ASCII digits. This matches both vanilla tags (FRA, ENG, KOR) and the
/// digit-bearing tags commonly used by mods (X3E, Y1D, K01).
fn is_country_tag_token(token: &str) -> bool {
	if token.len() != 3 {
		return false;
	}
	let mut chars = token.chars();
	let first = chars.next().unwrap();
	if !first.is_ascii_uppercase() {
		return false;
	}
	chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

pub fn is_province_id_text(value: &str) -> bool {
	value.parse::<u32>().is_ok_and(|id| id > 0)
}

/// The triggers the schema names, each once, sorted: the engine's, and the
/// game's scripted triggers it declares under their own names.
pub fn schema_trigger_names() -> &'static [String] {
	static NAMES: OnceLock<Vec<String>> = OnceLock::new();
	NAMES.get_or_init(|| schema_alias_names(&CompiledAliasCategory::Trigger))
}

/// The effects the schema names, each once, sorted.
pub fn schema_effect_names() -> &'static [String] {
	static NAMES: OnceLock<Vec<String>> = OnceLock::new();
	NAMES.get_or_init(|| schema_alias_names(&CompiledAliasCategory::Effect))
}

/// The scope links the schema names (`owner`, `capital_scope`), which switch
/// scope among triggers and effects alike.
pub fn schema_link_names() -> &'static [String] {
	static NAMES: OnceLock<Vec<String>> = OnceLock::new();
	NAMES.get_or_init(|| {
		let mut names = rule_engine()
			.links()
			.iter()
			.map(|link| link.name.clone())
			.collect::<Vec<_>>();
		names.sort();
		names.dedup();
		names
	})
}

/// The names of a category's aliases that name one key, not a set of them
/// (`<scripted_effect>`, `enum[country_tags]`).
fn schema_alias_names(category: &CompiledAliasCategory) -> Vec<String> {
	let mut names = rule_engine()
		.aliases()
		.iter()
		.filter(|alias| &alias.category == category)
		.filter(|alias| !alias.name.contains(['<', '[']))
		.map(|alias| alias.name.clone())
		.collect::<Vec<_>>();
	names.sort();
	names.dedup();
	names
}

/// Whether the schema declares `key` itself as a trigger.
pub(crate) fn is_schema_trigger(key: &str) -> bool {
	rule_engine()
		.alias(CompiledAliasCategory::Trigger, &key.to_ascii_lowercase())
		.is_some()
}

/// Whether the schema declares `key` itself as an effect.
pub(crate) fn is_schema_effect(key: &str) -> bool {
	rule_engine()
		.alias(CompiledAliasCategory::Effect, &key.to_ascii_lowercase())
		.is_some()
}

/// What the names a set holds are in script, as `(trigger, effect)`: the
/// definitions of a schema type (`trade_good`) or the values of a complex enum
/// (`enum[country_tags]`). The schema declares `alias[trigger:<trade_good>] =
/// int`, so the name of every trade good, the game's or a mod's, is a
/// trigger, and `alias[effect:enum[country_tags]] = { ... }`, so every tag is
/// an effect.
pub(crate) fn schema_names_are_keys(set: &str) -> (bool, bool) {
	let (trigger, effect, _) = schema_key_set(set);
	(trigger, effect)
}

/// The scope a name a set holds switches to where it is written as a key
/// (`FRA = { ... }` to a country, `finland_area = { ... }` to its
/// provinces), when the schema says one.
pub(crate) fn schema_names_push_scope(set: &str) -> Option<&'static str> {
	schema_key_set(set).2
}

/// Whether a set's names are trigger keys and effect keys, and the scope
/// those keys push.
fn schema_key_set(set: &str) -> (bool, bool, Option<&'static str>) {
	type KeySets = std::collections::HashMap<String, (bool, bool, Option<&'static str>)>;
	static KEY_SETS: OnceLock<KeySets> = OnceLock::new();
	KEY_SETS
		.get_or_init(|| {
			let mut sets = KeySets::new();
			for alias in rule_engine().aliases() {
				let set = if let Some(type_name) = alias
					.name
					.strip_prefix('<')
					.and_then(|name| name.strip_suffix('>'))
				{
					// `<area.used>` names the areas of one subtype; any
					// area's name is taken.
					type_name.split('.').next().unwrap_or(type_name)
				} else if alias.name.starts_with("enum[")
					&& cwt_marker_parts(&alias.name)
						.is_some_and(|(_, name)| rule_engine().complex_enum(name).is_some())
				{
					alias.name.as_str()
				} else {
					continue;
				};
				let entry = sets.entry(set.to_string()).or_default();
				match alias.category {
					CompiledAliasCategory::Trigger => entry.0 = true,
					CompiledAliasCategory::Effect => entry.1 = true,
					_ => continue,
				}
				if entry.2.is_none() {
					entry.2 = alias.attributes.push_scope.as_deref();
				}
			}
			sets
		})
		.get(set)
		.copied()
		.unwrap_or_default()
}

/// What EU4 script means where the rule files leave the reader to know.
pub(crate) struct Eu4Dialect;

impl SchemaDialect for Eu4Dialect {
	/// CWTools' EU4 one-to-one scopes: scope switches and logical blocks the
	/// schema accepts through its `scope_field` alias instead of a named
	/// alias.
	fn scope_keyword(&self, key: &str) -> Option<ScopeKeyword> {
		const KEYWORDS: &[(&str, ScopeKeyword)] = &[
			("THIS", ScopeKeyword::This),
			("ROOT", ScopeKeyword::Root),
			("FROM", ScopeKeyword::From),
			("PREV", ScopeKeyword::Prev),
			("PREV_PREV", ScopeKeyword::PrevPrev),
			("AND", ScopeKeyword::And),
			("OR", ScopeKeyword::Or),
			("NOT", ScopeKeyword::Not),
			("hidden_effect", ScopeKeyword::HiddenEffect),
			("hidden_trigger", ScopeKeyword::HiddenTrigger),
		];
		KEYWORDS
			.iter()
			.find(|(keyword, _)| keyword.eq_ignore_ascii_case(key))
			.map(|(_, keyword)| *keyword)
	}

	fn is_event_target(&self, text: &str) -> bool {
		text.strip_prefix("event_target:")
			.is_some_and(|name| !name.is_empty())
	}

	fn is_date_key(&self, key: &str) -> bool {
		is_clausewitz_date_key(key)
	}

	fn is_parameter_key(&self, key: &str) -> bool {
		key.len() > 2 && key.starts_with('$') && key.ends_with('$')
	}

	fn holds_parameter(&self, text: &str) -> bool {
		text.contains('$')
	}

	fn reads_as_number(&self, text: &str) -> bool {
		matches!(unquoted_scalar(text), ScalarValue::Number(_))
	}

	fn is_runtime_value(&self, complex_enum: &str, text: &str) -> bool {
		complex_enum == "country_tags" && is_dynamic_country_tag(text)
	}
}

/// Tags the game assigns at runtime to countries no file defines: colonial
/// nations `C00`–`C74`, custom nations `D00`–`D74`, estate disaster
/// countries `E00`–`E49`, federations `F00`–`F19`, client states
/// `K00`–`K99`, observers `O00`–`O09`, spectators `S00`–`S09` and trading
/// cities `T00`–`T74` (EU4 wiki, Countries).
pub(crate) fn is_dynamic_country_tag(text: &str) -> bool {
	let [prefix, tens @ b'0'..=b'9', ones @ b'0'..=b'9'] = text.as_bytes() else {
		return false;
	};
	let count = match prefix {
		b'C' | b'D' | b'T' => 75,
		b'E' => 50,
		b'F' => 20,
		b'K' => 100,
		b'O' | b'S' => 10,
		_ => return false,
	};
	u32::from(tens - b'0') * 10 + u32::from(ones - b'0') < count
}

impl SchemaNode for AstStatement {
	type Value = AstValue;

	fn assignment(&self) -> Option<(&str, &AstValue)> {
		match self {
			AstStatement::Assignment { key, value, .. } => Some((key, value)),
			AstStatement::Item { .. } | AstStatement::Comment { .. } => None,
		}
	}

	fn item(&self) -> Option<&AstValue> {
		match self {
			AstStatement::Item { value, .. } => Some(value),
			AstStatement::Assignment { .. } | AstStatement::Comment { .. } => None,
		}
	}
}

impl SchemaValue for AstValue {
	type Node = AstStatement;

	fn scalar(&self) -> Option<SchemaScalar<'_>> {
		let AstValue::Scalar { value, .. } = self else {
			return None;
		};
		Some(match value {
			ScalarValue::Identifier(text) => SchemaScalar::Identifier(text),
			ScalarValue::String(text) => SchemaScalar::String(text),
			ScalarValue::Number(text) => SchemaScalar::Number(text),
			ScalarValue::Bool(value) => SchemaScalar::Bool(*value),
		})
	}

	fn block(&self) -> Option<&[AstStatement]> {
		match self {
			AstValue::Block { items, .. } => Some(items),
			AstValue::Scalar { .. } => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use std::ffi::OsString;

	use super::{CwtSchema, EMBEDDED_CWT_SCHEMA_ID, EMBEDDED_RULE_PACK, load_schema, override_dir};
	use crate::game::schema::CwtLoadStatus;
	use crate::game::schema::query::CompiledRulePack;
	use crate::game::schema::source::cwt_files;
	use crate::game::schema::source::{
		SchemaPack, cwt_schema_id_from_dir, cwt_schema_id_from_dirs, normalize_line_endings,
	};

	/// The directories `build.rs` compiles, in its order.
	fn embedded_schema_dirs() -> [PathBuf; 2] {
		let package = Path::new(env!("CARGO_MANIFEST_DIR"));
		[
			package.join("vendor/cwtools-eu4-config"),
			package.join("src/game/eu4/cwt_supplement"),
		]
	}

	#[test]
	fn default_schema_is_the_embedded_pack_of_the_vendored_config_and_supplement() {
		let schema = load_schema(None);
		let [vendor, supplement] = embedded_schema_dirs();
		let embedded_id =
			cwt_schema_id_from_dirs(&[&vendor, &supplement]).expect("hash embedded CWT");

		assert_eq!(schema.cache_status(), CwtLoadStatus::Embedded);
		assert_eq!(schema.source_id(), &embedded_id);
		assert_eq!(schema.source_id().to_hex(), EMBEDDED_CWT_SCHEMA_ID);
		assert!(schema.facts().root_count() > 0);
		assert!(schema.facts().alias_count() > 0);
	}

	#[test]
	fn compiling_the_vendored_config_and_supplement_reproduces_the_embedded_bytes() {
		// build.rs compiled the embedded pack in another process, so equality
		// also proves the encoding does not depend on per-process hash seeds.
		let [vendor, supplement] = embedded_schema_dirs();
		let pack =
			SchemaPack::load_from_dirs(&[&vendor, &supplement]).expect("compile embedded CWT");
		let bytes = CompiledRulePack::from_schema_pack(&pack)
			.to_bytes()
			.expect("encode compiled CWT");

		assert!(bytes == EMBEDDED_RULE_PACK);
	}

	#[test]
	fn a_crlf_checkout_compiles_to_the_embedded_bytes() {
		// Windows checkouts convert the rules' line endings; their binaries
		// must embed the same pack the same `cwt_schema_id` names elsewhere.
		let copies = embedded_schema_dirs().map(|source_root| {
			let crlf = tempfile::tempdir().expect("create CRLF schema copy");
			for source in cwt_files(&source_root).expect("list embedded CWT") {
				let target = crlf
					.path()
					.join(source.strip_prefix(&source_root).expect("embedded path"));
				let text = std::fs::read(&source).expect("read embedded CWT");
				let lf = normalize_line_endings(&text);
				let mut converted = Vec::with_capacity(lf.len() * 11 / 10);
				for &byte in lf.iter() {
					if byte == b'\n' {
						converted.push(b'\r');
					}
					converted.push(byte);
				}
				std::fs::create_dir_all(target.parent().expect("CWT parent"))
					.expect("create CWT parent");
				std::fs::write(target, converted).expect("write CRLF CWT");
			}
			crlf
		});

		let pack = SchemaPack::load_from_dirs(&[copies[0].path(), copies[1].path()])
			.expect("compile CRLF CWT");
		let bytes = CompiledRulePack::from_schema_pack(&pack)
			.to_bytes()
			.expect("encode compiled CWT");

		assert!(bytes == EMBEDDED_RULE_PACK);
	}

	#[test]
	fn an_override_compiles_its_own_directory() {
		let root =
			Path::new(env!("CARGO_MANIFEST_DIR")).join("src/game/schema/tests/fixtures/binding");
		let schema = load_schema(Some(root.clone()));

		assert_eq!(schema.cache_status(), CwtLoadStatus::CompiledFromSource);
		assert!(schema.cache_path().is_none());
		assert_eq!(
			schema.source_id(),
			&cwt_schema_id_from_dir(&root).expect("hash fixture CWT")
		);
		assert_ne!(schema.source_id().to_hex(), EMBEDDED_CWT_SCHEMA_ID);
	}

	#[test]
	#[should_panic(expected = "FOCH_CWTOOLS_SCHEMA_DIR")]
	fn an_override_without_rule_files_is_fatal() {
		let empty = tempfile::tempdir().expect("create empty schema directory");
		load_schema(Some(empty.path().to_path_buf()));
	}

	#[test]
	fn an_empty_override_variable_is_unset() {
		assert_eq!(override_dir(None), None);
		assert_eq!(override_dir(Some(OsString::new())), None);
		assert_eq!(
			override_dir(Some(OsString::from("/maintainer/cwt"))),
			Some(PathBuf::from("/maintainer/cwt"))
		);
	}

	/// A user schema can declare a rule path that is not a game path. It no
	/// longer loads, and the report says which schema, which file and which
	/// declaration instead of switching schema features off silently.
	#[test]
	#[should_panic(expected = "FOCH_CWTOOLS_SCHEMA_DIR")]
	fn a_user_schema_that_fails_to_load_is_reported_with_its_cause() {
		let root = tempfile::tempdir().expect("create schema dir");
		let file = root.path().join("ideas.cwt");
		std::fs::write(
			&file,
			"types = { type[idea_group] = { path = \"game/common/ideas/\" } }\n",
		)
		.expect("write schema");
		let error = CwtSchema::load_with_cache(root.path(), None)
			.err()
			.expect("a trailing `/` is not a game path");
		let report: String = error.to_string();

		for expected in [
			root.path().display().to_string(),
			file.display().to_string(),
			"`idea_group` declares an invalid path".to_string(),
		] {
			assert!(report.contains(&expected), "{expected} not in {report}");
		}
		load_schema(Some(root.path().to_path_buf()));
	}
}
