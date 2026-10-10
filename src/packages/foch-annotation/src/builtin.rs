//! Annotations Foch understands.

use crate::schema::{
	AnnotationKind, AnnotationSchema, ParamSchema, PositionalSchema, Registry, TargetRule,
};
use crate::value::ValueType;

/// Upper bound of real game days a single test may advance in total.
pub const MAX_ADVANCE_DAYS: u32 = 36_500;

pub const TEST: &str = "test";

pub const PARAMETRIZE: &str = "parametrize";

pub const TEST_DESCRIPTION: &str = "#test(...) attaches to the next top-level country_event, which must declare hidden=yes and is_triggered_only=yes. Every annotation line must remain an EU4 comment. #skip, #ignore, #xfail and #mark apply to the next #test. AI is off unless ai=on is given. Without expect, a smoke test verifies invocation completion only, not event behavior.";

/// Parameters shared by `#test(...)` and `test = { ... }` blocks below
/// `tests/`. In a block, `effect`, `advance_days` and `expect` are ordered
/// steps and the block may also `fire` any invocable event.
pub const TEST_PARAMS: &[ParamSchema] = &[
	ParamSchema {
		name: "time",
		value_type: ValueType::Date,
		required: false,
		description: "Initial game date (YYYY.M.D), not a date-change effect. Required unless #parametrize provides time. Runtime support is checked separately.",
		example: "1444.11.11",
	},
	ParamSchema {
		name: "tag",
		value_type: ValueType::Tag,
		required: false,
		description: "Initial country scope; three uppercase ASCII letters/digits, starting with a letter. Required unless #parametrize provides tag.",
		example: "SWE",
	},
	ParamSchema {
		name: "name",
		value_type: ValueType::Text,
		required: false,
		description: "Stable case name, unique per target. Quote names containing spaces. Required in tests/ blocks.",
		example: "\"reform grants flag\"",
	},
	ParamSchema {
		name: "effect",
		value_type: ValueType::Effects,
		required: false,
		description: "Native EU4 preparation effects executed in the chosen country before invoking the target event.",
		example: "{ set_country_flag = ready }",
	},
	ParamSchema {
		name: "advance_days",
		value_type: ValueType::Integer {
			min: 0,
			max: MAX_ADVANCE_DAYS,
		},
		required: false,
		description: "Advance actual game days after calling the event; zero means an immediate check. Does not guarantee ordering against other same-day events.",
		example: "2",
	},
	ParamSchema {
		name: "expect",
		value_type: ValueType::Conditions,
		required: false,
		description: "Native EU4 conditions to assert; each top-level condition is reported separately. Without expect, the case is a smoke test.",
		example: "{ has_country_flag = reform_done }",
	},
	ParamSchema {
		name: "use",
		value_type: ValueType::Identifiers,
		required: false,
		description: "Fixtures defined in tests/, run in dependency order before the case.",
		example: "{ at_war_with_denmark }",
	},
	ParamSchema {
		name: "ai",
		value_type: ValueType::Choice {
			options: &["on", "off"],
		},
		required: false,
		description: "Whether computer-controlled countries act. Default off for deterministic results; write ai=on only when the behavior under test needs AI decisions. A case with ai=on never shares a session.",
		example: "on",
	},
	ParamSchema {
		name: "session",
		value_type: ValueType::Choice {
			options: &["auto", "alone"],
		},
		required: false,
		description: "auto lets Foch share a game session with compatible cases; alone always uses a dedicated session.",
		example: "alone",
	},
	ParamSchema {
		name: "player",
		value_type: ValueType::Bool,
		required: false,
		description: "yes requires the case's country to be the player, so it never shares a session.",
		example: "yes",
	},
];

const REASON: ParamSchema = ParamSchema {
	name: "reason",
	value_type: ValueType::Text,
	required: false,
	description: "Why, shown in reports.",
	example: "\"needs a war fixture\"",
};

pub const SCHEMAS: &[AnnotationSchema] = &[
	AnnotationSchema {
		name: TEST,
		kind: AnnotationKind::Primary,
		description: TEST_DESCRIPTION,
		params: TEST_PARAMS,
		positional: None,
		target: Some(TargetRule {
			key: "country_event",
			required_yes: &["hidden", "is_triggered_only"],
		}),
	},
	AnnotationSchema {
		name: "skip",
		kind: AnnotationKind::Modifier { of: TEST },
		description: "Do not run the next #test; reported as skipped.",
		params: &[REASON],
		positional: None,
		target: None,
	},
	AnnotationSchema {
		name: "ignore",
		kind: AnnotationKind::Modifier { of: TEST },
		description: "Run the next #test only with --include-ignored or --ignored, for slow cases.",
		params: &[REASON],
		positional: None,
		target: None,
	},
	AnnotationSchema {
		name: "xfail",
		kind: AnnotationKind::Modifier { of: TEST },
		description: "The next #test's expectation is known to fail. Only assertion failures are absorbed; crashes and incomplete runs still fail.",
		params: &[
			REASON,
			ParamSchema {
				name: "strict",
				value_type: ValueType::Bool,
				required: false,
				description: "yes turns an unexpected pass into a failure.",
				example: "yes",
			},
		],
		positional: None,
		target: None,
	},
	AnnotationSchema {
		name: "mark",
		kind: AnnotationKind::Modifier { of: TEST },
		description: "Labels the next #test for selection with -m.",
		params: &[],
		positional: Some(PositionalSchema {
			description: "labels",
			example: "war, slow",
		}),
		target: None,
	},
	AnnotationSchema {
		name: PARAMETRIZE,
		kind: AnnotationKind::Modifier { of: TEST },
		description: "Expands the next #test into one case per combination of the listed start dimensions (their Cartesian product). A dimension listed here must not also be given in #test. Each instance is addressed and selected as name[tag=DAN,time=1500.1.1].",
		params: &[
			ParamSchema {
				name: "tag",
				value_type: ValueType::List {
					element: &ValueType::Tag,
				},
				required: false,
				description: "Start countries to expand over; one case per tag.",
				example: "{ SWE DAN NOR }",
			},
			ParamSchema {
				name: "time",
				value_type: ValueType::List {
					element: &ValueType::Date,
				},
				required: false,
				description: "Start dates to expand over; one case per date. Each must be a runtime-supported start.",
				example: "{ 1444.11.11 1500.1.1 }",
			},
		],
		positional: None,
		target: None,
	},
];

pub const REGISTRY: Registry = Registry { schemas: SCHEMAS };
