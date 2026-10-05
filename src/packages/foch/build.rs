//! Compiles the vendored CWTools EU4 config into the rule pack `foch` embeds.
//!
//! The schema decides output bytes: the merge rewrites a number into EU4's
//! representation only where the schema types its field. Loading the schema
//! from a directory at run time made the same binary write different bytes on
//! a machine without that directory, so the pack is compiled here and carried
//! inside the binary. A missing or empty vendored directory is therefore a
//! build error, never a binary without a schema.
//!
//! The compiler is the library's own `src/game/schema`, included by path: a
//! build script cannot link the crate it builds. The same validated game-path
//! types are included so build-time compilation and runtime rule matching
//! use identical boundaries.

#![allow(dead_code)]

#[path = "src/game/schema/compile.rs"]
mod compile;
#[path = "src/game/schema/error.rs"]
mod error;
#[path = "src/model/game_path.rs"]
mod model;
#[path = "src/game/schema/query.rs"]
mod query;
#[path = "src/game/schema/rule_path.rs"]
mod rule_path;
#[path = "src/game/schema/source.rs"]
mod source;
#[path = "src/game/schema/syntax.rs"]
mod syntax;

use std::env;
use std::fs;
use std::path::PathBuf;

use query::CompiledRulePack;
use source::SchemaPack;

const SCHEMA_DIR: &str = "../../../vendor/cwtools-eu4-config";
const SUBMODULE_HINT: &str =
	"git submodule update --init src/packages/tree-sitter-paradox vendor/cwtools-eu4-config";

fn main() {
	// The compiler modules need no entry: rustc records them as this script's
	// sources, so editing one rebuilds and reruns it.
	println!("cargo::rerun-if-changed={SCHEMA_DIR}");
	let manifest_dir =
		PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
	let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));

	let schema = SchemaPack::load_from_dir(&manifest_dir.join(SCHEMA_DIR)).unwrap_or_else(|error| {
		panic!(
			"foch embeds the EU4 CWT rule pack compiled from `{SCHEMA_DIR}`, which could not be compiled: {error}\n\
			 If the submodule is not checked out, run: {SUBMODULE_HINT}"
		)
	});
	let pack = CompiledRulePack::from_schema_pack(&schema);
	let bytes = pack
		.to_bytes()
		.unwrap_or_else(|error| panic!("encode the compiled CWT rule pack: {error}"));
	fs::write(out_dir.join("cwt-rules.pack"), bytes)
		.unwrap_or_else(|error| panic!("write the compiled CWT rule pack: {error}"));
	println!("cargo::rustc-env=FOCH_CWT_SCHEMA_ID={}", schema.id.to_hex());
}
