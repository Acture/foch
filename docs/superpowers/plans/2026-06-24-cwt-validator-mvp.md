# CWT Validator MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone schema-conformance validator that checks a parsed Paradox mod file against a `CwtSchema`, reporting invalid enum values and unknown keys with severity tiers and a false-positive suppression gate.

**Architecture:** A new `foch_language::validator` module (game-aware semantics over the existing `cwt` schema model + Clausewitz parser AST). It is a SEPARATE subsystem from the merger (different module, its own diagnostic types, surfaced later as a distinct `foch validate` command — never fused into merge). The MVP core is a pure function over `(&CwtSchema, &[CwtRule], &[AstStatement])`, fully unit-testable on synthetic schema+AST with no vendored `.cwt` and no real EU4 data.

**Tech Stack:** Rust (edition 2024, hard tabs). Consumes `foch_language::cwt::{CwtSchema, CwtType, CwtRule, CwtRuleBody, CwtValueType, CwtEnum}` and `foch_language::analyzer::parser::{AstStatement, AstValue, ScalarValue, SpanRange, parse_clausewitz_content}`.

**North star:** completely + correctly merge all local mods with maximal compatibility. The validator is the other half of "cwt is the prerequisite for the validator": it surfaces schema violations a mod author must fix, independent of merge.

**Scope note (writing-plans Scope Check):** This is the validator MVP only — two checks (enum-value, unknown-key) over an already-resolved rule set. Separate plans/issues cover: vendoring real `.cwt` (Phase 0b), path→type resolution wiring against real data, base-symbol-table checks (`<religion>` value existence), cardinality/required-field checks, and the `foch validate` CLI. Each task here produces working, tested software on its own.

---

## File Structure

- Create `crates/foch-language/src/validator/mod.rs` — module API: `ValidatorFinding`, `ValidatorSeverity`, `ValidationOptions`, re-exports, and the `validate_block` orchestrator.
- Create `crates/foch-language/src/validator/finding.rs` — diagnostic types (`ValidatorFinding`, `ValidatorSeverity`).
- Create `crates/foch-language/src/validator/enum_check.rs` — invalid-enum-value check.
- Create `crates/foch-language/src/validator/unknown_key_check.rs` — unknown-key check + validated-subset gating + suppression.
- Modify `crates/foch-language/src/lib.rs` — add `pub mod validator;`.

Each file has one responsibility (types / enum check / unknown-key check / orchestration). The two checks operate only on `(&CwtSchema, &[CwtRule], &[AstStatement])`, so they unit-test independently with hand-built inputs.

---

### Task 1: Validator diagnostic types

**Files:**
- Create: `crates/foch-language/src/validator/finding.rs`
- Create: `crates/foch-language/src/validator/mod.rs`
- Modify: `crates/foch-language/src/lib.rs` (add `pub mod validator;`)

- [ ] **Step 1: Write the failing test**

In `crates/foch-language/src/validator/finding.rs` (bottom):

```rust
#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn finding_orders_by_severity_then_position() {
		let err = ValidatorFinding {
			rule_id: "invalid-enum-value".to_string(),
			severity: ValidatorSeverity::Error,
			message: "x".to_string(),
			line: 5,
			column: 1,
		};
		let warn = ValidatorFinding {
			rule_id: "unknown-key".to_string(),
			severity: ValidatorSeverity::Warning,
			message: "y".to_string(),
			line: 2,
			column: 1,
		};
		assert!(ValidatorSeverity::Error > ValidatorSeverity::Warning);
		assert!(ValidatorSeverity::Warning > ValidatorSeverity::Info);
		// sort_key groups by severity (desc handled by caller) then line/column
		assert!(err.sort_key() > warn.sort_key() || err.severity != warn.severity);
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p foch-language --lib validator::finding -- --nocapture`
Expected: FAIL to compile (`validator` module missing).

- [ ] **Step 3: Write minimal implementation**

In `crates/foch-language/src/lib.rs` add (alphabetically, near `pub mod cwt;`):

```rust
pub mod validator;
```

Create `crates/foch-language/src/validator/mod.rs`:

```rust
mod enum_check;
mod finding;
mod unknown_key_check;

pub use finding::{ValidatorFinding, ValidatorSeverity};
```

(Only declare `mod finding;` referenced items for now. Add `enum_check`/`unknown_key_check` `mod` lines in their tasks — to keep Task 1 compiling, comment those two `mod` lines until Task 2/3. The `pub use` of finding stays.)

Create `crates/foch-language/src/validator/finding.rs`:

```rust
/// Severity tier for a validator finding. Ordering is meaningful:
/// `Error > Warning > Info`, so callers can sort/filter by minimum severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ValidatorSeverity {
	Info,
	Warning,
	Error,
}

/// A single schema-conformance finding produced by the validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorFinding {
	pub rule_id: String,
	pub severity: ValidatorSeverity,
	pub message: String,
	pub line: usize,
	pub column: usize,
}

impl ValidatorFinding {
	/// Stable sort key: position first, so findings read top-to-bottom.
	pub fn sort_key(&self) -> (usize, usize, String) {
		(self.line, self.column, self.rule_id.clone())
	}
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p foch-language --lib validator::finding -- --nocapture`
Expected: PASS.

Note: the not-yet-built `enum_check`/`unknown_key_check` modules are commented out in `mod.rs`, so the crate compiles. If clippy `-D warnings` complains about anything, fix minimally. Run `cargo clippy -p foch-language --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/foch-language/src/lib.rs crates/foch-language/src/validator/mod.rs crates/foch-language/src/validator/finding.rs
git commit -m "language(validator): diagnostic types for the cwt validator"
```

---

### Task 2: Invalid-enum-value check

**Files:**
- Create: `crates/foch-language/src/validator/enum_check.rs`
- Modify: `crates/foch-language/src/validator/mod.rs` (enable `mod enum_check;`)

**Behavior:** Given a flat list of `CwtRule`s for a block and the AST statements in that block, for each `AstStatement::Assignment { key, value: Scalar }` whose matching rule has `body == CwtRuleBody::Leaf(CwtValueType::Enum(enum_name))`, look up the enum in `schema.enums`. If the enum is known AND the scalar value is not one of its `values`, emit an `Error` finding `invalid-enum-value`. If the enum name is NOT in the schema, emit nothing (can't validate what we don't know — avoids false positives). Matching rule = the `CwtRule` whose `key == assignment.key`.

- [ ] **Step 1: Write the failing test**

In `crates/foch-language/src/validator/enum_check.rs` (bottom):

```rust
#[cfg(test)]
mod tests {
	use super::*;
	use crate::cwt::{CwtEnum, CwtRule, CwtRuleBody, CwtSchema, CwtValueType};
	use crate::analyzer::parser::parse_clausewitz_content;
	use std::path::PathBuf;

	fn rules() -> Vec<CwtRule> {
		vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Enum("power_categories".to_string())),
			cardinality: None,
			options: Vec::new(),
		}]
	}

	fn schema() -> CwtSchema {
		CwtSchema {
			enums: vec![CwtEnum {
				name: "power_categories".to_string(),
				values: vec!["ADM".to_string(), "DIP".to_string(), "MIL".to_string()],
			}],
			..Default::default()
		}
	}

	fn ast(src: &str) -> Vec<crate::analyzer::parser::AstStatement> {
		parse_clausewitz_content(PathBuf::from("t.txt"), src).ast.statements
	}

	#[test]
	fn flags_value_outside_enum() {
		let findings = check_enum_values(&schema(), &rules(), &ast("category = ECO\n"));
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "invalid-enum-value");
		assert_eq!(findings[0].severity, ValidatorSeverity::Error);
	}

	#[test]
	fn accepts_value_in_enum() {
		let findings = check_enum_values(&schema(), &rules(), &ast("category = ADM\n"));
		assert!(findings.is_empty());
	}

	#[test]
	fn ignores_unknown_enum_name() {
		// rule references an enum the schema doesn't define -> cannot validate, no finding
		let mut s = schema();
		s.enums.clear();
		let findings = check_enum_values(&s, &rules(), &ast("category = ECO\n"));
		assert!(findings.is_empty());
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p foch-language --lib validator::enum_check -- --nocapture`
Expected: FAIL to compile (`check_enum_values` not found).

- [ ] **Step 3: Write minimal implementation**

In `validator/mod.rs` uncomment/add `mod enum_check;` and `pub use enum_check::check_enum_values;`.

Create `crates/foch-language/src/validator/enum_check.rs` (above the test block):

```rust
use super::finding::{ValidatorFinding, ValidatorSeverity};
use crate::analyzer::parser::{AstStatement, AstValue};
use crate::cwt::{CwtRule, CwtRuleBody, CwtSchema, CwtValueType};

/// Check scalar assignments whose rule declares an `enum[...]` value type:
/// the value must be one of the schema enum's known values. Unknown enum
/// names are skipped (we cannot validate what the schema does not define).
pub fn check_enum_values(
	schema: &CwtSchema,
	rules: &[CwtRule],
	statements: &[AstStatement],
) -> Vec<ValidatorFinding> {
	let mut findings = Vec::new();
	for statement in statements {
		let AstStatement::Assignment { key, value, .. } = statement else {
			continue;
		};
		let AstValue::Scalar { value: scalar, span } = value else {
			continue;
		};
		let Some(rule) = rules.iter().find(|r| &r.key == key) else {
			continue;
		};
		let CwtRuleBody::Leaf(CwtValueType::Enum(enum_name)) = &rule.body else {
			continue;
		};
		let Some(cwt_enum) = schema.enums.iter().find(|e| &e.name == enum_name) else {
			continue; // unknown enum -> cannot validate
		};
		let text = scalar.as_text();
		if !cwt_enum.values.iter().any(|v| v == &text) {
			findings.push(ValidatorFinding {
				rule_id: "invalid-enum-value".to_string(),
				severity: ValidatorSeverity::Error,
				message: format!(
					"value `{text}` for `{key}` is not in enum `{enum_name}` (allowed: {})",
					cwt_enum.values.join(", ")
				),
				line: span.start.line,
				column: span.start.column,
			});
		}
	}
	findings
}
```

NOTE for the engineer: confirm `SpanRange`'s field names by reading `crates/foch-language/src/analyzer/parser.rs` (look for `pub struct SpanRange` and `SpanPoint`/`start`). If the line/column accessor differs (e.g. `span.start.line` vs a helper), use the real shape. Confirm `ScalarValue::as_text()` exists (it is used elsewhere in the cwt interpreter).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p foch-language --lib validator::enum_check -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/foch-language/src/validator/mod.rs crates/foch-language/src/validator/enum_check.rs
git commit -m "language(validator): invalid-enum-value check"
```

---

### Task 3: Unknown-key check (validated-subset gated, suppressible)

**Files:**
- Create: `crates/foch-language/src/validator/unknown_key_check.rs`
- Modify: `crates/foch-language/src/validator/mod.rs` (enable `mod unknown_key_check;`, add `ValidationOptions`)

**Behavior:** Given the `CwtRule`s for a block and the AST statements, for each `AstStatement::Assignment { key }` whose `key` is NOT the `key` of any rule in the set, emit a `Warning` finding `unknown-key` — BUT only when the block is "validated-subset" eligible and the key is not suppressed. This check is false-positive-prone (alias_name expansions, dynamic keys), so it is conservative by default:
- `ValidationOptions.check_unknown_keys` gates the whole check (default `false` — opt-in).
- `ValidationOptions.suppressed_keys: BTreeSet<String>` skips known-dynamic keys.
- A rule set containing any `alias_name`/`alias_match_left` leaf (i.e. the block accepts open-ended aliased keys) disables unknown-key checking for that block entirely (can't enumerate valid keys).

- [ ] **Step 1: Write the failing test**

In `crates/foch-language/src/validator/unknown_key_check.rs` (bottom):

```rust
#[cfg(test)]
mod tests {
	use super::*;
	use crate::cwt::{CwtRule, CwtRuleBody, CwtValueType};
	use crate::analyzer::parser::parse_clausewitz_content;
	use std::collections::BTreeSet;
	use std::path::PathBuf;

	fn rule(key: &str, vt: CwtValueType) -> CwtRule {
		CwtRule { key: key.to_string(), body: CwtRuleBody::Leaf(vt), cardinality: None, options: Vec::new() }
	}
	fn ast(src: &str) -> Vec<crate::analyzer::parser::AstStatement> {
		parse_clausewitz_content(PathBuf::from("t.txt"), src).ast.statements
	}

	#[test]
	fn flags_unknown_key_when_enabled() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions { enabled: true, suppressed: BTreeSet::new() };
		let findings = check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts);
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unknown-key");
		assert_eq!(findings[0].severity, ValidatorSeverity::Warning);
	}

	#[test]
	fn known_key_is_clean() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions { enabled: true, suppressed: BTreeSet::new() };
		assert!(check_unknown_keys(&rules, &ast("category = ADM\n"), &opts).is_empty());
	}

	#[test]
	fn disabled_by_default() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let opts = UnknownKeyOptions { enabled: false, suppressed: BTreeSet::new() };
		assert!(check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts).is_empty());
	}

	#[test]
	fn suppressed_key_is_skipped() {
		let rules = vec![rule("category", CwtValueType::Scalar)];
		let mut suppressed = BTreeSet::new();
		suppressed.insert("bogus".to_string());
		let opts = UnknownKeyOptions { enabled: true, suppressed };
		assert!(check_unknown_keys(&rules, &ast("bogus = 1\n"), &opts).is_empty());
	}

	#[test]
	fn alias_accepting_block_disables_check() {
		// a rule whose value type is alias_name[...] means the block accepts
		// open-ended aliased keys, so unknown-key checking must not fire.
		let rules = vec![rule("trigger", CwtValueType::AliasName("trigger".to_string()))];
		let opts = UnknownKeyOptions { enabled: true, suppressed: BTreeSet::new() };
		assert!(check_unknown_keys(&rules, &ast("anything = 1\n"), &opts).is_empty());
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p foch-language --lib validator::unknown_key_check -- --nocapture`
Expected: FAIL to compile (`check_unknown_keys`/`UnknownKeyOptions` not found).

- [ ] **Step 3: Write minimal implementation**

In `validator/mod.rs` add `mod unknown_key_check;` and `pub use unknown_key_check::{check_unknown_keys, UnknownKeyOptions};`.

Create `crates/foch-language/src/validator/unknown_key_check.rs` (above the test block):

```rust
use super::finding::{ValidatorFinding, ValidatorSeverity};
use crate::analyzer::parser::AstStatement;
use crate::cwt::{CwtRule, CwtRuleBody, CwtValueType};
use std::collections::BTreeSet;

/// Options controlling the false-positive-prone unknown-key check.
#[derive(Clone, Debug, Default)]
pub struct UnknownKeyOptions {
	/// Off by default; opt in only where the rule set is trusted.
	pub enabled: bool,
	/// Keys to never flag (dynamic / engine-handled).
	pub suppressed: BTreeSet<String>,
}

/// Flag assignment keys not present in the rule set. Conservative:
/// - disabled unless `options.enabled`;
/// - skipped for suppressed keys;
/// - entirely disabled for blocks whose rules accept open-ended aliased keys
///   (`alias_name[...]` / `alias_match_left[...]`), which cannot be enumerated.
pub fn check_unknown_keys(
	rules: &[CwtRule],
	statements: &[AstStatement],
	options: &UnknownKeyOptions,
) -> Vec<ValidatorFinding> {
	if !options.enabled || block_accepts_open_keys(rules) {
		return Vec::new();
	}
	let known: BTreeSet<&str> = rules.iter().map(|r| r.key.as_str()).collect();
	let mut findings = Vec::new();
	for statement in statements {
		let AstStatement::Assignment { key, key_span, .. } = statement else {
			continue;
		};
		if known.contains(key.as_str()) || options.suppressed.contains(key) {
			continue;
		}
		findings.push(ValidatorFinding {
			rule_id: "unknown-key".to_string(),
			severity: ValidatorSeverity::Warning,
			message: format!("unknown key `{key}` is not declared in the schema for this block"),
			line: key_span.start.line,
			column: key_span.start.column,
		});
	}
	findings
}

fn block_accepts_open_keys(rules: &[CwtRule]) -> bool {
	rules.iter().any(|r| {
		matches!(
			&r.body,
			CwtRuleBody::Leaf(CwtValueType::AliasName(_))
				| CwtRuleBody::Leaf(CwtValueType::AliasMatchLeft(_))
		)
	})
}
```

NOTE: confirm `key_span` field name on `AstStatement::Assignment` (the parser uses `key_span: SpanRange`) and `SpanRange.start.{line,column}` against the real parser source; adjust if different.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p foch-language --lib validator::unknown_key_check -- --nocapture`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/foch-language/src/validator/mod.rs crates/foch-language/src/validator/unknown_key_check.rs
git commit -m "language(validator): unknown-key check with validated-subset gating + suppression"
```

---

### Task 4: `validate_block` orchestrator + `ValidationOptions`

**Files:**
- Modify: `crates/foch-language/src/validator/mod.rs`

**Behavior:** A single entry point `validate_block(schema, rules, statements, options) -> Vec<ValidatorFinding>` that runs both checks and returns findings sorted by `(line, column, rule_id)`. `ValidationOptions` bundles the unknown-key opt-in + suppression. This is the stable surface a future `foch validate` command calls (after path→type resolution picks the `rules`).

- [ ] **Step 1: Write the failing test**

Add to `crates/foch-language/src/validator/mod.rs` (in a `#[cfg(test)] mod tests` block):

```rust
#[cfg(test)]
mod tests {
	use super::*;
	use crate::cwt::{CwtEnum, CwtRule, CwtRuleBody, CwtSchema, CwtValueType};
	use crate::analyzer::parser::parse_clausewitz_content;
	use std::path::PathBuf;

	#[test]
	fn validate_block_runs_both_checks_sorted() {
		let schema = CwtSchema {
			enums: vec![CwtEnum {
				name: "cat".to_string(),
				values: vec!["ADM".to_string()],
			}],
			..Default::default()
		};
		let rules = vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Enum("cat".to_string())),
			cardinality: None,
			options: Vec::new(),
		}];
		// line 1: unknown key (warning, when enabled); line 2: bad enum value (error)
		let src = "bogus = 1\ncategory = ECO\n";
		let ast = parse_clausewitz_content(PathBuf::from("t.txt"), src).ast.statements;
		let mut options = ValidationOptions::default();
		options.check_unknown_keys = true;
		let findings = validate_block(&schema, &rules, &ast, &options);
		assert_eq!(findings.len(), 2);
		// sorted by line: unknown-key (line 1) before invalid-enum-value (line 2)
		assert_eq!(findings[0].rule_id, "unknown-key");
		assert_eq!(findings[1].rule_id, "invalid-enum-value");
	}

	#[test]
	fn unknown_keys_off_by_default() {
		let schema = CwtSchema::default();
		let rules = vec![CwtRule {
			key: "category".to_string(),
			body: CwtRuleBody::Leaf(CwtValueType::Scalar),
			cardinality: None,
			options: Vec::new(),
		}];
		let ast = parse_clausewitz_content(PathBuf::from("t.txt"), "bogus = 1\n").ast.statements;
		let findings = validate_block(&schema, &rules, &ast, &ValidationOptions::default());
		assert!(findings.is_empty());
	}
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p foch-language --lib validator::tests -- --nocapture`
Expected: FAIL to compile (`validate_block` / `ValidationOptions` not found).

- [ ] **Step 3: Write minimal implementation**

Add to `crates/foch-language/src/validator/mod.rs` (after the `pub use` lines):

```rust
use crate::analyzer::parser::AstStatement;
use crate::cwt::{CwtRule, CwtSchema};
use std::collections::BTreeSet;

pub use enum_check::check_enum_values;
pub use unknown_key_check::{check_unknown_keys, UnknownKeyOptions};

/// Top-level validation options for a block.
#[derive(Clone, Debug, Default)]
pub struct ValidationOptions {
	/// Opt-in for the false-positive-prone unknown-key check.
	pub check_unknown_keys: bool,
	/// Keys the unknown-key check must never flag.
	pub suppressed_keys: BTreeSet<String>,
}

/// Validate the statements of one block against its cwt rule set, returning
/// findings sorted by position then rule id. Runs the enum-value check
/// (always) and the unknown-key check (gated by options).
pub fn validate_block(
	schema: &CwtSchema,
	rules: &[CwtRule],
	statements: &[AstStatement],
	options: &ValidationOptions,
) -> Vec<ValidatorFinding> {
	let mut findings = check_enum_values(schema, rules, statements);
	let unknown_options = UnknownKeyOptions {
		enabled: options.check_unknown_keys,
		suppressed: options.suppressed_keys.clone(),
	};
	findings.extend(check_unknown_keys(rules, statements, &unknown_options));
	findings.sort_by_key(|f| f.sort_key());
	findings
}
```

Ensure the final `mod.rs` declares `mod enum_check; mod finding; mod unknown_key_check;` and re-exports `ValidatorFinding, ValidatorSeverity, ValidationOptions, validate_block, check_enum_values, check_unknown_keys, UnknownKeyOptions`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p foch-language --lib validator -- --nocapture`
Expected: PASS (all validator tests: finding 1 + enum 3 + unknown 5 + orchestrator 2).

- [ ] **Step 5: Full gauntlet + commit**

Run:
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p foch-engine --test merge_e2e
```
Expected: all green; `merge_e2e` 14/14 (the validator is additive analysis — it must NOT change merge output; no version-const bump needed, nothing serialized into existing caches).

```bash
git add crates/foch-language/src/validator/mod.rs
git commit -m "language(validator): validate_block orchestrator over enum + unknown-key checks"
```

---

### Task 5 (follow-on, separate issue — NOT in this PR): path→type resolution + `foch validate` CLI

Wire the validator to real mods: resolve which `CwtType.rules` apply to a file (by `CwtType.path` suffix-matching the file's relative path), load a real (vendored) `CwtSchema`, and add a `foch validate <playset>` CLI command that parses each mod file, resolves its type, runs `validate_block`, and renders findings. This depends on Phase 0b (vendoring `.cwt`) and the base-symbol-table work (for `<type_ref>` existence checks). Tracked separately under the *Standalone validator* milestone; left out here so the MVP ships self-contained and fully unit-tested.

---

## Self-Review

**Spec coverage (issue #29: "Validator MVP over CwtSchema: enum-value + unknown-key checks, severity-tiered, suppression/validated-subset gate"):**
- enum-value check → Task 2 (`check_enum_values`). ✅
- unknown-key check → Task 3 (`check_unknown_keys`). ✅
- severity-tiered → Task 1 (`ValidatorSeverity::{Error,Warning,Info}`, ordered). ✅
- suppression → Task 3 (`UnknownKeyOptions.suppressed`) + Task 4 (`ValidationOptions.suppressed_keys`). ✅
- validated-subset gate → Task 3 (`enabled` opt-in + `block_accepts_open_keys` disables on aliased blocks). ✅
- "separate from the merger" → distinct `validator` module, own diagnostic types, not wired into merge; CLI deferred to Task 5. ✅

**Placeholder scan:** All code blocks are concrete. The only IMPLEMENTOR notes are: confirm `SpanRange.start.{line,column}` and `AstStatement::Assignment.key_span` / `ScalarValue::as_text()` field names against `parser.rs` (read one file, use real names) — not invented behavior. Path→type resolution and real-`.cwt` loading are explicitly deferred to Task 5 (separate issue), not stubbed here.

**Type consistency:** `ValidatorFinding`, `ValidatorSeverity`, `ValidationOptions`, `UnknownKeyOptions`, `check_enum_values(&CwtSchema, &[CwtRule], &[AstStatement])`, `check_unknown_keys(&[CwtRule], &[AstStatement], &UnknownKeyOptions)`, and `validate_block(&CwtSchema, &[CwtRule], &[AstStatement], &ValidationOptions)` are used identically across tasks. `CwtRuleBody::Leaf(CwtValueType::Enum(_))` / `AliasName(_)` / `AliasMatchLeft(_)` match the real `cwt::model` enum variants confirmed on master.

**Determinism / safety:** findings sorted deterministically; checks are pure, total (no panics — `let-else`/`find`), and additive (no merge-path changes; `merge_e2e` re-run in Task 4). Unknown enum names and aliased blocks are intentionally NOT flagged to control false positives — a core requirement, not a gap.
