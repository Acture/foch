pub(crate) mod analysis;
pub(crate) mod documents;
pub(crate) mod eu4_builtin;
pub(crate) mod localisation;
pub(crate) mod param_contracts;
pub(crate) mod parser;
pub(crate) mod report;
pub(crate) mod rules;
pub(crate) mod run;
pub(crate) mod semantic_index;

pub use run::{run_checks, run_checks_with_options};
