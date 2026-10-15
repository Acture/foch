//! How EU4 coerces script text into the value a field's consumer reads.
//!
//! The merge compares numbers by their exact decimal value and never by what
//! a reader keeps, because which reader applies differs from field to field.
//! This module records the one reader the binary shows and the fields an
//! in-game check tied to it, so the final write can drop digits the game was
//! seen to ignore and leave everything else as the mods wrote it.
//!
//! Evidence, read out of the unstripped 1.37.5 binary, which ships with full
//! C++ symbols. The reasoning and the call-site census are in
//! `docs/project-status.md`; the disassembly itself was taken under `target/`
//! and is gone, so what is load-bearing is restated here rather than cited:
//!
//! * The script surface — `CReader::Read(CFixedPoint&)`, `TValueEffect`,
//!   `TValueTrigger`, `CReader::ReadUniform` — reaches
//!   `CToken::ReadValue(CFixedPoint&)`, which takes the integer part with
//!   `sscanf("%i")`, finds `.` with `strchr`, copies **at most three** fraction
//!   digits into a `"000"` buffer, and scales the integer part by 1000.
//!   `CToken::GetFloat` does the same through `StringToFixedPoint`. Three
//!   decimals, truncated, never rounded.
//! * `CToken::GetInt` is `atoi`: the integer prefix, and `0` for text that has
//!   none.
//! * The game's finer reader, `CToken::GetFloat64` at 1/32768, has two callers
//!   — `CCountry::ReadMember` and an `fpml` overload — and neither is a script
//!   path, so it does not apply to file content.
//! * The binary does not say which fields reach which reader, and in game they
//!   differ: the `add_army_professionalism` effect and the
//!   `army_professionalism` trigger keep a fourth decimal that `add_prestige`
//!   drops. So a field counts as three-decimal only once an in-game check has
//!   shown it, and the list below holds exactly those.
//!
//! This is pinned to that engine build. A future patch that changes a reader
//! invalidates the list below, not just its precision.

use crate::game::schema::query::CompiledAliasCategory;

/// The number of fraction digits `CToken::ReadValue` keeps.
const KEPT_FRACTION_DIGITS: usize = 3;

/// The scale `CToken::ReadValue` applies to the integer part (`imul eax, 0x3e8`).
const FIXED_POINT_SCALE: i64 = 1000;

/// A value read through the script fixed-point reader, in thousandths.
///
/// Comparing these compares what the game holds, not what the file says.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScriptFixedPoint(i64);

/// Reads `text` the way the script fixed-point reader does: integer part plus
/// at most three truncated fraction digits.
///
/// Returns `None` for anything that is not a plain decimal numeral. The game
/// would still read *some* value there — `atoi` yields `0` for text it cannot
/// parse — but claiming an equivalence from a shape this model has not been
/// checked against would fold values that are only incidentally alike, so
/// unmodelled shapes abstain instead.
pub fn script_fixed_point(text: &str) -> Option<ScriptFixedPoint> {
	let (negative, digits) = split_sign(text)?;
	let (integer, fraction) = match digits.split_once('.') {
		Some((integer, fraction)) => (integer, fraction),
		None => (digits, ""),
	};
	if integer.is_empty() && fraction.is_empty() {
		return None;
	}
	if !is_ascii_digits(integer) || !is_ascii_digits(fraction) {
		return None;
	}
	let integer = if integer.is_empty() {
		0
	} else {
		integer.parse::<i64>().ok()?
	};
	// Short fractions are zero-filled and long ones truncated, which is the
	// `"000"` buffer the reader copies into.
	let kept = &fraction[..fraction.len().min(KEPT_FRACTION_DIGITS)];
	let thousandths = match kept.len() {
		0 => 0,
		length => kept.parse::<i64>().ok()? * 10_i64.pow((KEPT_FRACTION_DIGITS - length) as u32),
	};
	let magnitude = integer
		.checked_mul(FIXED_POINT_SCALE)?
		.checked_add(thousandths)?;
	Some(ScriptFixedPoint(if negative {
		-magnitude
	} else {
		magnitude
	}))
}

/// Effects whose number was seen, in game, to lose every digit past the third.
///
/// Each was checked on 1.37.5 by applying a value with a fourth decimal and
/// reading back what the game held. `change_innovativeness` is typed `int` in
/// the schema but read three decimals in game, so the list goes by name, not
/// by type.
const THREE_DECIMAL_EFFECTS: &[&str] = &[
	"add_army_tradition",
	"add_navy_tradition",
	"add_mercantilism",
	"add_legitimacy",
	"add_war_exhaustion",
	"add_inflation",
	"add_corruption",
	"change_innovativeness",
	"add_prestige",
	"add_local_autonomy",
	"add_prosperity",
];

/// Triggers whose threshold was seen to compare at three decimals.
const THREE_DECIMAL_TRIGGERS: &[&str] = &["army_tradition", "prestige", "mercantilism"];

/// Modifiers whose total was seen to keep three decimals.
const THREE_DECIMAL_MODIFIERS: &[&str] = &[
	"yearly_army_professionalism",
	"discipline",
	"drill_gain_modifier",
	"global_trade_goods_size_modifier",
	"harsh_treatment_cost",
];

/// Effects whose `value = ...` field was seen to keep three decimals.
const THREE_DECIMAL_VALUE_EFFECTS: &[&str] = &["set_variable", "change_variable"];

/// Triggers whose `value = ...` field was seen to compare at three decimals.
const THREE_DECIMAL_VALUE_TRIGGERS: &[&str] = &["check_variable", "has_global_modifier_value"];

/// Whether the game was seen to read the number bound to this schema alias
/// at three decimals.
///
/// The category matters: `prestige` is a trigger that was checked and a
/// modifier that was not.
pub fn alias_reads_three_decimals(category: &CompiledAliasCategory, name: &str) -> bool {
	let checked = match category {
		CompiledAliasCategory::Effect => THREE_DECIMAL_EFFECTS,
		CompiledAliasCategory::Trigger => THREE_DECIMAL_TRIGGERS,
		CompiledAliasCategory::Modifier => THREE_DECIMAL_MODIFIERS,
		CompiledAliasCategory::Link | CompiledAliasCategory::Other(_) => return false,
	};
	checked.contains(&name)
}

/// Whether the game was seen to read the `value` field inside the block this
/// schema alias binds at three decimals.
///
/// The schema types that `value` as a number, a variable name or a keyword at
/// once, so it is found by the block around it rather than by its own type.
pub fn alias_value_reads_three_decimals(category: &CompiledAliasCategory, name: &str) -> bool {
	let checked = match category {
		CompiledAliasCategory::Effect => THREE_DECIMAL_VALUE_EFFECTS,
		CompiledAliasCategory::Trigger => THREE_DECIMAL_VALUE_TRIGGERS,
		_ => return false,
	};
	checked.contains(&name)
}

/// `text` without the fraction digits past the third, or `None` when there are
/// none to drop.
///
/// Only for a field the game reads at three decimals, where this is the value
/// the game already holds. Everything else about the spelling stays as
/// written.
pub fn drop_unread_fraction_digits(text: &str) -> Option<String> {
	script_fixed_point(text)?;
	let (integer, fraction) = text.split_once('.')?;
	(fraction.len() > KEPT_FRACTION_DIGITS)
		.then(|| format!("{integer}.{}", &fraction[..KEPT_FRACTION_DIGITS]))
}

fn split_sign(text: &str) -> Option<(bool, &str)> {
	// The sign is modelled separately so truncation goes toward zero on both
	// sides. Which side of the decimal point the binary applies the sign to is
	// not established, but a sign difference is never an equivalence either
	// way, so this cannot fold two values the game keeps apart.
	match text.as_bytes() {
		[b'-', ..] => Some((true, &text[1..])),
		[b'+', ..] => Some((false, &text[1..])),
		[] => None,
		_ => Some((false, text)),
	}
}

fn is_ascii_digits(text: &str) -> bool {
	text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixed(text: &str) -> Option<i64> {
		script_fixed_point(text).map(|value| value.0)
	}

	#[test]
	fn trailing_zeros_do_not_change_the_value_the_game_reads() {
		assert_eq!(fixed("0.5"), Some(500));
		assert_eq!(fixed("0.50"), Some(500));
		assert_eq!(fixed("0.500"), Some(500));
		assert_eq!(fixed("1"), Some(1000));
		assert_eq!(fixed("1.0"), Some(1000));
		assert_eq!(fixed("1.000"), Some(1000));
	}

	#[test]
	fn the_fourth_decimal_is_truncated_rather_than_rounded() {
		assert_eq!(fixed("0.1234"), Some(123));
		assert_eq!(fixed("0.1239"), Some(123));
		// Rounding would make this 124; the reader copies three digits.
		assert_eq!(fixed("0.1299"), Some(129));
		assert_eq!(fixed("0.0025"), Some(2));
	}

	#[test]
	fn distinct_values_stay_distinct() {
		assert_ne!(fixed("0.5"), fixed("0.6"));
		assert_ne!(fixed("1.001"), fixed("1.002"));
		assert_ne!(fixed("0.5"), fixed("-0.5"));
		assert_ne!(fixed("1"), fixed("10"));
	}

	#[test]
	fn negative_values_truncate_toward_zero() {
		assert_eq!(fixed("-0.1234"), Some(-123));
		assert_eq!(fixed("-1.5"), Some(-1500));
		assert_eq!(fixed("-0.50"), fixed("-0.5"));
	}

	#[test]
	fn leading_zeros_and_a_missing_integer_part_are_read_as_written() {
		assert_eq!(fixed("007"), Some(7000));
		assert_eq!(fixed(".5"), Some(500));
		assert_eq!(fixed("0.5"), fixed(".5"));
		assert_eq!(fixed("5."), Some(5000));
	}

	#[test]
	fn unmodelled_shapes_abstain() {
		assert_eq!(fixed(""), None);
		assert_eq!(fixed("."), None);
		assert_eq!(fixed("-"), None);
		assert_eq!(fixed("yes"), None);
		assert_eq!(fixed("1e-3"), None);
		assert_eq!(fixed("1444.11.11"), None);
		assert_eq!(fixed("0x10"), None);
		assert_eq!(fixed("123abc"), None);
	}

	#[test]
	fn digits_past_the_third_are_dropped_and_nothing_else_moves() {
		assert_eq!(
			drop_unread_fraction_digits("0.0125").as_deref(),
			Some("0.012")
		);
		assert_eq!(
			drop_unread_fraction_digits("0.1239").as_deref(),
			Some("0.123")
		);
		assert_eq!(
			drop_unread_fraction_digits("-0.0125").as_deref(),
			Some("-0.012")
		);
		assert_eq!(
			drop_unread_fraction_digits("1.0000").as_deref(),
			Some("1.000")
		);
		assert_eq!(
			drop_unread_fraction_digits("+.12345").as_deref(),
			Some("+.123")
		);
	}

	#[test]
	fn a_number_the_game_reads_whole_is_left_alone() {
		for text in [
			"0.5",
			"0.50",
			"0.125",
			"1",
			"-3",
			"yes",
			"1e-3",
			"1444.11.11",
		] {
			assert_eq!(drop_unread_fraction_digits(text), None, "{text}");
		}
	}

	#[test]
	fn dropping_is_what_the_reader_already_does() {
		for text in ["0.0125", "0.1239", "-0.0125", "1.0000", "7.12345"] {
			let dropped = drop_unread_fraction_digits(text).expect("digits to drop");
			assert_eq!(
				script_fixed_point(&dropped),
				script_fixed_point(text),
				"{text}"
			);
		}
	}

	#[test]
	fn only_the_checked_fields_count_as_three_decimal() {
		assert!(alias_reads_three_decimals(
			&CompiledAliasCategory::Effect,
			"add_prestige"
		));
		assert!(alias_reads_three_decimals(
			&CompiledAliasCategory::Modifier,
			"discipline"
		));
		assert!(alias_reads_three_decimals(
			&CompiledAliasCategory::Trigger,
			"prestige"
		));
		// Checked in game and found to keep the fourth decimal.
		assert!(!alias_reads_three_decimals(
			&CompiledAliasCategory::Effect,
			"add_army_professionalism"
		));
		assert!(!alias_reads_three_decimals(
			&CompiledAliasCategory::Trigger,
			"army_professionalism"
		));
		// One name, two fields: the modifier was never checked.
		assert!(!alias_reads_three_decimals(
			&CompiledAliasCategory::Modifier,
			"prestige"
		));
	}

	#[test]
	fn a_value_field_is_found_by_the_block_around_it() {
		assert!(alias_value_reads_three_decimals(
			&CompiledAliasCategory::Effect,
			"set_variable"
		));
		assert!(alias_value_reads_three_decimals(
			&CompiledAliasCategory::Trigger,
			"has_global_modifier_value"
		));
		assert!(!alias_value_reads_three_decimals(
			&CompiledAliasCategory::Effect,
			"add_prestige"
		));
	}
}
