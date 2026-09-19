use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProbeResult;

pub(super) const PAGE_URL: &str = "https://steamcommunity.com/workshop/browse/?appid=236850&browsesort=mostrecent&section=readytouseitems&p=1&numperpage=30";
const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Selection {
	pub url: String,
	pub collected_at_unix: u64,
	pub page_blake3: String,
	pub items: Vec<Item>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Item {
	pub publishedfileid: String,
	pub title: String,
	pub time_updated: u64,
	pub file_size: String,
}

impl Selection {
	pub fn validate(&self, expected_count: usize) -> ProbeResult<()> {
		if self.items.len() != expected_count || expected_count == 0 {
			return Err(format!(
				"expected {expected_count} page items, got {}",
				self.items.len()
			)
			.into());
		}
		let mut ids: BTreeSet<&str> = BTreeSet::new();
		for item in &self.items {
			if item.publishedfileid.parse::<u64>()? == 0 || !ids.insert(&item.publishedfileid) {
				return Err("invalid or duplicate Workshop ID in selection".into());
			}
			item.file_size.parse::<u64>()?;
		}
		Ok(())
	}
}

pub(super) fn load_or_fetch(
	root: &Path,
	url: &str,
	expected_count: usize,
) -> ProbeResult<Selection> {
	let path = root.join("selection.json");
	let selection: Selection = if path.exists() {
		serde_json::from_slice(&fs::read(path)?)?
	} else {
		eprintln!("[workshop-probe] fetching newest Workshop page");
		let response = reqwest::blocking::Client::builder()
			.timeout(Duration::from_secs(45))
			.build()?
			.get(url)
			.send()?
			.error_for_status()?;
		let mut bytes: Vec<u8> = Vec::new();
		response.take(MAX_PAGE_BYTES + 1).read_to_end(&mut bytes)?;
		if bytes.len() as u64 > MAX_PAGE_BYTES {
			return Err("Workshop page exceeds the response size limit".into());
		}
		// Retain unrecognized HTML too, so layout changes are inspectable.
		fs::write(root.join("source-page.html"), &bytes)?;
		let selection = parse_page(std::str::from_utf8(&bytes)?, url, expected_count)?;
		super::write_json(&path, &selection)?;
		selection
	};
	selection.validate(expected_count)?;
	if selection.url != url {
		return Err(
			"saved selection belongs to another query; choose a new probe directory".into(),
		);
	}
	Ok(selection)
}

fn parse_page(html: &str, url: &str, expected_count: usize) -> ProbeResult<Selection> {
	let marker = "window.SSR.renderContext=JSON.parse(";
	let payload = html
		.split_once(marker)
		.ok_or("Workshop page has no supported SSR payload")?
		.1;
	let value: Value = serde_json::Deserializer::from_str(payload)
		.into_iter::<Value>()
		.next()
		.ok_or("empty Workshop SSR payload")??;
	let mut pages: Vec<Vec<Item>> = Vec::new();
	collect_pages(&value, 0, &mut pages)?;
	if pages.len() != 1 {
		return Err(format!("expected one first-page response, found {}", pages.len()).into());
	}
	let selection = Selection {
		url: url.into(),
		collected_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
		page_blake3: blake3::hash(html.as_bytes()).to_hex().to_string(),
		items: pages.pop().unwrap(),
	};
	selection.validate(expected_count)?;
	Ok(selection)
}

fn collect_pages(value: &Value, depth: usize, pages: &mut Vec<Vec<Item>>) -> ProbeResult<()> {
	if depth > 32 {
		return Err("Workshop SSR nesting exceeds limit".into());
	}
	match value {
		Value::Object(object) => {
			if object.get("current_page") == Some(&Value::from(1))
				&& let Some(Value::Array(items)) = object.get("results")
			{
				if object.get("eresult") != Some(&Value::from(1))
					|| items.iter().any(|item| item["consumer_appid"] != 236850)
				{
					return Err("Workshop response failed or contains another game's items".into());
				}
				pages.push(
					items
						.iter()
						.cloned()
						.map(serde_json::from_value)
						.collect::<Result<Vec<Item>, _>>()?,
				);
			} else {
				for child in object.values() {
					collect_pages(child, depth + 1, pages)?;
				}
			}
		}
		Value::Array(array) => {
			for child in array {
				collect_pages(child, depth + 1, pages)?;
			}
		}
		Value::String(text) if text.starts_with(['{', '[', '"']) => {
			if let Ok(decoded) = serde_json::from_str::<Value>(text) {
				collect_pages(&decoded, depth + 1, pages)?;
			}
		}
		_ => {}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn page_fetch_is_frozen_and_reused_without_another_network_request() {
		let root = tempfile::tempdir().unwrap();
		let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
		let url = format!("http://{}/page", server.server_addr());
		let response = json!({"eresult":1,"current_page":1,"results":[
			{"publishedfileid":"22","consumer_appid":236850,"title":"fixture","time_updated":1,"file_size":"2"}
		]});
		let html = format!(
			"window.SSR.renderContext=JSON.parse({});",
			serde_json::to_string(&response.to_string()).unwrap()
		);
		let worker = std::thread::spawn(move || {
			server
				.recv_timeout(Duration::from_secs(10))
				.unwrap()
				.unwrap()
				.respond(tiny_http::Response::from_string(html))
				.unwrap();
		});
		let first = load_or_fetch(root.path(), &url, 1).unwrap();
		worker.join().unwrap();
		let second = load_or_fetch(root.path(), &url, 1).unwrap();
		assert_eq!(first.page_blake3, second.page_blake3);
		assert_eq!(first.collected_at_unix, second.collected_at_unix);
		assert!(root.path().join("source-page.html").is_file());
	}

	#[test]
	fn page_decoder_preserves_order_and_rejects_incomplete_or_ambiguous_pages() {
		let response = json!({"eresult": 1, "current_page": 1, "results": [
			{"publishedfileid":"22", "consumer_appid":236850, "title":"B", "time_updated":1, "file_size":"2"},
			{"publishedfileid":"11", "consumer_appid":236850, "title":"A", "time_updated":1, "file_size":"3"}
		]});
		let encode = |value: Value| {
			format!(
				"<script>window.SSR.renderContext=JSON.parse({});</script>",
				serde_json::to_string(&json!({"queryData": value.to_string()}).to_string())
					.unwrap()
			)
		};
		let html = encode(response.clone());
		let selected = parse_page(&html, PAGE_URL, 2).unwrap();
		assert_eq!(
			selected
				.items
				.iter()
				.map(|item| item.publishedfileid.as_str())
				.collect::<Vec<_>>(),
			["22", "11"]
		);
		assert!(parse_page(&html, PAGE_URL, 30).is_err());
		assert!(
			parse_page(
				&encode(json!([response.clone(), response.clone()])),
				PAGE_URL,
				2
			)
			.is_err()
		);
		let mut duplicate = response;
		duplicate["results"][1]["publishedfileid"] = json!("22");
		assert!(parse_page(&encode(duplicate), PAGE_URL, 2).is_err());
		assert!(parse_page("access denied", PAGE_URL, 2).is_err());
	}
}
