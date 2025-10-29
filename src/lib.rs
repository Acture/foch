use std::path::PathBuf;

mod config;
mod filesystem;
mod game;
mod modus;
mod parsing;
mod path;
mod utils;
mod cli;

pub use modus::merge::merge_root;

fn get_test_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}
fn get_corpus_path() -> PathBuf {
	get_test_dir().join("corpus")
}


