//! Live EU4 game-state feature plane over the shared runtime.
//!
//! Split of concerns per the runtime architecture: the raw memory
//! access, the verified layout, and the typed views live in
//! [`foch_runtime::dataplane`] (consumed here with
//! `default-features = false`); this crate adds only what is ours —
//! the still-unverified [`candidates`] offsets, the [`view`] reads
//! that turn them into plane inputs, and [`plane`]'s 29-column
//! semantic contract shared by the simulator, the offline tooling,
//! and the in-game plugin. `foch-eu4-decide` (the decision plane)
//! stays free of any game-memory knowledge.

pub mod candidates;
pub mod plane;
pub mod view;

pub use plane::{COLUMNS, NODE, RULES, State};
