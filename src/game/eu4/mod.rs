pub mod analysis;
pub mod base;
pub mod coercion;
pub mod content;
pub(crate) mod cwt;
pub mod editor;
mod profile;
pub mod scope;
pub mod script;
pub mod text;

pub use cwt::{
	CWT_SCHEMA_OVERRIDE_ENV, EMBEDDED_CWT_SCHEMA_ID, active_cwt_schema_id, cwt_schema_override,
};
pub use profile::Eu4;

#[cfg(test)]
mod cwt_container_scope_diff_tests;
#[cfg(test)]
mod cwt_scope_diff_tests;
