//! Problems found before a game is launched.

use crate::source::SourceSpan;
use serde::{Deserialize, Serialize};
use std::fmt;

pub use foch_annotation::Severity;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiagnosticCode {
	/// The script itself does not parse.
	Parse,
	/// A malformed annotation, test block or fixture.
	Annotation,
	/// A comment that looks like an annotation but uses an unknown name.
	UnknownAnnotation,
	/// An annotation that is not followed by what it applies to.
	OrphanAnnotation,
	/// An annotation inside a definition rather than before it.
	NestedAnnotation,
	DuplicateCase,
	DuplicateFixture,
	UnknownFixture,
	FixtureCycle,
	/// The target cannot be invoked by the framework.
	UnsupportedTarget,
	TooManyCases,
	InvalidMod,
	UnknownTag,
	UnknownEvent,
	/// A trigger used where an effect is required.
	TriggerAsEffect,
	/// An effect used where a condition is required.
	EffectAsTrigger,
}

/// `bypassable` separates two kinds of findings. A malformed test cannot be
/// compiled, so it always blocks. A finding from Foch's own script model may
/// be wrong about the game, so the user may insist on running the real game.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
	pub code: DiagnosticCode,
	pub severity: Severity,
	pub span: Option<SourceSpan>,
	pub message: String,
	pub bypassable: bool,
}

impl Diagnostic {
	pub(crate) fn error(
		code: DiagnosticCode,
		span: Option<SourceSpan>,
		message: impl Into<String>,
	) -> Self {
		Self {
			code,
			severity: Severity::Error,
			span,
			message: message.into(),
			bypassable: false,
		}
	}

	pub(crate) fn model_error(
		code: DiagnosticCode,
		span: Option<SourceSpan>,
		message: impl Into<String>,
	) -> Self {
		Self {
			bypassable: true,
			..Self::error(code, span, message)
		}
	}
}

impl From<foch_annotation::Diagnostic> for Diagnostic {
	fn from(diagnostic: foch_annotation::Diagnostic) -> Self {
		use foch_annotation::Code;
		let code = match diagnostic.code {
			Code::Parse => DiagnosticCode::Parse,
			Code::UnknownAnnotation => DiagnosticCode::UnknownAnnotation,
			Code::Orphan => DiagnosticCode::OrphanAnnotation,
			Code::Nested => DiagnosticCode::NestedAnnotation,
			Code::UnsupportedTarget => DiagnosticCode::UnsupportedTarget,
			_ => DiagnosticCode::Annotation,
		};
		Self {
			code,
			severity: diagnostic.severity,
			span: Some(diagnostic.span),
			message: diagnostic.message,
			bypassable: false,
		}
	}
}

impl fmt::Display for Diagnostic {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		if let Some(span) = &self.span {
			write!(formatter, "{span}: ")?;
		}
		let severity = match self.severity {
			Severity::Error => "error",
			Severity::Warning => "warning",
		};
		write!(formatter, "{severity}: {}", self.message)
	}
}

/// Whether diagnostics prevent launching the game. Warnings never block;
/// errors block unless every error is bypassable and the user insisted.
pub fn blocks_run(diagnostics: &[Diagnostic], run_anyway: bool) -> bool {
	diagnostics.iter().any(|diagnostic| {
		diagnostic.severity == Severity::Error && !(run_anyway && diagnostic.bypassable)
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_model_findings_can_be_overridden() {
		let model = Diagnostic::model_error(DiagnosticCode::UnknownTag, None, "x");
		let malformed = Diagnostic::error(DiagnosticCode::Annotation, None, "x");
		let warning = Diagnostic {
			severity: Severity::Warning,
			..Diagnostic::error(DiagnosticCode::UnknownEvent, None, "x")
		};
		assert!(!blocks_run(std::slice::from_ref(&warning), false));
		assert!(blocks_run(std::slice::from_ref(&model), false));
		assert!(!blocks_run(std::slice::from_ref(&model), true));
		assert!(blocks_run(&[model, malformed], true));
	}
}
