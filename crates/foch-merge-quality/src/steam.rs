//! Steam Workshop discovery via the Steam Web API (reqwest blocking).
//!
//! Builds the corpus from:
//!   1. `IPublishedFileService/QueryFiles/v1/`  — keyword search for candidates.
//!   2. `ISteamRemoteStorage/GetPublishedFileDetails/v1/` — title / timestamps /
//!      subscription counts (POST, batched ≤50).
//!   3. `IPublishedFileService/GetDetails/v1/?includechildren=true` — required
//!      items (the mods the compatch patches) — far more reliable than regexing
//!      the free-text description.
//!
//! Testability: pure parsers (`parse_query_files`, `parse_details`,
//! `parse_children`, `build_cases`) are unit-tested with inline JSON fixtures —
//! no network needed. HTTP shells are thin wrappers that fetch JSON and
//! delegate to the parsers.
//!
//! Design note: `generated_at` is set to `0` — calling wall-clock functions is
//! explicitly avoided per the spec; consumers should treat `0` as "unknown".

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::CmdResult;
use crate::config::{EU4_APPID, tool_commit};
use crate::corpus::{Case, Corpus, PatchedMeta};
use crate::secrets;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Workshop search terms that capture EU4 compatibility patches across naming
/// conventions. Broad recall here; precision comes from the ≥2-mod-children
/// filter in `build_cases`.
const SEARCH_TERMS: &[&str] = &[
	"Compatch",
	"compatibility patch",
	"compat patch",
	"compatibility",
	"compat",
	"addon",
	"submod",
	"unofficial patch",
	"merge",
	"fix",
	"Anbennar",
	"MEIOU",
	"Voltaire",
	"overhaul",
];

/// Maximum Workshop items per paginated request.
const PER_PAGE: usize = 100;

/// Batch size for multi-item detail/children endpoints.
const BATCH: usize = 50;

// ---------------------------------------------------------------------------
// Internal record type
// ---------------------------------------------------------------------------

pub(crate) struct DetailRecord {
	pub(crate) id: String,
	pub(crate) title: String,
	pub(crate) time_created: i64,
	pub(crate) time_updated: i64,
	/// Lifetime subscriber count (prefers `lifetime_subscriptions` over `subscriptions`).
	pub(crate) subscriptions: i64,
}

// ---------------------------------------------------------------------------
// Numeric coercion helpers
//
// Steam returns `publishedfileid` as a JSON string and counts/timestamps as
// JSON numbers — but the string variant also appears for some numeric fields
// across endpoints. `as_i64()` silently returns `None` on a quoted number, so
// we always try both forms.
// ---------------------------------------------------------------------------

/// Accept a JSON value as `i64`, handling both JSON numbers and numeric strings.
fn coerce_i64(v: &Value) -> Option<i64> {
	if let Some(n) = v.as_i64() {
		return Some(n);
	}
	v.as_str()?.trim().parse().ok()
}

/// Read a field as `i64`, defaulting to `0` when absent or unparseable.
fn field_i64(obj: &Value, key: &str) -> i64 {
	obj.get(key).and_then(coerce_i64).unwrap_or(0)
}

/// Read a field as a `String`, accepting quoted strings and JSON integers
/// (Steam sometimes returns ids as numbers).
fn field_str(obj: &Value, key: &str) -> String {
	let v = match obj.get(key) {
		Some(v) => v,
		None => return String::new(),
	};
	if let Some(s) = v.as_str() {
		return s.to_string();
	}
	if let Some(n) = v.as_i64() {
		return n.to_string();
	}
	String::new()
}

// ---------------------------------------------------------------------------
// Pure parsers (unit-tested without network)
// ---------------------------------------------------------------------------

/// Parse a `IPublishedFileService/QueryFiles/v1/` **response** object.
///
/// `json` is the **inner** `response` value (the caller strips the outer
/// `{"response": …}` wrapper before calling). Returns `(ids, next_cursor)`.
pub(crate) fn parse_query_files(json: &Value) -> (Vec<String>, Option<String>) {
	let arr = json
		.get("publishedfiledetails")
		.and_then(|v| v.as_array())
		.map(|a| a.as_slice())
		.unwrap_or(&[]);

	let ids: Vec<String> = arr
		.iter()
		.filter_map(|d| {
			let raw = d.get("publishedfileid")?;
			if let Some(s) = raw.as_str()
				&& !s.is_empty()
			{
				return Some(s.to_string());
			}
			raw.as_i64().map(|n| n.to_string())
		})
		.collect();

	let cursor = json
		.get("next_cursor")
		.and_then(|v| v.as_str())
		.filter(|s| !s.is_empty())
		.map(|s| s.to_string());

	(ids, cursor)
}

/// Parse a `ISteamRemoteStorage/GetPublishedFileDetails/v1/` **response** object.
///
/// `json` is the inner `response` value. Missing or malformed entries are
/// silently skipped.
pub(crate) fn parse_details(json: &Value) -> Vec<DetailRecord> {
	let arr = json
		.get("publishedfiledetails")
		.and_then(|v| v.as_array())
		.map(|a| a.as_slice())
		.unwrap_or(&[]);

	arr.iter()
		.filter_map(|d| {
			let id_v = d.get("publishedfileid")?;
			let id = if let Some(s) = id_v.as_str() {
				s.to_string()
			} else {
				id_v.as_i64()?.to_string()
			};
			let title = field_str(d, "title");
			let time_created = field_i64(d, "time_created");
			let time_updated = field_i64(d, "time_updated");
			// `lifetime_subscriptions` = all-time count; `subscriptions` = current.
			// Prefer the former for popularity ranking.
			let subscriptions = d
				.get("lifetime_subscriptions")
				.and_then(coerce_i64)
				.or_else(|| d.get("subscriptions").and_then(coerce_i64))
				.unwrap_or(0);
			Some(DetailRecord {
				id,
				title,
				time_created,
				time_updated,
				subscriptions,
			})
		})
		.collect()
}

/// Parse the children of a single compatch entry from a
/// `IPublishedFileService/GetDetails/v1/?includechildren=true` **response** object.
///
/// `json` is the inner `response` value (containing the full `publishedfiledetails`
/// batch). This function locates the entry whose `publishedfileid == compatch_id`
/// and returns required-item mod ids with `file_type == 0`, excluding the
/// compatch itself.
///
/// The HTTP shell uses `parse_children_map` (which processes all entries at once);
/// this function is exposed for direct unit-testing.
#[allow(dead_code)]
pub(crate) fn parse_children(json: &Value, compatch_id: &str) -> Vec<String> {
	let arr = json
		.get("publishedfiledetails")
		.and_then(|v| v.as_array())
		.map(|a| a.as_slice())
		.unwrap_or(&[]);

	let entry = arr
		.iter()
		.find(|d| field_str(d, "publishedfileid") == compatch_id);

	let entry = match entry {
		Some(e) => e,
		None => return vec![],
	};

	entry
		.get("children")
		.and_then(|v| v.as_array())
		.map(|children| {
			children
				.iter()
				.filter_map(|c| {
					if field_i64(c, "file_type") != 0 {
						return None;
					}
					let fid = field_str(c, "publishedfileid");
					if fid.is_empty() || fid == compatch_id {
						return None;
					}
					Some(fid)
				})
				.collect()
		})
		.unwrap_or_default()
}

/// Parse the full batch into a `compatch_id → children` map.
///
/// Used by the HTTP shell to process an entire 50-item chunk at once,
/// sharing the same filter logic as `parse_children`.
fn parse_children_map(json: &Value) -> HashMap<String, Vec<String>> {
	let arr = json
		.get("publishedfiledetails")
		.and_then(|v| v.as_array())
		.map(|a| a.as_slice())
		.unwrap_or(&[]);

	arr.iter()
		.map(|d| {
			let cid = field_str(d, "publishedfileid");
			let children = d
				.get("children")
				.and_then(|v| v.as_array())
				.map(|children| {
					children
						.iter()
						.filter_map(|c| {
							if field_i64(c, "file_type") != 0 {
								return None;
							}
							let fid = field_str(c, "publishedfileid");
							if fid.is_empty() || fid == cid {
								return None;
							}
							Some(fid)
						})
						.collect()
				})
				.unwrap_or_default();
			(cid, children)
		})
		.collect()
}

/// Assemble [`Case`] entries from compatch details, children map, and patched-mod
/// details. Drops candidates with fewer than 2 patched mods.
///
/// **Divergence from Python harness**: the Python implementation additionally
/// falls back to regex-scanning the compatch description for Workshop URLs when
/// the `children` list has fewer than 2 entries. This implementation uses
/// children only, because the function signature does not receive description
/// text. In practice the children-based approach is more reliable.
pub(crate) fn build_cases(
	details: &HashMap<String, DetailRecord>,
	children_map: &HashMap<String, Vec<String>>,
	mod_details: &HashMap<String, DetailRecord>,
) -> Vec<Case> {
	let mut cases = Vec::new();

	for (cid, patched) in children_map {
		if patched.len() < 2 {
			continue;
		}
		let d = match details.get(cid) {
			Some(d) => d,
			None => continue,
		};
		let patched_meta: BTreeMap<String, PatchedMeta> = patched
			.iter()
			.map(|mid| {
				let m = mod_details.get(mid);
				(
					mid.clone(),
					PatchedMeta {
						title: m.map(|r| r.title.clone()).unwrap_or_default(),
						time_created: m.map(|r| r.time_created).unwrap_or(0),
						time_updated: m.map(|r| r.time_updated).unwrap_or(0),
					},
				)
			})
			.collect();

		cases.push(Case {
			compatch_id: cid.clone(),
			title: d.title.clone(),
			patched: patched.clone(),
			time_created: d.time_created,
			time_updated: d.time_updated,
			subscriptions: d.subscriptions,
			patched_meta,
		});
	}

	cases
}

// ---------------------------------------------------------------------------
// HTTP shells (thin; not unit-tested directly)
// ---------------------------------------------------------------------------

fn http_client() -> reqwest::blocking::Client {
	reqwest::blocking::Client::builder()
		.timeout(std::time::Duration::from_secs(60))
		.build()
		.expect("failed to build reqwest client")
}

/// Paginate `QueryFiles` for one search term → published-file ids.
/// Stops when the page is empty, the cursor is absent, or the cursor is
/// unchanged (safety guard against infinite loops).
fn query_files_paged(
	client: &reqwest::blocking::Client,
	key: &str,
	search_text: &str,
	max_items: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
	let mut ids: Vec<String> = Vec::new();
	let mut cursor = "*".to_string();
	let appid = EU4_APPID.to_string();
	let npp = PER_PAGE.to_string();

	while ids.len() < max_items {
		let resp: Value = client
			.get("https://api.steampowered.com/IPublishedFileService/QueryFiles/v1/")
			.query(&[
				("key", key),
				("appid", &appid),
				("search_text", search_text),
				("numperpage", &npp),
				("query_type", "12"),
				("cursor", &cursor),
			])
			.send()?
			.json()?;

		let response = resp.get("response").cloned().unwrap_or_default();

		let (page_ids, next_cursor) = parse_query_files(&response);
		let page_empty = page_ids.is_empty();
		ids.extend(page_ids);

		match next_cursor {
			None => break,
			Some(nc) if nc == cursor => break,
			Some(_) if page_empty => break,
			Some(nc) => cursor = nc,
		}
	}

	ids.truncate(max_items);
	Ok(ids)
}

/// Union `QueryFiles` ids across all `SEARCH_TERMS`, preserving insertion order.
fn discover_candidate_ids(
	client: &reqwest::blocking::Client,
	key: &str,
	per_term: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
	let mut order: Vec<String> = Vec::new();
	let mut seen: HashSet<String> = HashSet::new();

	for term in SEARCH_TERMS {
		let term_ids = query_files_paged(client, key, term, per_term)?;
		let count = term_ids.len();
		for id in term_ids {
			if seen.insert(id.clone()) {
				order.push(id);
			}
		}
		eprintln!("  [discover] {term:?}: {count} ids (union {})", seen.len());
	}

	Ok(order)
}

/// Batch `GetPublishedFileDetails` (POST, ≤50 per call) → `id → DetailRecord`.
fn fetch_details_all(
	client: &reqwest::blocking::Client,
	ids: &[String],
) -> Result<HashMap<String, DetailRecord>, Box<dyn std::error::Error>> {
	let mut out: HashMap<String, DetailRecord> = HashMap::new();
	let total = ids.len();

	for (chunk_start, chunk) in ids.chunks(BATCH).enumerate() {
		let offset = chunk_start * BATCH;
		let mut params: Vec<(String, String)> =
			vec![("itemcount".to_string(), chunk.len().to_string())];
		for (i, fid) in chunk.iter().enumerate() {
			params.push((format!("publishedfileids[{i}]"), fid.clone()));
		}

		let resp: Value = client
			.post("https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/")
			.form(&params)
			.send()?
			.json()?;

		let response = resp.get("response").cloned().unwrap_or_default();
		for rec in parse_details(&response) {
			out.insert(rec.id.clone(), rec);
		}

		eprintln!("  [details] {}/{total}", offset + chunk.len());
		std::thread::sleep(std::time::Duration::from_millis(200));
	}

	Ok(out)
}

/// Batch `GetDetails?includechildren=true` (GET, ≤50 per call) → `id → children`.
fn fetch_children_all(
	client: &reqwest::blocking::Client,
	key: &str,
	ids: &[String],
) -> Result<HashMap<String, Vec<String>>, Box<dyn std::error::Error>> {
	let mut out: HashMap<String, Vec<String>> = HashMap::new();
	let total = ids.len();

	for (chunk_start, chunk) in ids.chunks(BATCH).enumerate() {
		let offset = chunk_start * BATCH;
		let mut params: Vec<(String, String)> = vec![
			("key".to_string(), key.to_string()),
			("includechildren".to_string(), "true".to_string()),
		];
		for (i, fid) in chunk.iter().enumerate() {
			params.push((format!("publishedfileids[{i}]"), fid.clone()));
		}

		let resp: Value = client
			.get("https://api.steampowered.com/IPublishedFileService/GetDetails/v1/")
			.query(&params)
			.send()?
			.json()?;

		let response = resp.get("response").cloned().unwrap_or_default();
		out.extend(parse_children_map(&response));

		eprintln!("  [children] {}/{total}", offset + chunk.len());
		std::thread::sleep(std::time::Duration::from_millis(200));
	}

	Ok(out)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Discover EU4 compatches, pair each with the mods it patches, and write the
/// resulting corpus to `corpus_out`. `max_items` caps candidates per search term.
///
/// Requires `STEAM_API_KEY` in the environment (or `keyring get steam api_key`).
/// Returns an error if the key is unavailable.
pub fn discover(corpus_out: &Path, max_items: usize) -> CmdResult {
	let key = secrets::steam_api_key()
		.ok_or("Steam API key not found (set STEAM_API_KEY or `keyring set steam api_key`)")?;

	let client = http_client();

	eprintln!(
		"[1/5] discovering candidates across {} search terms (≤{max_items} per term)…",
		SEARCH_TERMS.len()
	);
	let cand_ids = discover_candidate_ids(&client, &key, max_items)?;

	eprintln!(
		"[2/5] {} unique candidates; fetching compatch metadata…",
		cand_ids.len()
	);
	let details = fetch_details_all(&client, &cand_ids)?;

	eprintln!("[3/5] fetching required-items (children) for accurate pairs…");
	let children_map = fetch_children_all(&client, &key, &cand_ids)?;

	// Collect patched-mod ids from multi-mod compatches only.
	let patched_all: Vec<String> = {
		let mut seen: HashSet<String> = HashSet::new();
		let mut order: Vec<String> = Vec::new();
		for (cid, patched) in &children_map {
			if patched.len() >= 2 {
				for mid in patched {
					if mid != cid && seen.insert(mid.clone()) {
						order.push(mid.clone());
					}
				}
			}
		}
		order
	};

	eprintln!("[4/5] fetching {} patched-mod details…", patched_all.len());
	let mod_details = fetch_details_all(&client, &patched_all)?;

	let cases = build_cases(&details, &children_map, &mod_details);
	eprintln!("[5/5] built {} multi-mod compatches", cases.len());

	let corpus = Corpus {
		// Wall-clock intentionally omitted per spec; 0 = "unknown".
		generated_at: 0,
		tool_commit: tool_commit().unwrap_or_default(),
		search_terms: SEARCH_TERMS.iter().map(|s| s.to_string()).collect(),
		cases,
	};

	let json = corpus.to_json_pretty()?;
	if let Some(parent) = corpus_out.parent() {
		std::fs::create_dir_all(parent)?;
	}
	std::fs::write(corpus_out, &json)?;
	Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	// -----------------------------------------------------------------------
	// parse_query_files
	// -----------------------------------------------------------------------

	/// Basic case: three string ids + a cursor.
	#[test]
	fn parse_query_files_ids_and_cursor() {
		let json = serde_json::json!({
			"publishedfiledetails": [
				{"publishedfileid": "111"},
				{"publishedfileid": "222"},
				{"publishedfileid": "333"}
			],
			"next_cursor": "AAABbbbbbb=="
		});
		let (ids, cursor) = parse_query_files(&json);
		assert_eq!(ids, vec!["111", "222", "333"]);
		assert_eq!(cursor, Some("AAABbbbbbb==".to_string()));
	}

	/// IDs may arrive as JSON integers — should be normalised to strings.
	#[test]
	fn parse_query_files_numeric_id() {
		let json = serde_json::json!({
			"publishedfiledetails": [
				{"publishedfileid": 9876543210_i64}
			]
		});
		let (ids, cursor) = parse_query_files(&json);
		assert_eq!(ids, vec!["9876543210"]);
		assert!(cursor.is_none());
	}

	/// No `publishedfiledetails` key → empty ids, no cursor.
	#[test]
	fn parse_query_files_empty_response() {
		let json = serde_json::json!({});
		let (ids, cursor) = parse_query_files(&json);
		assert!(ids.is_empty());
		assert!(cursor.is_none());
	}

	// -----------------------------------------------------------------------
	// parse_details
	// -----------------------------------------------------------------------

	/// title / timestamps / subscriptions; `lifetime_subscriptions` preferred.
	/// `time_updated` arrives as a numeric string (coerce_i64 coverage).
	#[test]
	fn parse_details_maps_fields_correctly() {
		let json = serde_json::json!({
			"publishedfiledetails": [{
				"publishedfileid": "42",
				"title": "My Compatch",
				"time_created": 1_700_000_000_i64,
				"time_updated": "1700001000",   // numeric string — coerce_i64 coverage
				"lifetime_subscriptions": 500,
				"subscriptions": 400
			}]
		});
		let recs = parse_details(&json);
		assert_eq!(recs.len(), 1);
		let r = &recs[0];
		assert_eq!(r.id, "42");
		assert_eq!(r.title, "My Compatch");
		assert_eq!(r.time_created, 1_700_000_000);
		assert_eq!(r.time_updated, 1_700_001_000);
		assert_eq!(r.subscriptions, 500); // lifetime_subscriptions wins
	}

	/// Falls back to `subscriptions` when `lifetime_subscriptions` is absent.
	#[test]
	fn parse_details_subscriptions_fallback() {
		let json = serde_json::json!({
			"publishedfiledetails": [{
				"publishedfileid": "99",
				"title": "Patch",
				"subscriptions": 123
			}]
		});
		let recs = parse_details(&json);
		assert_eq!(recs[0].subscriptions, 123);
	}

	// -----------------------------------------------------------------------
	// parse_children
	// -----------------------------------------------------------------------

	/// Only `file_type == 0` entries are kept; the compatch's own id is excluded.
	#[test]
	fn parse_children_filters_type_and_self() {
		let json = serde_json::json!({
			"publishedfiledetails": [{
				"publishedfileid": "999",
				"children": [
					{"publishedfileid": "100", "file_type": 0},
					{"publishedfileid": "200", "file_type": 0},
					{"publishedfileid": "300", "file_type": 1},   // non-mod → excluded
					{"publishedfileid": "999", "file_type": 0}    // self → excluded
				]
			}]
		});
		let children = parse_children(&json, "999");
		assert_eq!(children, vec!["100", "200"]);
	}

	/// Compatch id not present in the batch → empty result (not a panic).
	#[test]
	fn parse_children_missing_id_returns_empty() {
		let json = serde_json::json!({
			"publishedfiledetails": [{
				"publishedfileid": "111",
				"children": [{"publishedfileid": "222", "file_type": 0}]
			}]
		});
		let children = parse_children(&json, "999");
		assert!(children.is_empty());
	}

	/// Entry with no `children` key → empty (not a panic).
	#[test]
	fn parse_children_no_children_field() {
		let json = serde_json::json!({
			"publishedfiledetails": [{"publishedfileid": "555"}]
		});
		let children = parse_children(&json, "555");
		assert!(children.is_empty());
	}

	// -----------------------------------------------------------------------
	// build_cases
	// -----------------------------------------------------------------------

	fn make_detail(id: &str, title: &str, tc: i64, tu: i64, subs: i64) -> DetailRecord {
		DetailRecord {
			id: id.to_string(),
			title: title.to_string(),
			time_created: tc,
			time_updated: tu,
			subscriptions: subs,
		}
	}

	/// ≥2 patched mods → one Case with populated `patched_meta`.
	#[test]
	fn build_cases_multi_mod_included() {
		let mut details = HashMap::new();
		details.insert(
			"999".to_string(),
			make_detail("999", "Great Compatch", 100, 200, 1000),
		);

		let mut children_map = HashMap::new();
		children_map.insert(
			"999".to_string(),
			vec!["100".to_string(), "200".to_string()],
		);

		let mut mod_details = HashMap::new();
		mod_details.insert(
			"100".to_string(),
			make_detail("100", "Mod A", 50, 150, 5000),
		);
		mod_details.insert(
			"200".to_string(),
			make_detail("200", "Mod B", 60, 160, 4000),
		);

		let cases = build_cases(&details, &children_map, &mod_details);
		assert_eq!(cases.len(), 1);

		let c = &cases[0];
		assert_eq!(c.compatch_id, "999");
		assert_eq!(c.title, "Great Compatch");
		assert_eq!(c.time_created, 100);
		assert_eq!(c.time_updated, 200);
		assert_eq!(c.subscriptions, 1000);
		assert_eq!(c.patched, vec!["100", "200"]);
		assert_eq!(c.patched_meta["100"].title, "Mod A");
		assert_eq!(c.patched_meta["100"].time_updated, 150);
		assert_eq!(c.patched_meta["200"].title, "Mod B");
		assert_eq!(c.patched_meta["200"].time_updated, 160);
	}

	/// <2 patched mods → dropped.
	#[test]
	fn build_cases_single_mod_dropped() {
		let mut details = HashMap::new();
		details.insert(
			"888".to_string(),
			make_detail("888", "Single Patch", 100, 200, 50),
		);

		let mut children_map = HashMap::new();
		children_map.insert("888".to_string(), vec!["100".to_string()]); // only 1 child

		let cases = build_cases(&details, &children_map, &HashMap::new());
		assert!(cases.is_empty());
	}

	/// 0 patched mods → dropped.
	#[test]
	fn build_cases_zero_mods_dropped() {
		let mut details = HashMap::new();
		details.insert("777".to_string(), make_detail("777", "Lonely", 0, 0, 0));

		let mut children_map = HashMap::new();
		children_map.insert("777".to_string(), vec![]);

		let cases = build_cases(&details, &children_map, &HashMap::new());
		assert!(cases.is_empty());
	}

	/// `details` missing entry for a candidate → that candidate is dropped.
	#[test]
	fn build_cases_missing_detail_dropped() {
		let children_map: HashMap<String, Vec<String>> = [(
			"no_detail_id".to_string(),
			vec!["A".to_string(), "B".to_string()],
		)]
		.into();

		let cases = build_cases(&HashMap::new(), &children_map, &HashMap::new());
		assert!(cases.is_empty());
	}

	/// `patched_meta` is present even when `mod_details` doesn't have the entry
	/// (title/timestamps default to empty/0).
	#[test]
	fn build_cases_missing_mod_detail_defaults() {
		let mut details = HashMap::new();
		details.insert("10".to_string(), make_detail("10", "CP", 1, 2, 3));

		let mut children_map = HashMap::new();
		children_map.insert("10".to_string(), vec!["A".to_string(), "B".to_string()]);

		let cases = build_cases(&details, &children_map, &HashMap::new());
		assert_eq!(cases.len(), 1);
		assert_eq!(cases[0].patched_meta["A"].title, "");
		assert_eq!(cases[0].patched_meta["B"].time_created, 0);
	}
}
