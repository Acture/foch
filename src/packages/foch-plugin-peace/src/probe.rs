//! Script-armed mixed peace probe on the shared economy tick.
//!
//! A test sets `foch_peace_probe_*` country flags. The monthly economy
//! callback reads them through [`foch_runtime::dataplane`] and, once one
//! country holds a complete order, builds the same mixed plan the editor's
//! send path would submit. The `FOCH_RECEIPT/1` line is that submission to
//! the test framework.
//!
//! This channel does not call the native offer constructor, treaty factory,
//! or send callback. Those need a live war and treaty objects the dataplane
//! does not expose, so `native_calls` stays `0`.

use crate::composition::{Clause, Draft, Side};
use crate::settlement::{self, MixedSend, PlanError};
use foch_runtime::dataplane::{CountryFlag, CountryFlags, Eu4Game};
use std::fmt;

/// Prefix of every flag this probe owns. A bare `foch_peace_probe` is not an
/// order; the script uses the starred names below.
pub(crate) const PREFIX: &str = "foch_peace_probe_";

/// Prologue of the pinned economy update (`layout::TICK_RVA`). The same
/// bytes the trade plugin and the directed-pacts probe already require.
pub(crate) const TICK_PATTERN: &str = "88 54 24 10 55 53 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 28 F1 FF FF 48 81 EC D8 0F 00 00 4C 8B F9 BA FF FF FF FF";

/// Treaty kinds the native offer table can hold. Mirrors `native`'s table.
const KIND_LIMIT: u32 = 40;
const DEMAND_ADDRESS: usize = 0x1_0000;
const CONCESSION_ADDRESS: usize = 0x2_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProbeSubmission {
	actor: String,
	opponent: String,
	case_name: String,
	request: String,
	active: Side,
	demands: Vec<(u32, u64)>,
	concessions: Vec<(u32, u64)>,
	plan: MixedSend,
}

impl ProbeSubmission {
	pub(crate) fn receipt(&self) -> String {
		let demands = clauses_field(&self.demands);
		let concessions = clauses_field(&self.concessions);
		let active = match self.active {
			Side::Receive => "receive",
			Side::Give => "give",
		};
		receipt_line(&[
			("type", "mixed_probe"),
			("status", "submitted"),
			("case", &self.case_name),
			("request", &self.request),
			("actor", &self.actor),
			("opponent", &self.opponent),
			("active", active),
			("demands", &demands),
			("concessions", &concessions),
			("merged_terms", &self.plan.merged.len().to_string()),
			("native_calls", "0"),
		])
	}
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProbeError {
	RepeatedField(&'static str),
	BadFlag(String),
	AmbiguousActors,
	Plan(PlanError),
}

impl fmt::Display for ProbeError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::RepeatedField(field) => {
				write!(formatter, "peace probe flag {field} is set more than once")
			}
			Self::BadFlag(name) => {
				write!(formatter, "peace probe flag {name} is not a valid order")
			}
			Self::AmbiguousActors => {
				formatter.write_str("more than one country has a complete peace probe")
			}
			Self::Plan(error) => write!(
				formatter,
				"peace probe cannot construct a mixed offer: {error:?}"
			),
		}
	}
}

impl std::error::Error for ProbeError {}

#[derive(Default)]
struct Parts {
	saw: bool,
	opponent: Option<String>,
	demands: Vec<(u32, u64)>,
	concessions: Vec<(u32, u64)>,
	case_name: Option<String>,
	request: Option<String>,
	active: Option<Side>,
}

/// Reads probe-bearing countries. Countries whose flags cannot be snapshotted
/// are skipped; a later tick can try again. An empty vector means the
/// registry or every flag read was unavailable, which is not a malformed order.
///
/// # Safety
/// `game` must be the live registry on the economy-tick thread, and `digest`
/// must be that process's host-reported executable digest. Flag vectors and
/// the global name table stay stable for this call.
pub(crate) unsafe fn snapshots(game: &Eu4Game, digest: &str) -> Vec<CountryFlags> {
	let Some(countries) = game.countries() else {
		return Vec::new();
	};
	let mut found = Vec::new();
	for index in 0..countries.count() {
		let Some(country) = countries.country(index) else {
			continue;
		};
		let Ok(flags) = (unsafe { country.flags(game.exe_base, digest) }) else {
			continue;
		};
		if flags.flags.iter().any(|flag| flag.name.starts_with(PREFIX)) {
			found.push(flags);
		}
	}
	found
}

/// Constructs at most one mixed submission from the tick's flag snapshots.
/// `Ok(None)` means the script has not finished arming a complete order yet.
pub(crate) fn interpret(snapshots: &[CountryFlags]) -> Result<Option<ProbeSubmission>, ProbeError> {
	let mut ready = Vec::new();
	for snapshot in snapshots {
		if let Some(submission) = one(snapshot)? {
			ready.push(submission);
		}
	}
	match ready.len() {
		0 => Ok(None),
		1 => Ok(ready.pop()),
		_ => Err(ProbeError::AmbiguousActors),
	}
}

fn one(snapshot: &CountryFlags) -> Result<Option<ProbeSubmission>, ProbeError> {
	let mut parts = Parts::default();
	for flag in &snapshot.flags {
		if flag.name.starts_with(PREFIX) {
			parse_flag(flag, &mut parts)?;
		}
	}
	if !parts.saw {
		return Ok(None);
	}
	if !is_tag(&snapshot.country_tag) {
		return Err(ProbeError::BadFlag(snapshot.country_tag.clone()));
	}
	let (Some(opponent), Some(case_name), Some(request)) =
		(parts.opponent, parts.case_name, parts.request)
	else {
		return Ok(None);
	};
	if parts.demands.is_empty() || parts.concessions.is_empty() {
		return Ok(None);
	}
	if opponent == snapshot.country_tag {
		return Err(ProbeError::BadFlag(format!("{PREFIX}vs_{opponent}")));
	}
	let active = parts.active.unwrap_or(Side::Receive);
	let receive = Draft {
		giver: identity(&opponent),
		receiver: identity(&snapshot.country_tag),
		selected: synthetic_clauses(&parts.demands, DEMAND_ADDRESS),
	};
	let give = Draft {
		giver: identity(&snapshot.country_tag),
		receiver: identity(&opponent),
		selected: synthetic_clauses(&parts.concessions, CONCESSION_ADDRESS),
	};
	let plan = settlement::plan(&receive, &give, active).map_err(ProbeError::Plan)?;
	Ok(Some(ProbeSubmission {
		actor: snapshot.country_tag.clone(),
		opponent,
		case_name,
		request,
		active,
		demands: parts.demands,
		concessions: parts.concessions,
		plan,
	}))
}

fn parse_flag(flag: &CountryFlag, parts: &mut Parts) -> Result<(), ProbeError> {
	let Some(rest) = flag.name.strip_prefix(PREFIX) else {
		return Ok(());
	};
	parts.saw = true;
	let (head, tail) = rest.split_once('_').unwrap_or((rest, ""));
	match head {
		"vs" => {
			if !is_tag(tail) {
				return Err(ProbeError::BadFlag(flag.name.clone()));
			}
			set_once(&mut parts.opponent, tail.to_owned(), "vs")
		}
		"demand" => {
			parts.demands.push(parse_clause(&flag.name, tail)?);
			Ok(())
		}
		"give" => {
			parts.concessions.push(parse_clause(&flag.name, tail)?);
			Ok(())
		}
		"case" => {
			if !is_token(tail) {
				return Err(ProbeError::BadFlag(flag.name.clone()));
			}
			set_once(&mut parts.case_name, tail.to_owned(), "case")
		}
		"request" => {
			if !is_token(tail) {
				return Err(ProbeError::BadFlag(flag.name.clone()));
			}
			set_once(&mut parts.request, tail.to_owned(), "request")
		}
		"active" => {
			let side = match tail {
				"receive" => Side::Receive,
				"give" => Side::Give,
				_ => return Err(ProbeError::BadFlag(flag.name.clone())),
			};
			set_once(&mut parts.active, side, "active")
		}
		_ => Err(ProbeError::BadFlag(flag.name.clone())),
	}
}

fn parse_clause(name: &str, tail: &str) -> Result<(u32, u64), ProbeError> {
	let Some((kind_text, target_text)) = tail.split_once('_') else {
		return Err(ProbeError::BadFlag(name.to_owned()));
	};
	if kind_text.is_empty()
		|| target_text.is_empty()
		|| !kind_text.bytes().all(|byte| byte.is_ascii_digit())
		|| !target_text.bytes().all(|byte| byte.is_ascii_digit())
		|| target_text.contains('_')
	{
		return Err(ProbeError::BadFlag(name.to_owned()));
	}
	let kind = kind_text
		.parse::<u32>()
		.map_err(|_| ProbeError::BadFlag(name.to_owned()))?;
	let target = target_text
		.parse::<u64>()
		.map_err(|_| ProbeError::BadFlag(name.to_owned()))?;
	if kind >= KIND_LIMIT {
		return Err(ProbeError::BadFlag(name.to_owned()));
	}
	Ok((kind, target))
}

fn set_once<T>(slot: &mut Option<T>, value: T, field: &'static str) -> Result<(), ProbeError> {
	if slot.is_some() {
		return Err(ProbeError::RepeatedField(field));
	}
	*slot = Some(value);
	Ok(())
}

fn is_tag(value: &str) -> bool {
	value.len() == 3
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_token(value: &str) -> bool {
	(1..=32).contains(&value.len())
		&& value
			.bytes()
			.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Packed tag identity, the same low-byte layout the dataplane reads. Byte 7
/// is set so the value is not a pointer and not an independent-country zero.
fn identity(tag: &str) -> u64 {
	let bytes = tag.as_bytes();
	u64::from_le_bytes([bytes[0], bytes[1], bytes[2], 0, 0, 0, 0, 1])
}

fn synthetic_clauses(clauses: &[(u32, u64)], base: usize) -> Vec<Clause> {
	clauses
		.iter()
		.enumerate()
		.map(|(index, (kind, target))| Clause {
			address: base + index * 0x10,
			kind: *kind,
			target: *target,
		})
		.collect()
}

fn clauses_field(clauses: &[(u32, u64)]) -> String {
	clauses
		.iter()
		.map(|(kind, target)| format!("{kind}:{target}"))
		.collect::<Vec<_>>()
		.join(",")
}

fn receipt_line(fields: &[(&str, &str)]) -> String {
	let mut line = String::from("FOCH_RECEIPT/1 {");
	for (index, (key, value)) in fields.iter().enumerate() {
		if index > 0 {
			line.push(',');
		}
		push_json_string(&mut line, key);
		line.push(':');
		push_json_string(&mut line, value);
	}
	line.push('}');
	line
}

fn push_json_string(out: &mut String, value: &str) {
	out.push('"');
	for character in value.chars() {
		match character {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			other if other.is_control() => {
				for encoded in other.encode_utf16(&mut [0; 2]) {
					out.push_str(&format!("\\u{encoded:04x}"));
				}
			}
			other => out.push(other),
		}
	}
	out.push('"');
}

#[cfg(test)]
mod tests {
	use super::*;

	fn flag(name: &str) -> CountryFlag {
		CountryFlag {
			name: name.to_owned(),
			set_at_hours: 1,
		}
	}

	fn country(tag: &str, names: &[&str]) -> CountryFlags {
		CountryFlags {
			country_tag: tag.to_owned(),
			flags: names.iter().copied().map(flag).collect(),
		}
	}

	fn armed(extra: &[&str]) -> CountryFlags {
		let mut names = vec![
			"foch_peace_probe_vs_FRA",
			"foch_peace_probe_demand_3_111",
			"foch_peace_probe_give_0_500",
			"foch_peace_probe_case_rosy",
			"foch_peace_probe_request_1",
		];
		names.extend(extra);
		country("SWE", &names)
	}

	#[test]
	fn a_complete_order_submits_the_mixed_plan_as_a_receipt() {
		let submission = interpret(&[armed(&[])]).unwrap().unwrap();
		assert_eq!(submission.actor, "SWE");
		assert_eq!(submission.opponent, "FRA");
		assert_eq!(submission.active, Side::Receive);
		assert_eq!(submission.plan.merged.len(), 1);
		assert_eq!(submission.plan.merged[0].kind, 0);
		assert_eq!(submission.plan.merged[0].target, 500);
		assert_eq!(submission.plan.restore[0].kind, 3);
		assert_eq!(
			submission.receipt(),
			concat!(
				r#"FOCH_RECEIPT/1 {"type":"mixed_probe","status":"submitted","case":"rosy","request":"1","#,
				r#""actor":"SWE","opponent":"FRA","active":"receive","demands":"3:111","concessions":"0:500","#,
				r#""merged_terms":"1","native_calls":"0"}"#
			)
		);
		let parsed = submission.receipt();
		assert!(parsed.starts_with("FOCH_RECEIPT/1 {"));
		for forbidden in [
			"plugin_id",
			"run_id",
			"timestamp_ms",
			"log_path",
			"native_calls\":\"1",
		] {
			assert!(!parsed.contains(forbidden), "{forbidden}");
		}
	}

	#[test]
	fn the_give_tab_merges_the_demands_into_the_active_offer() {
		let submission = interpret(&[armed(&["foch_peace_probe_active_give"])])
			.unwrap()
			.unwrap();
		assert_eq!(submission.active, Side::Give);
		assert_eq!(submission.plan.merged[0].kind, 3);
		assert_eq!(submission.plan.restore[0].kind, 0);
		assert!(submission.receipt().contains(r#""active":"give""#));
		assert!(submission.receipt().contains(r#""merged_terms":"1""#));
	}

	#[test]
	fn an_incomplete_order_waits_and_unrelated_flags_are_ignored() {
		let mut partial = armed(&[]);
		partial.flags.pop();
		assert_eq!(interpret(&[partial]).unwrap(), None);
		let mut extra = armed(&["some_mod_flag"]);
		extra.flags.insert(
			0,
			CountryFlag {
				name: "foch_peace_probe".to_owned(),
				set_at_hours: 0,
			},
		);
		assert!(interpret(&[extra]).unwrap().is_some());
		assert_eq!(
			interpret(&[country("FRA", &["ordinary_flag"])]).unwrap(),
			None
		);
	}

	#[test]
	fn a_malformed_or_ambiguous_order_is_refused() {
		assert_eq!(
			interpret(&[armed(&["foch_peace_probe_vs_ENG"])]),
			Err(ProbeError::RepeatedField("vs"))
		);
		assert!(matches!(
			interpret(&[armed(&["foch_peace_probe_demand_40_1"])]),
			Err(ProbeError::BadFlag(_))
		));
		assert!(matches!(
			interpret(&[armed(&["foch_peace_probe_active_maybe"])]),
			Err(ProbeError::BadFlag(_))
		));
		let self_peace = country(
			"FRA",
			&[
				"foch_peace_probe_vs_FRA",
				"foch_peace_probe_demand_3_1",
				"foch_peace_probe_give_0_1",
				"foch_peace_probe_case_rosy",
				"foch_peace_probe_request_1",
			],
		);
		assert!(matches!(
			interpret(&[self_peace]),
			Err(ProbeError::BadFlag(_))
		));
		assert_eq!(
			interpret(&[armed(&[]), armed(&[])]),
			Err(ProbeError::AmbiguousActors)
		);
	}

	#[test]
	#[cfg(windows)]
	fn synthetic_memory_reads_the_order_through_the_shared_dataplane() {
		use foch_runtime::dataplane::{Memory, layout};
		// Private flag layout walked by `CountryView::flags`.
		const FLAG_NAMES_RVA: usize = 0x2357d80;
		const FLAG_IDS: usize = 0x280;
		const FLAG_DATES: usize = 0x298;
		let labels = [
			"foch_peace_probe_vs_FRA",
			"foch_peace_probe_demand_3_111",
			"foch_peace_probe_give_0_500",
			"foch_peace_probe_case_rosy",
			"foch_peace_probe_request_1",
		];
		let payloads: Vec<Vec<u8>> = labels
			.iter()
			.map(|label| label.as_bytes().to_vec())
			.collect();
		let mut name_blob = vec![0u8; payloads.len() * 32];
		for (index, payload) in payloads.iter().enumerate() {
			let start = index * 32;
			name_blob[start..start + 8].copy_from_slice(&(payload.as_ptr() as u64).to_le_bytes());
			name_blob[start + 16..start + 24]
				.copy_from_slice(&(payload.len() as u64).to_le_bytes());
			name_blob[start + 24..start + 32]
				.copy_from_slice(&(payload.len() as u64).to_le_bytes());
		}
		let ids: Vec<u16> = (0..labels.len() as u16).collect();
		let dates = vec![1_500i32; labels.len()];
		let mut country = vec![0u64; (FLAG_DATES + 24) / 8];
		let mut image = vec![0u64; (FLAG_NAMES_RVA + 24) / 8];
		let mut context = vec![0u64; (layout::COUNTRIES_COUNT + 8) / 8];
		let mut manager = vec![0u64; (layout::country_manager::INDEX_ARRAY + 16) / 8];
		let country_addr = country.as_mut_ptr() as usize;
		let exe_base = image.as_mut_ptr() as usize;
		let context_addr = context.as_mut_ptr() as usize;
		let manager_addr = manager.as_mut_ptr() as usize;
		let pointers = vec![country_addr as u64];
		assert!(country_addr > 0x1_0000 && exe_base > 0x1_0000);
		write_qword(
			&mut country,
			layout::country::TAG,
			u64::from_le_bytes(*b"SWE\0\0\0\0\x01"),
		);
		write_vector(&mut country, FLAG_IDS, ids.as_ptr() as u64, ids.len(), 2);
		write_vector(
			&mut country,
			FLAG_DATES,
			dates.as_ptr() as u64,
			dates.len(),
			4,
		);
		write_vector(
			&mut image,
			FLAG_NAMES_RVA,
			name_blob.as_ptr() as u64,
			payloads.len(),
			32,
		);
		write_qword(&mut image, layout::CONTEXT_GLOBAL_RVA, context_addr as u64);
		write_qword(
			&mut image,
			layout::COUNTRY_MANAGER_GLOBAL_RVA,
			manager_addr as u64,
		);
		write_qword(
			&mut manager,
			layout::country_manager::INDEX_ARRAY,
			pointers.as_ptr() as u64,
		);
		write_i32(&mut context, layout::COUNTRIES_COUNT, 1);

		let submission = {
			std::hint::black_box((
				&payloads, &name_blob, &ids, &dates, &country, &image, &context, &manager,
				&pointers,
			));
			let game = Eu4Game {
				memory: Memory,
				exe_base,
				context: context_addr,
			};
			assert_eq!(
				Eu4Game::context_of(&Memory, exe_base, context_addr),
				Some(context_addr)
			);
			let snapshots = unsafe { snapshots(&game, layout::EXPECTED_EXE_SHA256) };
			interpret(&snapshots).unwrap().unwrap()
		};
		assert_eq!(submission.actor, "SWE");
		assert_eq!(submission.opponent, "FRA");
		assert!(submission.receipt().contains(r#""demands":"3:111""#));
		assert!(submission.receipt().contains(r#""native_calls":"0""#));
		std::hint::black_box((payloads, name_blob, ids, dates, pointers));
	}

	fn write_qword(buf: &mut [u64], byte_offset: usize, value: u64) {
		assert_eq!(byte_offset % 8, 0);
		buf[byte_offset / 8] = value;
	}

	fn write_i32(buf: &mut [u64], byte_offset: usize, value: i32) {
		let index = byte_offset / 8;
		let shift = (byte_offset % 8) * 8;
		let mask = 0xffff_ffffu64 << shift;
		buf[index] = (buf[index] & !mask) | ((value as u32 as u64) << shift);
	}

	fn write_vector(buf: &mut [u64], byte_offset: usize, begin: u64, count: usize, stride: usize) {
		let end = begin + u64::try_from(count * stride).unwrap();
		write_qword(buf, byte_offset, begin);
		write_qword(buf, byte_offset + 8, end);
		write_qword(buf, byte_offset + 16, end);
	}
}
