//! Parser-independent merge algorithms and shared merge models.

// The engine still lives in a separate package and consumes this module directly. Once the
// engine fold lands, this implementation detail can return to crate-private visibility.
#[doc(hidden)]
pub mod kernel;
