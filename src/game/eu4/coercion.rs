//! How EU4 coerces script text into the value a field's consumer reads.
//!
//! The game's readers are much coarser than text comparison, so two mods can
//! write one field differently and mean the same thing. Modelling the readers
//! lets the merge distinguish that from a real disagreement.
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
//!
//! This is pinned to that engine build. A future patch that changes a reader
//! invalidates the equivalences below, not just their precision.

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

/// Reads `text` the way `CToken::GetInt` does: `atoi` over the integer prefix.
///
/// Abstains on the same unmodelled shapes as [`script_fixed_point`], for the
/// same reason.
pub fn script_int(text: &str) -> Option<i64> {
	let (negative, digits) = split_sign(text)?;
	let integer_length = digits
		.find(|character: char| !character.is_ascii_digit())
		.unwrap_or(digits.len());
	let (integer, rest) = digits.split_at(integer_length);
	// `atoi` stops at the first non-digit, so a fraction is discarded rather
	// than rounded. Anything else trailing is text this model has not checked.
	if !rest.is_empty() && !is_decimal_fraction(rest) {
		return None;
	}
	if integer.is_empty() {
		return None;
	}
	let magnitude = integer.parse::<i64>().ok()?;
	Some(if negative { -magnitude } else { magnitude })
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

fn is_decimal_fraction(text: &str) -> bool {
	text.strip_prefix('.').is_some_and(is_ascii_digits)
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
	fn the_int_reader_takes_the_integer_prefix() {
		assert_eq!(script_int("123"), Some(123));
		assert_eq!(script_int("0123"), Some(123));
		assert_eq!(script_int("123.9"), Some(123));
		assert_eq!(script_int("-4.9"), Some(-4));
		assert_eq!(script_int("+7"), Some(7));
	}

	#[test]
	fn the_int_reader_abstains_where_the_model_was_not_checked() {
		assert_eq!(script_int("123abc"), None);
		assert_eq!(script_int("abc"), None);
		assert_eq!(script_int(""), None);
		assert_eq!(script_int("1444.11.11"), None);
	}

	#[test]
	fn fixed_point_equality_is_never_coarser_than_int_equality() {
		// The merge may resolve a field's reader to either one. Folding under
		// the fixed-point model must therefore never claim an equality the
		// integer reader would reject, or a wrong field type could hide a real
		// difference. Equal thousandths imply an equal integer part.
		const TEXTS: &[&str] = &[
			"0", "0.0", "0.5", "0.50", "0.6", "1", "1.0", "1.000", "1.5", "2", "-1", "-1.0",
			"-1.5", "10", "007", ".5", "0.1234", "0.1239", "0.9999",
		];
		for left in TEXTS {
			for right in TEXTS {
				let (Some(left_fixed), Some(right_fixed)) =
					(script_fixed_point(left), script_fixed_point(right))
				else {
					continue;
				};
				if left_fixed != right_fixed {
					continue;
				}
				let (Some(left_int), Some(right_int)) = (script_int(left), script_int(right))
				else {
					continue;
				};
				assert_eq!(
					left_int, right_int,
					"`{left}` and `{right}` are one fixed-point value but two integers"
				);
			}
		}
	}
}
