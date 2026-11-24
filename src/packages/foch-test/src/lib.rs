//! In-game tests for EU4 mods, modeled on pytest and Rust's test harness.
//!
//! The library computes; its caller does input and output. It never walks
//! directories, reads configuration, launches processes or prints. A run is
//! a sequence of plain-data stages, each testable on its own:
//!
//! ```text
//! SourceFile ─collect→ Collection ─expand→ Expanded ─lint→ LintResult
//!                                              └──────group──→ Plan
//! Plan ─compile→ Bundle ─(caller runs the runner)→ RunArtifacts ─judge→ CaseResult
//! CaseResult* ─→ Report
//! ```
//!
//! [`judge`](judge::judge) is the single place that decides whether a case
//! passed. Sharing a game session between cases is an optimization decided by
//! [`group`](plan::group) from each case's declared needs and its estimated
//! footprint; a case's verdict must not depend on it.
//!
//! [`interpret`](interpret::interpret) is the offline layer: the same plan
//! executed against a small model organized by
//! [`interpret::SemanticsCatalog`], whose embedded catalog is `unpinned` —
//! results record that, and a `pinned` catalog refuses primitives without
//! decompiled handler evidence. Its results are separate types and never
//! game evidence.

pub mod collect;
pub mod compile;
pub mod diagnostic;
pub mod interpret;
pub mod judge;
pub mod lint;
pub mod model;
pub mod plan;
pub mod report;
pub mod select;
pub mod source;

pub use collect::{Collection, collect};
pub use compile::{Bundle, RunContext, RunnerCapabilities, check_capabilities, compile};
pub use diagnostic::{Diagnostic, DiagnosticCode, Severity, blocks_run};
pub use interpret::{
	ContentLayer, CountrySlice, EvidenceStatus, HistoryTimeline, InterpretOptions, Interpretation,
	InterpretedCase, LayerFile, Observation, Observed, Outcome, ProvinceSeed, ProvinceSlice,
	SeedFacts, SeededCountry, SemanticsCatalog, SubjectTypeSet, SubjectWarPolicy, fold_history,
	interpret, overlay_decisions, overlay_history, overlay_subject_types,
};
pub use judge::{CaseResult, RunArtifacts, RunnerExit, Status, Timing, judge, reconcile};
pub use lint::{Footprint, LintResult, ProjectFacts, lint};
pub use model::Tag;
pub use plan::{ExpandOptions, Expanded, GroupOptions, Plan, expand, group};
pub use report::Report;
pub use select::Selector;
pub use source::{RelPath, SourceFile, SourceKind};

use foch::model::GamePath;

/// A parser path label for internal parsing.
pub(crate) fn game_path(text: &str) -> &GamePath {
	GamePath::new(text).expect("valid game path label")
}
