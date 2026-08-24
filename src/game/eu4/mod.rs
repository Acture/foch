pub mod analysis;
pub mod base;
pub mod content;
pub mod cwt;
mod profile;
pub mod scope;
pub mod script;
pub mod text;

pub use profile::Eu4;

#[cfg(test)]
mod cwt_container_scope_diff_tests;
#[cfg(test)]
mod cwt_scope_diff_tests;
