mod binding_baseline;
mod compiled;

use std::fs;

use super::{CwtSchema, CwtSource};
use crate::game::schema::cache::CwtLoadStatus;

fn binding_fixture_dir() -> std::path::PathBuf {
	std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("src/game/schema/tests/fixtures/binding")
}

#[test]
fn concrete_schema_load_exposes_cached_facts() {
	let cache = tempfile::tempdir().expect("create schema cache");
	let source = || CwtSource::UserProvided {
		path: binding_fixture_dir(),
	};
	let first = CwtSchema::load_with_cache(&binding_fixture_dir(), source(), Some(cache.path()))
		.expect("compile schema facts");
	assert_eq!(first.cache_status(), CwtLoadStatus::CompiledFromSource);
	assert!(first.facts().root_count() > 0);
	assert!(first.cache_path().is_some_and(std::path::Path::is_file));
	assert!(first.timings().source_compile.is_some());

	let second = CwtSchema::load_with_cache(&binding_fixture_dir(), source(), Some(cache.path()))
		.expect("load cached schema facts");
	assert_eq!(second.cache_status(), CwtLoadStatus::CacheHit);
	assert_eq!(second.source_id(), first.source_id());
	assert_eq!(second.facts().root_count(), first.facts().root_count());
	assert!(second.timings().cache_read.is_some());
}

#[test]
fn shared_schema_load_does_not_initialize_game_scopes() {
	let schema = CwtSchema::load_with_cache(
		&binding_fixture_dir(),
		CwtSource::UserProvided {
			path: binding_fixture_dir(),
		},
		None,
	)
	.expect("load schema without cache");

	assert!(schema.facts().root_count() > 0);
	assert!(schema.cache_path().is_none());
}

#[test]
fn fixture_copy_remains_owned_by_the_shared_schema_module() {
	let fixture = binding_fixture_dir().join("events.cwt");
	let contents = fs::read_to_string(fixture).expect("read migrated CWT fixture");
	assert!(contents.contains("type[event]"));
}
