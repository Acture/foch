//! Foch annotations: extra meaning carried in EU4 script comments.
//!
//! An annotation such as `#test(time=1444.11.11, tag=SWE)` is an ordinary
//! comment to the game, so an annotated mod stays a valid mod that EU4 loads
//! without any Foch step. This crate only reads annotations; it never changes
//! script that the game loads. Extensions that need compiling into native
//! script belong elsewhere.
//!
//! Each annotation is described by an [`AnnotationSchema`](schema::AnnotationSchema),
//! plain data that drives validation, completion, hover and help alike.
//! Consumers such as the test runner give annotations their meaning; editors
//! need only this crate.

pub mod builtin;
pub mod diagnostic;
pub mod editor;
pub mod extract;
pub mod schema;
pub mod source;
pub mod value;

pub use diagnostic::{Code, Diagnostic, Severity};
pub use extract::{Annotation, Applied, Attachment, Extraction, Target, extract, extract_parsed};
pub use schema::{AnnotationSchema, Registry};
