//! Frozen, revision-addressed EU4 modding-wiki knowledge packs.
//!
//! Acquisition is optional (`wiki`); archive verification, chunk derivation,
//! and bounded lexical search are fully offline.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use bm25::{Document, SearchEngineBuilder, Tokenizer};
use serde::{Deserialize, Serialize};

pub const MANIFEST_SCHEMA: &str = "1.0.0";
pub const PROFILE: &str = "eu4-modding";
pub const CHUNKER_VERSION: &str = "rendered-html-v1";
pub const MAX_ARCHIVE_BYTES: u64 = 30 * 1024 * 1024;
pub const MAX_SEARCH_LIMIT: usize = 50;
pub const MAX_SEARCH_CHARS: usize = 100_000;

const SITE: &str = "https://eu4.paradoxwikis.com";
const NAVBOX_TITLE: &str = "Template:Modding navbox";
const MAX_UNCOMPRESSED_BYTES: u64 = 160 * 1024 * 1024;
const MAX_CHUNK_CHARS: usize = 2_400;
const NOTICE_PATH: &str = "NOTICE.md";
const ATTRIBUTION_PATH: &str = "ATTRIBUTION.json";
const MANIFEST_PATH: &str = "manifest.json";
const CACHE_MANIFEST_PATH: &str = "manifest.json";
const CACHE_CHUNKS_PATH: &str = "chunks.json";
const LICENSE_URL: &str = "https://creativecommons.org/licenses/by-sa/3.0/";
const NOTICE: &str = "\
# EU4 Modding Wiki Knowledge Pack

The wiki text and rendered HTML in this archive are copied from the Europa
Universalis IV Wiki and are provided under the Creative Commons
Attribution-ShareAlike 3.0 license:
https://creativecommons.org/licenses/by-sa/3.0/

Each archived page records its title, exact revision, revision timestamp,
contributors, canonical URL, permanent revision URL, and history URL in
`ATTRIBUTION.json` and in the page record. The archive is an unmodified
revision-pinned snapshot apart from deterministic packaging. Derived text
chunks are not included in the archive.

This archive contains no images, revision histories, or embeddings.
";

#[derive(Debug)]
pub struct KnowledgeError {
	message: String,
}

impl KnowledgeError {
	fn invalid(message: impl Into<String>) -> Self {
		Self {
			message: message.into(),
		}
	}
}

impl fmt::Display for KnowledgeError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.message)
	}
}

impl std::error::Error for KnowledgeError {}

impl From<io::Error> for KnowledgeError {
	fn from(error: io::Error) -> Self {
		Self::invalid(error.to_string())
	}
}

impl From<serde_json::Error> for KnowledgeError {
	fn from(error: serde_json::Error) -> Self {
		Self::invalid(error.to_string())
	}
}

#[cfg(feature = "wiki")]
impl From<reqwest::Error> for KnowledgeError {
	fn from(error: reqwest::Error) -> Self {
		Self::invalid(error.to_string())
	}
}

pub type KnowledgeResult<T> = Result<T, KnowledgeError>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WikiTransport {
	DirectMediawikiApi,
	JinaAiMarkdownEnvelope,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct PageTransport {
	pub revision: WikiTransport,
	pub rendered_html: WikiTransport,
	pub contributors: Vec<WikiTransport>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct Contributor {
	pub name: String,
	pub user_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WikiSection {
	pub index: String,
	pub level: u8,
	pub title: String,
	pub anchor: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WikiPageRevision {
	pub title: String,
	pub page_id: u64,
	pub revision_id: u64,
	pub parent_revision_id: Option<u64>,
	pub timestamp: String,
	pub contributors: Vec<Contributor>,
	pub canonical_url: String,
	pub permanent_url: String,
	pub history_url: String,
	pub transport: PageTransport,
	pub raw_wikitext_hash: String,
	pub rendered_html_hash: String,
	pub raw_wikitext: String,
	pub rendered_html: String,
	pub sections: Vec<WikiSection>,
}

impl WikiPageRevision {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		title: String,
		page_id: u64,
		revision_id: u64,
		parent_revision_id: Option<u64>,
		timestamp: String,
		mut contributors: Vec<Contributor>,
		canonical_url: String,
		permanent_url: String,
		history_url: String,
		transport: PageTransport,
		raw_wikitext: String,
		rendered_html: String,
		mut sections: Vec<WikiSection>,
	) -> Self {
		contributors.sort();
		contributors.dedup();
		sections.sort_by(|left, right| {
			section_index_key(&left.index).cmp(&section_index_key(&right.index))
		});
		Self {
			title,
			page_id,
			revision_id,
			parent_revision_id,
			timestamp,
			contributors,
			canonical_url,
			permanent_url,
			history_url,
			transport,
			raw_wikitext_hash: digest(raw_wikitext.as_bytes()),
			rendered_html_hash: digest(rendered_html.as_bytes()),
			raw_wikitext,
			rendered_html,
			sections,
		}
	}

	fn validate(&self) -> KnowledgeResult<()> {
		if self.title.is_empty() || self.page_id == 0 || self.revision_id == 0 {
			return Err(KnowledgeError::invalid(
				"page title, page id, and revision id must be present",
			));
		}
		if self.timestamp.is_empty()
			|| self.canonical_url.is_empty()
			|| self.permanent_url.is_empty()
			|| self.history_url.is_empty()
		{
			return Err(KnowledgeError::invalid(format!(
				"page {} is missing revision or attribution metadata",
				self.title
			)));
		}
		if digest(self.raw_wikitext.as_bytes()) != self.raw_wikitext_hash {
			return Err(KnowledgeError::invalid(format!(
				"raw wikitext hash mismatch for {}",
				self.title
			)));
		}
		if digest(self.rendered_html.as_bytes()) != self.rendered_html_hash {
			return Err(KnowledgeError::invalid(format!(
				"rendered HTML hash mismatch for {}",
				self.title
			)));
		}
		if self.transport.contributors.is_empty() {
			return Err(KnowledgeError::invalid(format!(
				"page {} has no recorded contributor transport",
				self.title
			)));
		}
		Ok(())
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SelectionManifest {
	pub source_title: String,
	pub source_revision_id: u64,
	pub source_transport: WikiTransport,
	pub policy: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RightsManifest {
	pub license: String,
	pub license_url: String,
	pub notice_path: String,
	pub notice_hash: String,
	pub attribution_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ManifestPage {
	pub title: String,
	pub page_id: u64,
	pub revision_id: u64,
	pub timestamp: String,
	pub record_path: String,
	pub record_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct KnowledgeManifest {
	pub schema: String,
	pub profile: String,
	pub game_version: String,
	pub pack_id: String,
	pub site: String,
	pub snapshot_timestamp: String,
	pub selection: SelectionManifest,
	pub rights: RightsManifest,
	pub exclusions: Vec<String>,
	pub pages: Vec<ManifestPage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgePack {
	pub manifest: KnowledgeManifest,
	pub pages: Vec<WikiPageRevision>,
}

impl KnowledgePack {
	pub fn new(
		game_version: String,
		source_revision_id: u64,
		source_transport: WikiTransport,
		snapshot_timestamp: String,
		mut pages: Vec<WikiPageRevision>,
	) -> KnowledgeResult<Self> {
		let game_version = game_version.trim().to_string();
		if game_version.is_empty() {
			return Err(KnowledgeError::invalid(
				"knowledge pack game_version must not be empty",
			));
		}
		pages.sort_by(|left, right| {
			left.title
				.cmp(&right.title)
				.then(left.page_id.cmp(&right.page_id))
		});
		validate_page_set(&pages)?;
		let manifest_pages = pages
			.iter()
			.map(|page| {
				let bytes = canonical_json(page)?;
				Ok(ManifestPage {
					title: page.title.clone(),
					page_id: page.page_id,
					revision_id: page.revision_id,
					timestamp: page.timestamp.clone(),
					record_path: page_record_path(page),
					record_hash: digest(&bytes),
				})
			})
			.collect::<KnowledgeResult<Vec<_>>>()?;
		let mut manifest = KnowledgeManifest {
			schema: MANIFEST_SCHEMA.to_string(),
			profile: PROFILE.to_string(),
			game_version,
			pack_id: String::new(),
			site: SITE.to_string(),
			snapshot_timestamp,
			selection: SelectionManifest {
				source_title: NAVBOX_TITLE.to_string(),
				source_revision_id,
				source_transport,
				policy: "mainspace links in the rendered Modding navbox".to_string(),
			},
			rights: RightsManifest {
				license: "CC BY-SA 3.0".to_string(),
				license_url: LICENSE_URL.to_string(),
				notice_path: NOTICE_PATH.to_string(),
				notice_hash: digest(NOTICE.as_bytes()),
				attribution_path: ATTRIBUTION_PATH.to_string(),
			},
			exclusions: vec![
				"embeddings".to_string(),
				"images".to_string(),
				"revision-history".to_string(),
			],
			pages: manifest_pages,
		};
		manifest.pack_id = compute_pack_id(&manifest)?;
		let pack = Self { manifest, pages };
		validate_pack(&pack)?;
		Ok(pack)
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveReport {
	pub path: PathBuf,
	pub pack_id: String,
	pub archive_hash: String,
	pub archive_bytes: u64,
	pub page_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct Attribution {
	schema: String,
	profile: String,
	license: String,
	license_url: String,
	pages: Vec<PageAttribution>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct PageAttribution {
	title: String,
	page_id: u64,
	revision_id: u64,
	timestamp: String,
	contributors: Vec<Contributor>,
	canonical_url: String,
	permanent_url: String,
	history_url: String,
}

#[derive(Serialize)]
struct PackIdentity<'a> {
	schema: &'a str,
	profile: &'a str,
	game_version: &'a str,
	site: &'a str,
	snapshot_timestamp: &'a str,
	selection: &'a SelectionManifest,
	rights: &'a RightsManifest,
	exclusions: &'a [String],
	pages: &'a [ManifestPage],
}

pub fn write_knowledge_archive(
	pack: &KnowledgePack,
	output: impl AsRef<Path>,
) -> KnowledgeResult<ArchiveReport> {
	validate_pack(pack)?;
	let entries = archive_entries(pack)?;
	let bytes = encode_archive_entries(&entries)?;
	if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
		return Err(KnowledgeError::invalid(format!(
			"knowledge archive is {} bytes; hard limit is {MAX_ARCHIVE_BYTES} bytes",
			bytes.len()
		)));
	}
	let output = output.as_ref();
	if let Some(parent) = output.parent() {
		fs::create_dir_all(parent)?;
	}
	let parent = output.parent().unwrap_or_else(|| Path::new("."));
	let mut pending = tempfile::NamedTempFile::new_in(parent)?;
	pending.write_all(&bytes)?;
	pending.as_file_mut().sync_all()?;
	pending
		.persist(output)
		.map_err(|error| KnowledgeError::from(error.error))?;
	Ok(ArchiveReport {
		path: output.to_path_buf(),
		pack_id: pack.manifest.pack_id.clone(),
		archive_hash: digest(&bytes),
		archive_bytes: bytes.len() as u64,
		page_count: pack.pages.len(),
	})
}

pub fn verify_knowledge_archive(archive_path: impl AsRef<Path>) -> KnowledgeResult<KnowledgePack> {
	let archive_path = archive_path.as_ref();
	let archive_bytes = fs::metadata(archive_path)?.len();
	if archive_bytes > MAX_ARCHIVE_BYTES {
		return Err(KnowledgeError::invalid(format!(
			"knowledge archive is {archive_bytes} bytes; hard limit is {MAX_ARCHIVE_BYTES} bytes"
		)));
	}
	let entries = decode_archive_entries(File::open(archive_path)?)?;
	let manifest_bytes = entries
		.get(MANIFEST_PATH)
		.ok_or_else(|| KnowledgeError::invalid("knowledge archive has no manifest.json"))?;
	let manifest: KnowledgeManifest = serde_json::from_slice(manifest_bytes)?;
	validate_manifest(&manifest)?;

	let expected_paths: BTreeSet<String> = std::iter::once(MANIFEST_PATH.to_string())
		.chain(std::iter::once(manifest.rights.notice_path.clone()))
		.chain(std::iter::once(manifest.rights.attribution_path.clone()))
		.chain(manifest.pages.iter().map(|page| page.record_path.clone()))
		.collect();
	let actual_paths: BTreeSet<String> = entries.keys().cloned().collect();
	if actual_paths != expected_paths {
		return Err(KnowledgeError::invalid(format!(
			"knowledge archive entries do not match the manifest: expected {expected_paths:?}, got {actual_paths:?}"
		)));
	}

	let notice = entries
		.get(&manifest.rights.notice_path)
		.expect("expected paths were checked");
	if notice.as_slice() != NOTICE.as_bytes() || digest(notice) != manifest.rights.notice_hash {
		return Err(KnowledgeError::invalid(
			"knowledge archive license notice is missing or modified",
		));
	}

	let mut pages = Vec::with_capacity(manifest.pages.len());
	for manifest_page in &manifest.pages {
		let bytes = entries
			.get(&manifest_page.record_path)
			.expect("expected paths were checked");
		if digest(bytes) != manifest_page.record_hash {
			return Err(KnowledgeError::invalid(format!(
				"page record hash mismatch: {}",
				manifest_page.record_path
			)));
		}
		let page: WikiPageRevision = serde_json::from_slice(bytes)?;
		page.validate()?;
		if page.title != manifest_page.title
			|| page.page_id != manifest_page.page_id
			|| page.revision_id != manifest_page.revision_id
			|| page.timestamp != manifest_page.timestamp
			|| page_record_path(&page) != manifest_page.record_path
		{
			return Err(KnowledgeError::invalid(format!(
				"page record metadata mismatch: {}",
				manifest_page.record_path
			)));
		}
		pages.push(page);
	}
	let expected_attribution = canonical_json(&attribution_for(&manifest, &pages))?;
	let actual_attribution = entries
		.get(&manifest.rights.attribution_path)
		.expect("expected paths were checked");
	if actual_attribution.as_slice() != expected_attribution {
		return Err(KnowledgeError::invalid(
			"knowledge archive attribution is missing or modified",
		));
	}
	let pack = KnowledgePack { manifest, pages };
	validate_pack(&pack)?;
	Ok(pack)
}

fn validate_page_set(pages: &[WikiPageRevision]) -> KnowledgeResult<()> {
	if pages.is_empty() {
		return Err(KnowledgeError::invalid(
			"knowledge pack must contain at least one page",
		));
	}
	let mut page_ids = HashSet::new();
	let mut titles = HashSet::new();
	for page in pages {
		page.validate()?;
		if !page_ids.insert(page.page_id) {
			return Err(KnowledgeError::invalid(format!(
				"duplicate page id {}",
				page.page_id
			)));
		}
		if !titles.insert(page.title.clone()) {
			return Err(KnowledgeError::invalid(format!(
				"duplicate page title {}",
				page.title
			)));
		}
	}
	if !pages
		.windows(2)
		.all(|pair| (&pair[0].title, pair[0].page_id) < (&pair[1].title, pair[1].page_id))
	{
		return Err(KnowledgeError::invalid(
			"knowledge pages are not in canonical title order",
		));
	}
	Ok(())
}

fn validate_manifest(manifest: &KnowledgeManifest) -> KnowledgeResult<()> {
	if manifest.schema != MANIFEST_SCHEMA {
		return Err(KnowledgeError::invalid(format!(
			"unsupported knowledge manifest schema {}; expected {MANIFEST_SCHEMA}",
			manifest.schema
		)));
	}
	if manifest.profile != PROFILE {
		return Err(KnowledgeError::invalid(format!(
			"unsupported knowledge profile {}; expected {PROFILE}",
			manifest.profile
		)));
	}
	if manifest.game_version.trim().is_empty() {
		return Err(KnowledgeError::invalid(
			"knowledge manifest game_version must not be empty",
		));
	}
	if manifest.site != SITE
		|| manifest.selection.source_title != NAVBOX_TITLE
		|| manifest.selection.source_revision_id == 0
		|| manifest.snapshot_timestamp.is_empty()
	{
		return Err(KnowledgeError::invalid(
			"knowledge manifest has an invalid site or selection boundary",
		));
	}
	if manifest.rights.license != "CC BY-SA 3.0"
		|| manifest.rights.license_url != LICENSE_URL
		|| manifest.rights.notice_path != NOTICE_PATH
		|| manifest.rights.attribution_path != ATTRIBUTION_PATH
		|| manifest.rights.notice_hash != digest(NOTICE.as_bytes())
	{
		return Err(KnowledgeError::invalid(
			"knowledge manifest has invalid rights metadata",
		));
	}
	if manifest.exclusions
		!= ["embeddings", "images", "revision-history"]
			.map(str::to_string)
			.to_vec()
	{
		return Err(KnowledgeError::invalid(
			"knowledge manifest must exclude embeddings, images, and revision history",
		));
	}
	if manifest.pack_id != compute_pack_id(manifest)? {
		return Err(KnowledgeError::invalid(
			"knowledge manifest pack_id does not match its identity projection",
		));
	}
	if manifest.pages.is_empty()
		|| !manifest
			.pages
			.windows(2)
			.all(|pair| (&pair[0].title, pair[0].page_id) < (&pair[1].title, pair[1].page_id))
	{
		return Err(KnowledgeError::invalid(
			"knowledge manifest pages are empty or not canonically sorted",
		));
	}
	Ok(())
}

fn validate_pack(pack: &KnowledgePack) -> KnowledgeResult<()> {
	validate_manifest(&pack.manifest)?;
	validate_page_set(&pack.pages)?;
	if pack.manifest.pages.len() != pack.pages.len() {
		return Err(KnowledgeError::invalid(
			"knowledge manifest page count does not match page records",
		));
	}
	for (manifest_page, page) in pack.manifest.pages.iter().zip(&pack.pages) {
		let bytes = canonical_json(page)?;
		if manifest_page.title != page.title
			|| manifest_page.page_id != page.page_id
			|| manifest_page.revision_id != page.revision_id
			|| manifest_page.timestamp != page.timestamp
			|| manifest_page.record_path != page_record_path(page)
			|| manifest_page.record_hash != digest(&bytes)
		{
			return Err(KnowledgeError::invalid(format!(
				"manifest does not match page record {}",
				page.title
			)));
		}
	}
	Ok(())
}

fn compute_pack_id(manifest: &KnowledgeManifest) -> KnowledgeResult<String> {
	let identity = PackIdentity {
		schema: &manifest.schema,
		profile: &manifest.profile,
		game_version: &manifest.game_version,
		site: &manifest.site,
		snapshot_timestamp: &manifest.snapshot_timestamp,
		selection: &manifest.selection,
		rights: &manifest.rights,
		exclusions: &manifest.exclusions,
		pages: &manifest.pages,
	};
	let bytes = canonical_json(&identity)?;
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-knowledge-pack-v1\0");
	hasher.update(&bytes);
	Ok(hasher.finalize().to_hex().to_string())
}

fn page_record_path(page: &WikiPageRevision) -> String {
	format!("pages/{}-{}.json", page.page_id, page.revision_id)
}

fn attribution_for(manifest: &KnowledgeManifest, pages: &[WikiPageRevision]) -> Attribution {
	Attribution {
		schema: MANIFEST_SCHEMA.to_string(),
		profile: manifest.profile.clone(),
		license: manifest.rights.license.clone(),
		license_url: manifest.rights.license_url.clone(),
		pages: pages
			.iter()
			.map(|page| PageAttribution {
				title: page.title.clone(),
				page_id: page.page_id,
				revision_id: page.revision_id,
				timestamp: page.timestamp.clone(),
				contributors: page.contributors.clone(),
				canonical_url: page.canonical_url.clone(),
				permanent_url: page.permanent_url.clone(),
				history_url: page.history_url.clone(),
			})
			.collect(),
	}
}

fn archive_entries(pack: &KnowledgePack) -> KnowledgeResult<BTreeMap<String, Vec<u8>>> {
	let mut entries = BTreeMap::new();
	entries.insert(
		ATTRIBUTION_PATH.to_string(),
		canonical_json(&attribution_for(&pack.manifest, &pack.pages))?,
	);
	entries.insert(MANIFEST_PATH.to_string(), canonical_json(&pack.manifest)?);
	entries.insert(NOTICE_PATH.to_string(), NOTICE.as_bytes().to_vec());
	for page in &pack.pages {
		entries.insert(page_record_path(page), canonical_json(page)?);
	}
	Ok(entries)
}

fn encode_archive_entries(entries: &BTreeMap<String, Vec<u8>>) -> KnowledgeResult<Vec<u8>> {
	let encoder = zstd::Encoder::new(Vec::new(), 9)?;
	let mut builder = tar::Builder::new(encoder);
	for (path, bytes) in entries {
		validate_archive_path(Path::new(path))?;
		let mut header = tar::Header::new_gnu();
		header.set_entry_type(tar::EntryType::Regular);
		header.set_mode(0o644);
		header.set_uid(0);
		header.set_gid(0);
		header.set_mtime(0);
		header.set_username("")?;
		header.set_groupname("")?;
		header.set_size(bytes.len() as u64);
		header.set_cksum();
		builder.append_data(&mut header, path, bytes.as_slice())?;
	}
	let encoder = builder.into_inner()?;
	Ok(encoder.finish()?)
}

fn decode_archive_entries(reader: impl Read) -> KnowledgeResult<BTreeMap<String, Vec<u8>>> {
	let decoder = zstd::Decoder::new(reader)?;
	let mut bounded = decoder.take(MAX_UNCOMPRESSED_BYTES + 1);
	let mut tar_bytes = Vec::new();
	bounded.read_to_end(&mut tar_bytes)?;
	if tar_bytes.len() as u64 > MAX_UNCOMPRESSED_BYTES {
		return Err(KnowledgeError::invalid(format!(
			"knowledge archive expands beyond {MAX_UNCOMPRESSED_BYTES} bytes"
		)));
	}
	let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
	let mut entries = BTreeMap::new();
	let mut previous_path: Option<String> = None;
	for entry in archive.entries()? {
		let mut entry = entry?;
		if entry.header().entry_type() != tar::EntryType::Regular {
			return Err(KnowledgeError::invalid(
				"knowledge archive may contain only regular files",
			));
		}
		let path = entry.path()?.into_owned();
		validate_archive_path(&path)?;
		let path = path
			.to_str()
			.ok_or_else(|| KnowledgeError::invalid("archive path is not UTF-8"))?
			.to_string();
		if previous_path
			.as_ref()
			.is_some_and(|previous| previous >= &path)
		{
			return Err(KnowledgeError::invalid(
				"knowledge archive entries are duplicated or not sorted",
			));
		}
		previous_path = Some(path.clone());
		let header = entry.header();
		let username = header.username().map_err(|error| {
			KnowledgeError::invalid(format!(
				"archive entry {path} has an invalid username: {error}"
			))
		})?;
		let groupname = header.groupname().map_err(|error| {
			KnowledgeError::invalid(format!(
				"archive entry {path} has an invalid group name: {error}"
			))
		})?;
		if header.mode()? != 0o644
			|| header.uid()? != 0
			|| header.gid()? != 0
			|| header.mtime()? != 0
			|| username.is_some_and(|value| !value.is_empty())
			|| groupname.is_some_and(|value| !value.is_empty())
		{
			return Err(KnowledgeError::invalid(format!(
				"archive entry {path} does not use fixed metadata"
			)));
		}
		let mut bytes = Vec::new();
		entry.read_to_end(&mut bytes)?;
		entries.insert(path, bytes);
	}
	Ok(entries)
}

fn validate_archive_path(path: &Path) -> KnowledgeResult<()> {
	if path.is_absolute()
		|| path
			.components()
			.any(|component| !matches!(component, Component::Normal(_)))
	{
		return Err(KnowledgeError::invalid(format!(
			"unsafe archive path: {}",
			path.display()
		)));
	}
	Ok(())
}

fn canonical_json(value: &impl Serialize) -> KnowledgeResult<Vec<u8>> {
	let mut bytes = serde_json::to_vec_pretty(value)?;
	bytes.push(b'\n');
	Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
	blake3::hash(bytes).to_hex().to_string()
}

fn section_index_key(index: &str) -> Vec<u64> {
	index
		.split('.')
		.map(|part| part.parse::<u64>().unwrap_or(u64::MAX))
		.collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct KnowledgeChunk {
	pub chunk_id: String,
	pub game_version: String,
	pub page_id: u64,
	pub revision_id: u64,
	pub page_title: String,
	pub section: String,
	pub canonical_url: String,
	pub permanent_url: String,
	pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct ChunkCacheManifest {
	schema: String,
	profile: String,
	pack_id: String,
	chunker_version: String,
	chunks_path: String,
	chunks_hash: String,
	chunk_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkCache {
	pub directory: PathBuf,
	pub chunks: Vec<KnowledgeChunk>,
}

pub fn prepare_chunk_cache(
	archive_path: impl AsRef<Path>,
	dataset_root: impl AsRef<Path>,
) -> KnowledgeResult<ChunkCache> {
	let pack = verify_knowledge_archive(archive_path)?;
	let knowledge_root = dataset_root.as_ref().join(".work/knowledge");
	let directory = knowledge_root
		.join(&pack.manifest.pack_id)
		.join(CHUNKER_VERSION);
	let manifest_path = directory.join(CACHE_MANIFEST_PATH);
	let chunks_path = directory.join(CACHE_CHUNKS_PATH);
	if let Ok(manifest_bytes) = fs::read(&manifest_path)
		&& let Ok(manifest) = serde_json::from_slice::<ChunkCacheManifest>(&manifest_bytes)
		&& manifest.schema == MANIFEST_SCHEMA
		&& manifest.profile == PROFILE
		&& manifest.pack_id == pack.manifest.pack_id
		&& manifest.chunker_version == CHUNKER_VERSION
		&& manifest.chunks_path == CACHE_CHUNKS_PATH
		&& let Ok(chunks_bytes) = fs::read(&chunks_path)
		&& digest(&chunks_bytes) == manifest.chunks_hash
		&& let Ok(chunks) = serde_json::from_slice::<Vec<KnowledgeChunk>>(&chunks_bytes)
		&& chunks.len() == manifest.chunk_count
	{
		prune_obsolete_caches(&knowledge_root, &directory, PROFILE)?;
		return Ok(ChunkCache { directory, chunks });
	}

	let chunks = chunk_pages(&pack.pages, &pack.manifest.game_version)?;
	let chunks_bytes = canonical_json(&chunks)?;
	let cache_manifest = ChunkCacheManifest {
		schema: MANIFEST_SCHEMA.to_string(),
		profile: PROFILE.to_string(),
		pack_id: pack.manifest.pack_id.clone(),
		chunker_version: CHUNKER_VERSION.to_string(),
		chunks_path: CACHE_CHUNKS_PATH.to_string(),
		chunks_hash: digest(&chunks_bytes),
		chunk_count: chunks.len(),
	};
	fs::create_dir_all(&directory)?;
	atomic_write(&chunks_path, &chunks_bytes)?;
	atomic_write(&manifest_path, &canonical_json(&cache_manifest)?)?;
	prune_obsolete_caches(&knowledge_root, &directory, PROFILE)?;
	Ok(ChunkCache { directory, chunks })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> KnowledgeResult<()> {
	let parent = path
		.parent()
		.ok_or_else(|| KnowledgeError::invalid("cache path has no parent"))?;
	fs::create_dir_all(parent)?;
	let mut pending = tempfile::NamedTempFile::new_in(parent)?;
	pending.write_all(bytes)?;
	pending.as_file_mut().sync_all()?;
	pending
		.persist(path)
		.map_err(|error| KnowledgeError::from(error.error))?;
	Ok(())
}

fn prune_obsolete_caches(
	knowledge_root: &Path,
	active_directory: &Path,
	profile: &str,
) -> KnowledgeResult<()> {
	if !knowledge_root.is_dir() {
		return Ok(());
	}
	for pack_entry in fs::read_dir(knowledge_root)? {
		let pack_entry = pack_entry?;
		if !pack_entry.file_type()?.is_dir() {
			continue;
		}
		for version_entry in fs::read_dir(pack_entry.path())? {
			let version_entry = version_entry?;
			if !version_entry.file_type()?.is_dir() || version_entry.path() == active_directory {
				continue;
			}
			let manifest_path = version_entry.path().join(CACHE_MANIFEST_PATH);
			let is_same_profile = fs::read(manifest_path)
				.ok()
				.and_then(|bytes| serde_json::from_slice::<ChunkCacheManifest>(&bytes).ok())
				.is_some_and(|manifest| manifest.profile == profile);
			if is_same_profile {
				fs::remove_dir_all(version_entry.path())?;
			}
		}
		if fs::read_dir(pack_entry.path())?.next().is_none() {
			fs::remove_dir(pack_entry.path())?;
		}
	}
	Ok(())
}

fn chunk_pages(
	pages: &[WikiPageRevision],
	game_version: &str,
) -> KnowledgeResult<Vec<KnowledgeChunk>> {
	let mut chunks = Vec::new();
	for page in pages {
		let rendered =
			html2text::from_read(page.rendered_html.as_bytes(), 120).map_err(|error| {
				KnowledgeError::invalid(format!(
					"failed to render HTML for {}: {error}",
					page.title
				))
			})?;
		let sections = split_rendered_sections(page, &rendered);
		for (section, text) in sections {
			for (ordinal, chunk_text) in split_text_chunks(&text).into_iter().enumerate() {
				let mut hasher = blake3::Hasher::new();
				hasher.update(b"foch-knowledge-chunk-v1\0");
				hasher.update(&page.page_id.to_le_bytes());
				hasher.update(&page.revision_id.to_le_bytes());
				hasher.update(section.as_bytes());
				hasher.update(&(ordinal as u64).to_le_bytes());
				hasher.update(chunk_text.as_bytes());
				chunks.push(KnowledgeChunk {
					chunk_id: hasher.finalize().to_hex().to_string(),
					game_version: game_version.to_string(),
					page_id: page.page_id,
					revision_id: page.revision_id,
					page_title: page.title.clone(),
					section: section.clone(),
					canonical_url: page.canonical_url.clone(),
					permanent_url: page.permanent_url.clone(),
					text: chunk_text,
				});
			}
		}
	}
	chunks.sort_by(|left, right| {
		(
			left.page_title.as_str(),
			left.page_id,
			left.section.as_str(),
			&left.chunk_id,
		)
			.cmp(&(
				right.page_title.as_str(),
				right.page_id,
				right.section.as_str(),
				&right.chunk_id,
			))
	});
	Ok(chunks)
}

fn split_rendered_sections(page: &WikiPageRevision, rendered: &str) -> Vec<(String, String)> {
	let section_titles: BTreeMap<String, String> = page
		.sections
		.iter()
		.map(|section| (normalize_heading(&section.title), section.title.clone()))
		.collect();
	let mut output = Vec::new();
	let mut current_title = "Lead".to_string();
	let mut current_lines = Vec::new();
	for line in rendered.lines() {
		let normalized = normalize_heading(line);
		if let Some(title) = section_titles.get(&normalized)
			&& !normalized.is_empty()
		{
			push_section(&mut output, &current_title, &current_lines);
			current_title = title.clone();
			current_lines.clear();
			continue;
		}
		current_lines.push(line);
	}
	push_section(&mut output, &current_title, &current_lines);
	if output.is_empty() && !rendered.trim().is_empty() {
		output.push(("Lead".to_string(), rendered.trim().to_string()));
	}
	output
}

fn normalize_heading(input: &str) -> String {
	input
		.trim()
		.trim_matches(|character: char| matches!(character, '#' | '*' | '=' | '-' | ' '))
		.trim()
		.to_ascii_lowercase()
}

fn push_section(output: &mut Vec<(String, String)>, title: &str, lines: &[&str]) {
	let text = lines.join("\n").trim().to_string();
	if !text.is_empty() {
		output.push((title.to_string(), text));
	}
}

fn split_text_chunks(text: &str) -> Vec<String> {
	let mut chunks = Vec::new();
	let mut current = String::new();
	for paragraph in text
		.split("\n\n")
		.map(str::trim)
		.filter(|part| !part.is_empty())
	{
		let separator = usize::from(!current.is_empty()) * 2;
		if current.chars().count() + separator + paragraph.chars().count() <= MAX_CHUNK_CHARS {
			if !current.is_empty() {
				current.push_str("\n\n");
			}
			current.push_str(paragraph);
			continue;
		}
		if !current.is_empty() {
			chunks.push(std::mem::take(&mut current));
		}
		let mut rest = paragraph;
		while rest.chars().count() > MAX_CHUNK_CHARS {
			let split = char_boundary_at_or_before(rest, MAX_CHUNK_CHARS);
			chunks.push(rest[..split].trim().to_string());
			rest = rest[split..].trim_start();
		}
		current.push_str(rest);
	}
	if !current.is_empty() {
		chunks.push(current);
	}
	chunks
}

fn char_boundary_at_or_before(text: &str, maximum_chars: usize) -> usize {
	text.char_indices()
		.nth(maximum_chars)
		.map_or(text.len(), |(index, _)| index)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchOptions {
	pub limit: usize,
	pub max_chars: usize,
}

impl SearchOptions {
	pub fn bounded(limit: usize, max_chars: usize) -> Self {
		Self {
			limit: limit.min(MAX_SEARCH_LIMIT),
			max_chars: max_chars.min(MAX_SEARCH_CHARS),
		}
	}
}

impl Default for SearchOptions {
	fn default() -> Self {
		Self {
			limit: 10,
			max_chars: 12_000,
		}
	}
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SearchHit {
	pub score: f32,
	pub game_version: String,
	pub page_id: u64,
	pub revision_id: u64,
	pub page_title: String,
	pub section: String,
	pub permanent_url: String,
	pub text: String,
}

pub fn search_knowledge_archive(
	archive_path: impl AsRef<Path>,
	dataset_root: impl AsRef<Path>,
	query: &str,
	options: SearchOptions,
) -> KnowledgeResult<Vec<SearchHit>> {
	let cache = prepare_chunk_cache(archive_path, dataset_root)?;
	Ok(search_chunks(&cache.chunks, query, options))
}

fn search_chunks(chunks: &[KnowledgeChunk], query: &str, options: SearchOptions) -> Vec<SearchHit> {
	let options = SearchOptions::bounded(options.limit, options.max_chars);
	if chunks.is_empty()
		|| options.limit == 0
		|| options.max_chars == 0
		|| ClausewitzTokenizer.tokenize(query).is_empty()
	{
		return Vec::new();
	}
	let documents = chunks.iter().enumerate().map(|(index, chunk)| {
		Document::new(
			index,
			format!("{}\n{}\n{}", chunk.page_title, chunk.section, chunk.text),
		)
	});
	let engine =
		SearchEngineBuilder::<usize, u32, ClausewitzTokenizer>::with_tokenizer_and_documents(
			ClausewitzTokenizer,
			documents,
		)
		.build();
	let mut ranked = engine.search(query, chunks.len());
	ranked.sort_by(|left, right| {
		right.score.total_cmp(&left.score).then_with(|| {
			chunks[left.document.id]
				.chunk_id
				.cmp(&chunks[right.document.id].chunk_id)
		})
	});
	let mut remaining_chars = options.max_chars;
	let mut hits = Vec::new();
	for result in ranked.into_iter().take(options.limit) {
		if remaining_chars == 0 {
			break;
		}
		let chunk = &chunks[result.document.id];
		let text = truncate_chars(&chunk.text, remaining_chars);
		let used = text.chars().count();
		if used == 0 {
			break;
		}
		remaining_chars -= used;
		hits.push(SearchHit {
			score: result.score,
			game_version: chunk.game_version.clone(),
			page_id: chunk.page_id,
			revision_id: chunk.revision_id,
			page_title: chunk.page_title.clone(),
			section: chunk.section.clone(),
			permanent_url: chunk.permanent_url.clone(),
			text,
		});
	}
	hits
}

fn truncate_chars(text: &str, maximum_chars: usize) -> String {
	if text.chars().count() <= maximum_chars {
		return text.to_string();
	}
	let end = char_boundary_at_or_before(text, maximum_chars);
	text[..end].to_string()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClausewitzTokenizer;

impl Tokenizer for ClausewitzTokenizer {
	fn tokenize(&self, input_text: &str) -> Vec<String> {
		let mut tokens = Vec::new();
		let mut current = String::new();
		let flush = |current: &mut String, tokens: &mut Vec<String>| {
			let trimmed = current.trim_matches(|character: char| {
				matches!(character, '.' | '/' | ':' | '@' | '-' | '$' | '?')
			});
			if trimmed.is_empty() {
				current.clear();
				return;
			}
			let full = trimmed.to_ascii_lowercase();
			tokens.push(full.clone());
			for component in full
				.split(['/', ':', '.', '@'])
				.filter(|component| !component.is_empty() && *component != full)
			{
				tokens.push(component.to_string());
			}
			current.clear();
		};
		for character in input_text.chars() {
			if character.is_alphanumeric()
				|| matches!(character, '_' | '/' | '.' | ':' | '@' | '-' | '$' | '?')
			{
				current.push(character);
			} else {
				flush(&mut current, &mut tokens);
			}
		}
		flush(&mut current, &mut tokens);
		tokens
	}
}

pub fn parse_mediawiki_json(bytes: &[u8]) -> KnowledgeResult<(serde_json::Value, WikiTransport)> {
	if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
		if is_mediawiki_response(&value) {
			return Ok((value, WikiTransport::DirectMediawikiApi));
		}
		for key in ["data", "content", "markdown"] {
			if let Some(envelope) = value.get(key).and_then(serde_json::Value::as_str)
				&& let Ok(parsed) = parse_jina_markdown(envelope)
			{
				return Ok((parsed, WikiTransport::JinaAiMarkdownEnvelope));
			}
		}
	}
	let text = std::str::from_utf8(bytes)
		.map_err(|error| KnowledgeError::invalid(format!("API response is not UTF-8: {error}")))?;
	Ok((
		parse_jina_markdown(text)?,
		WikiTransport::JinaAiMarkdownEnvelope,
	))
}

fn is_mediawiki_response(value: &serde_json::Value) -> bool {
	value.get("query").is_some() || value.get("parse").is_some() || value.get("error").is_some()
}

fn parse_jina_markdown(text: &str) -> KnowledgeResult<serde_json::Value> {
	let marker = "Markdown Content:";
	let marker_index = text.find(marker).ok_or_else(|| {
		KnowledgeError::invalid("response is neither MediaWiki JSON nor a Jina envelope")
	})?;
	let mut payload = text[marker_index + marker.len()..].trim();
	if let Some(after_fence) = payload.strip_prefix("```") {
		payload = after_fence.trim_start();
		if let Some(after_language) = payload.strip_prefix("json") {
			payload = after_language.trim_start();
		}
		let fence = payload
			.rfind("```")
			.ok_or_else(|| KnowledgeError::invalid("Jina JSON code fence is not closed"))?;
		payload = payload[..fence].trim();
	}
	let value: serde_json::Value = serde_json::from_str(payload)?;
	if !is_mediawiki_response(&value) {
		return Err(KnowledgeError::invalid(
			"Jina envelope does not contain a MediaWiki JSON response",
		));
	}
	Ok(value)
}

#[cfg(feature = "wiki")]
fn jina_proxy_url(prefix: &str, direct_url: &str) -> String {
	format!(
		"{}/{}",
		prefix.trim_end_matches('/'),
		direct_url.replace('&', "%26")
	)
}

#[cfg(feature = "wiki")]
#[derive(Debug, Default)]
struct DirectTransportState {
	unavailable: bool,
}

#[cfg(feature = "wiki")]
impl DirectTransportState {
	fn should_attempt(&self) -> bool {
		!self.unavailable
	}

	fn record_non_mediawiki_response(&mut self) {
		self.unavailable = true;
	}
}

#[cfg(feature = "wiki")]
mod acquisition {
	use std::sync::Mutex;
	use std::time::{Duration, Instant};

	use reqwest::Url;
	use serde::Deserialize;

	use super::*;

	const DEFAULT_API_ENDPOINT: &str = "https://eu4.paradoxwikis.com/api.php";
	const DEFAULT_JINA_PREFIX: &str = "https://r.jina.ai/";

	#[derive(Clone, Debug)]
	pub struct WikiSnapshotOptions {
		pub output: PathBuf,
		pub game_version: String,
		pub api_endpoint: String,
		pub jina_prefix: Option<String>,
		pub user_agent: String,
		pub request_delay: Duration,
		pub maxlag: u32,
	}

	impl WikiSnapshotOptions {
		pub fn new(output: impl Into<PathBuf>, user_agent: impl Into<String>) -> Self {
			Self {
				output: output.into(),
				game_version: "auto".to_string(),
				api_endpoint: DEFAULT_API_ENDPOINT.to_string(),
				jina_prefix: Some(DEFAULT_JINA_PREFIX.to_string()),
				user_agent: user_agent.into(),
				request_delay: Duration::from_secs(15),
				maxlag: 5,
			}
		}
	}

	pub fn snapshot_eu4_modding(options: &WikiSnapshotOptions) -> KnowledgeResult<ArchiveReport> {
		let client = WikiClient::new(options)?;
		let navbox = client.navbox_pages()?;
		let mut pages = Vec::with_capacity(navbox.titles.len());
		for (index, title) in navbox.titles.iter().enumerate() {
			eprintln!(
				"[knowledge] page {}/{} {title}",
				index + 1,
				navbox.titles.len()
			);
			pages.push(client.fetch_page(title)?);
		}
		let pack = KnowledgePack::new(
			options.game_version.clone(),
			navbox.revision_id,
			navbox.transport,
			navbox.snapshot_timestamp,
			pages,
		)?;
		write_knowledge_archive(&pack, &options.output)
	}

	struct NavboxSelection {
		revision_id: u64,
		transport: WikiTransport,
		snapshot_timestamp: String,
		titles: Vec<String>,
	}

	struct WikiClient {
		client: reqwest::blocking::Client,
		endpoint: Url,
		jina_prefix: Option<String>,
		request_delay: Duration,
		last_request: Mutex<Option<Instant>>,
		direct_transport: Mutex<DirectTransportState>,
		maxlag: u32,
	}

	impl WikiClient {
		fn new(options: &WikiSnapshotOptions) -> KnowledgeResult<Self> {
			if options.user_agent.trim().is_empty() {
				return Err(KnowledgeError::invalid(
					"wiki acquisition requires a descriptive User-Agent",
				));
			}
			if options.game_version.trim().is_empty() {
				return Err(KnowledgeError::invalid(
					"wiki acquisition requires a non-empty game version",
				));
			}
			let endpoint = Url::parse(&options.api_endpoint).map_err(|error| {
				KnowledgeError::invalid(format!("invalid MediaWiki API endpoint: {error}"))
			})?;
			let client = reqwest::blocking::Client::builder()
				.user_agent(&options.user_agent)
				.timeout(Duration::from_secs(90))
				.build()?;
			Ok(Self {
				client,
				endpoint,
				jina_prefix: options.jina_prefix.clone(),
				request_delay: options.request_delay,
				last_request: Mutex::new(None),
				direct_transport: Mutex::new(DirectTransportState::default()),
				maxlag: options.maxlag,
			})
		}

		fn navbox_pages(&self) -> KnowledgeResult<NavboxSelection> {
			let params = vec![
				("action", "parse".to_string()),
				("page", NAVBOX_TITLE.to_string()),
				("prop", "links|revid".to_string()),
				("curtimestamp", "1".to_string()),
			];
			let response = self.get_json(&params)?;
			ensure_no_api_error(&response.value)?;
			let parsed: ParseResponse = serde_json::from_value(response.value)?;
			let mut titles = parsed
				.parse
				.links
				.into_iter()
				.filter(|link| link.namespace == 0 && link.exists.unwrap_or(true))
				.map(|link| link.title)
				.collect::<Vec<_>>();
			titles.sort();
			titles.dedup();
			if titles.is_empty() || parsed.parse.revision_id == 0 {
				return Err(KnowledgeError::invalid(
					"Template:Modding navbox returned no mainspace page links or revision",
				));
			}
			let snapshot_timestamp = parsed
				.current_timestamp
				.ok_or_else(|| KnowledgeError::invalid("MediaWiki omitted curtimestamp"))?;
			Ok(NavboxSelection {
				revision_id: parsed.parse.revision_id,
				transport: response.transport,
				snapshot_timestamp,
				titles,
			})
		}

		fn fetch_page(&self, title: &str) -> KnowledgeResult<WikiPageRevision> {
			let revision_response = self.get_json(&[
				("action", "query".to_string()),
				("prop", "info|revisions".to_string()),
				("inprop", "url".to_string()),
				("redirects", "1".to_string()),
				("rvlimit", "1".to_string()),
				("rvprop", "ids|timestamp|content".to_string()),
				("rvslots", "main".to_string()),
				("titles", title.to_string()),
			])?;
			ensure_no_api_error(&revision_response.value)?;
			let query: QueryResponse = serde_json::from_value(revision_response.value)?;
			let page = only_page(query.query.pages, title)?;
			if page.missing.unwrap_or(false) {
				return Err(KnowledgeError::invalid(format!(
					"wiki page is missing: {title}"
				)));
			}
			let revision = page
				.revisions
				.first()
				.ok_or_else(|| KnowledgeError::invalid(format!("page has no revision: {title}")))?;
			let raw_wikitext = revision_content(revision)?;

			let rendered_response = self.get_json(&[
				("action", "parse".to_string()),
				("oldid", revision.revision_id.to_string()),
				("prop", "text|sections|revid".to_string()),
			])?;
			ensure_no_api_error(&rendered_response.value)?;
			let rendered: RenderedParseResponse = serde_json::from_value(rendered_response.value)?;
			if rendered.parse.revision_id != revision.revision_id {
				return Err(KnowledgeError::invalid(format!(
					"rendered revision changed for {title}: expected {}, got {}",
					revision.revision_id, rendered.parse.revision_id
				)));
			}
			let rendered_html = api_text(&rendered.parse.text)?;
			let sections = rendered
				.parse
				.sections
				.iter()
				.map(parse_section)
				.collect::<KnowledgeResult<Vec<_>>>()?;

			let (contributors, contributor_transports) = self.fetch_contributors(page.page_id)?;
			let (canonical_url, permanent_url, history_url) = self.article_urls(
				&page.title,
				page.canonical_url.as_deref().or(page.full_url.as_deref()),
				revision.revision_id,
			)?;
			Ok(WikiPageRevision::new(
				page.title,
				page.page_id,
				revision.revision_id,
				(revision.parent_revision_id != 0).then_some(revision.parent_revision_id),
				revision.timestamp.clone(),
				contributors,
				canonical_url,
				permanent_url,
				history_url,
				PageTransport {
					revision: revision_response.transport,
					rendered_html: rendered_response.transport,
					contributors: contributor_transports,
				},
				raw_wikitext,
				rendered_html,
				sections,
			))
		}

		fn fetch_contributors(
			&self,
			page_id: u64,
		) -> KnowledgeResult<(Vec<Contributor>, Vec<WikiTransport>)> {
			let mut contributors = Vec::new();
			let mut transports = Vec::new();
			let mut continuation: Option<String> = None;
			loop {
				let mut params = vec![
					("action", "query".to_string()),
					("prop", "contributors".to_string()),
					("pageids", page_id.to_string()),
					("pclimit", "max".to_string()),
				];
				if let Some(value) = &continuation {
					params.push(("pccontinue", value.clone()));
				}
				let response = self.get_json(&params)?;
				transports.push(response.transport);
				ensure_no_api_error(&response.value)?;
				let query: QueryResponse = serde_json::from_value(response.value)?;
				let page = only_page(query.query.pages, &page_id.to_string())?;
				contributors.extend(
					page.contributors
						.into_iter()
						.map(|contributor| Contributor {
							name: contributor.name,
							user_id: contributor.user_id,
						}),
				);
				continuation = query
					.continuation
					.and_then(|values| values.get("pccontinue").cloned());
				if continuation.is_none() {
					break;
				}
			}
			contributors.sort();
			contributors.dedup();
			transports.sort();
			transports.dedup();
			Ok((contributors, transports))
		}

		fn article_urls(
			&self,
			title: &str,
			canonical: Option<&str>,
			revision_id: u64,
		) -> KnowledgeResult<(String, String, String)> {
			let canonical_url = if let Some(canonical) = canonical {
				canonical.to_string()
			} else {
				let mut url = self.endpoint.clone();
				url.set_query(None);
				url.set_path("/");
				url.path_segments_mut()
					.map_err(|_| KnowledgeError::invalid("API URL cannot be a base URL"))?
					.push(&title.replace(' ', "_"));
				url.to_string()
			};
			let mut permanent = self.endpoint.clone();
			permanent.set_query(None);
			permanent.set_path("/index.php");
			permanent
				.query_pairs_mut()
				.append_pair("title", title)
				.append_pair("oldid", &revision_id.to_string());
			let mut history = self.endpoint.clone();
			history.set_query(None);
			history.set_path("/index.php");
			history
				.query_pairs_mut()
				.append_pair("title", title)
				.append_pair("action", "history");
			Ok((canonical_url, permanent.to_string(), history.to_string()))
		}

		fn get_json(&self, parameters: &[(&str, String)]) -> KnowledgeResult<TransportResponse> {
			let mut url = self.endpoint.clone();
			{
				let mut query = url.query_pairs_mut();
				query.clear();
				for (key, value) in parameters {
					query.append_pair(key, value);
				}
				query
					.append_pair("format", "json")
					.append_pair("formatversion", "2")
					.append_pair("maxlag", &self.maxlag.to_string());
			}
			let should_attempt_direct = self
				.direct_transport
				.lock()
				.map_err(|_| KnowledgeError::invalid("direct transport state is poisoned"))?
				.should_attempt();
			let direct_error = if should_attempt_direct {
				match self.request_text(url.as_str()) {
					Ok(response) => match parse_mediawiki_json(&response.bytes) {
						Ok((value, transport)) => {
							return Ok(TransportResponse { value, transport });
						}
						Err(error) => {
							self.direct_transport
								.lock()
								.map_err(|_| {
									KnowledgeError::invalid("direct transport state is poisoned")
								})?
								.record_non_mediawiki_response();
							format!(
								"HTTP {} returned a non-MediaWiki response: {error}",
								response.status
							)
						}
					},
					Err(error) => error.to_string(),
				}
			} else {
				"direct transport disabled after an earlier non-MediaWiki response".to_string()
			};
			let Some(prefix) = &self.jina_prefix else {
				return Err(KnowledgeError::invalid(format!(
					"direct MediaWiki request failed: {direct_error}"
				)));
			};
			let fallback_url = jina_proxy_url(prefix, url.as_str());
			let response = self.request_text(&fallback_url).map_err(|error| {
				KnowledgeError::invalid(format!(
					"direct MediaWiki request failed ({direct_error}); Jina fallback failed ({error})"
				))
			})?;
			if !response.status.is_success() {
				return Err(KnowledgeError::invalid(format!(
					"Jina fallback returned HTTP {} after direct failure ({direct_error})",
					response.status
				)));
			}
			let (value, _) = parse_mediawiki_json(&response.bytes)?;
			Ok(TransportResponse {
				value,
				transport: WikiTransport::JinaAiMarkdownEnvelope,
			})
		}

		fn request_text(&self, url: &str) -> KnowledgeResult<HttpResponse> {
			let mut last_request = self
				.last_request
				.lock()
				.map_err(|_| KnowledgeError::invalid("wiki request limiter is poisoned"))?;
			if let Some(previous) = *last_request {
				let elapsed = previous.elapsed();
				if elapsed < self.request_delay {
					std::thread::sleep(self.request_delay - elapsed);
				}
			}
			let response = self.client.get(url).send()?;
			let status = response.status();
			let bytes = response.bytes()?.to_vec();
			*last_request = Some(Instant::now());
			Ok(HttpResponse { status, bytes })
		}
	}

	struct HttpResponse {
		status: reqwest::StatusCode,
		bytes: Vec<u8>,
	}

	struct TransportResponse {
		value: serde_json::Value,
		transport: WikiTransport,
	}

	#[derive(Deserialize)]
	struct ParseResponse {
		#[serde(rename = "curtimestamp")]
		current_timestamp: Option<String>,
		parse: NavboxParse,
	}

	#[derive(Deserialize)]
	struct NavboxParse {
		#[serde(rename = "revid")]
		revision_id: u64,
		links: Vec<ApiLink>,
	}

	#[derive(Deserialize)]
	struct ApiLink {
		#[serde(rename = "ns")]
		namespace: i64,
		title: String,
		exists: Option<bool>,
	}

	#[derive(Deserialize)]
	struct QueryResponse {
		query: ApiQuery,
		#[serde(rename = "continue")]
		continuation: Option<BTreeMap<String, String>>,
	}

	#[derive(Deserialize)]
	struct ApiQuery {
		pages: Vec<ApiPage>,
	}

	#[derive(Deserialize)]
	struct ApiPage {
		#[serde(rename = "pageid")]
		page_id: u64,
		title: String,
		missing: Option<bool>,
		#[serde(rename = "fullurl")]
		full_url: Option<String>,
		#[serde(rename = "canonicalurl")]
		canonical_url: Option<String>,
		#[serde(default)]
		revisions: Vec<ApiRevision>,
		#[serde(default)]
		contributors: Vec<ApiContributor>,
	}

	#[derive(Deserialize)]
	struct ApiRevision {
		#[serde(rename = "revid")]
		revision_id: u64,
		#[serde(rename = "parentid")]
		parent_revision_id: u64,
		timestamp: String,
		slots: BTreeMap<String, serde_json::Value>,
	}

	#[derive(Deserialize)]
	struct ApiContributor {
		name: String,
		#[serde(rename = "userid")]
		user_id: Option<u64>,
	}

	#[derive(Deserialize)]
	struct RenderedParseResponse {
		parse: RenderedParse,
	}

	#[derive(Deserialize)]
	struct RenderedParse {
		#[serde(rename = "revid")]
		revision_id: u64,
		text: serde_json::Value,
		#[serde(default)]
		sections: Vec<serde_json::Value>,
	}

	fn only_page(mut pages: Vec<ApiPage>, context: &str) -> KnowledgeResult<ApiPage> {
		if pages.len() != 1 {
			return Err(KnowledgeError::invalid(format!(
				"MediaWiki returned {} pages for {context}; expected one",
				pages.len()
			)));
		}
		Ok(pages.remove(0))
	}

	fn revision_content(revision: &ApiRevision) -> KnowledgeResult<String> {
		let main = revision
			.slots
			.get("main")
			.ok_or_else(|| KnowledgeError::invalid("revision has no main slot"))?;
		api_text(main)
	}

	fn api_text(value: &serde_json::Value) -> KnowledgeResult<String> {
		if let Some(text) = value.as_str() {
			return Ok(text.to_string());
		}
		for key in ["content", "*"] {
			if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
				return Ok(text.to_string());
			}
		}
		Err(KnowledgeError::invalid(
			"MediaWiki response has no text content",
		))
	}

	fn parse_section(value: &serde_json::Value) -> KnowledgeResult<WikiSection> {
		let index = value_string(value, "index")?;
		let level = value
			.get("level")
			.and_then(|level| {
				level
					.as_u64()
					.or_else(|| level.as_str().and_then(|level| level.parse().ok()))
			})
			.and_then(|level| u8::try_from(level).ok())
			.ok_or_else(|| KnowledgeError::invalid("MediaWiki section has invalid level"))?;
		Ok(WikiSection {
			index,
			level,
			title: value_string(value, "line")?,
			anchor: value_string(value, "anchor")?,
		})
	}

	fn value_string(value: &serde_json::Value, key: &str) -> KnowledgeResult<String> {
		value
			.get(key)
			.and_then(serde_json::Value::as_str)
			.map(str::to_string)
			.ok_or_else(|| KnowledgeError::invalid(format!("MediaWiki response is missing {key}")))
	}

	fn ensure_no_api_error(value: &serde_json::Value) -> KnowledgeResult<()> {
		if let Some(error) = value.get("error") {
			return Err(KnowledgeError::invalid(format!(
				"MediaWiki API error: {error}"
			)));
		}
		Ok(())
	}
}

#[cfg(feature = "wiki")]
pub use acquisition::{WikiSnapshotOptions, snapshot_eu4_modding};

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_page() -> WikiPageRevision {
		WikiPageRevision::new(
			"Event modding".to_string(),
			42,
			84,
			Some(83),
			"2026-07-27T00:00:00Z".to_string(),
			vec![Contributor {
				name: "ExampleEditor".to_string(),
				user_id: Some(7),
			}],
			"https://eu4.paradoxwikis.com/Event_modding".to_string(),
			"https://eu4.paradoxwikis.com/index.php?title=Event_modding&oldid=84".to_string(),
			"https://eu4.paradoxwikis.com/index.php?title=Event_modding&action=history".to_string(),
			PageTransport {
				revision: WikiTransport::DirectMediawikiApi,
				rendered_html: WikiTransport::DirectMediawikiApi,
				contributors: vec![WikiTransport::DirectMediawikiApi],
			},
			"country_event = { id = example.1 }\n".to_string(),
			"<h2>Scripted effects</h2><p>Use <code>country_event</code> from \
			 <code>common/scripted_effects/example_effect.txt</code> with \
			 <code>event_target:owner</code>.</p>"
				.to_string(),
			vec![WikiSection {
				index: "1".to_string(),
				level: 2,
				title: "Scripted effects".to_string(),
				anchor: "Scripted_effects".to_string(),
			}],
		)
	}

	fn sample_pack() -> KnowledgePack {
		KnowledgePack::new(
			"1.37.5".to_string(),
			12,
			WikiTransport::DirectMediawikiApi,
			"2026-07-27T01:00:00Z".to_string(),
			vec![sample_page()],
		)
		.unwrap()
	}

	#[test]
	fn parses_direct_and_jina_api_envelopes() {
		let direct = br#"{"query":{"pages":[]}}"#;
		let (value, transport) = parse_mediawiki_json(direct).unwrap();
		assert_eq!(transport, WikiTransport::DirectMediawikiApi);
		assert!(value.get("query").is_some());

		let jina = b"Title: api.php\nURL Source: https://example/api.php\n\
			Markdown Content:\n```json\n{\"parse\":{\"revid\":7}}\n```\n";
		let (value, transport) = parse_mediawiki_json(jina).unwrap();
		assert_eq!(transport, WikiTransport::JinaAiMarkdownEnvelope);
		assert_eq!(value["parse"]["revid"], 7);

		let wrapped = serde_json::json!({
			"data": "Title: API\nMarkdown Content:\n{\"query\":{\"pages\":[]}}"
		});
		let (value, transport) =
			parse_mediawiki_json(&serde_json::to_vec(&wrapped).unwrap()).unwrap();
		assert_eq!(transport, WikiTransport::JinaAiMarkdownEnvelope);
		assert!(value.get("query").is_some());
	}

	#[test]
	#[cfg(feature = "wiki")]
	fn direct_transport_stays_disabled_after_challenge() {
		let mut state = DirectTransportState::default();
		assert!(state.should_attempt());
		state.record_non_mediawiki_response();
		assert!(!state.should_attempt());
		assert!(!state.should_attempt());
	}

	#[test]
	#[cfg(feature = "wiki")]
	fn jina_proxy_url_preserves_the_inner_mediawiki_query() {
		let direct = "https://eu4.paradoxwikis.com/api.php?action=parse&page=Template%3AModding_navbox&prop=links%7Crevid";
		let proxied = jina_proxy_url("https://r.jina.ai/", direct);

		assert_eq!(
			proxied,
			"https://r.jina.ai/https://eu4.paradoxwikis.com/api.php?action=parse%26page=Template%3AModding_navbox%26prop=links%7Crevid"
		);
		assert!(!proxied.contains("&page="));
	}

	#[test]
	fn archive_is_deterministic_and_verifiable() {
		let pack = sample_pack();
		let other_version = KnowledgePack::new(
			"1.36.2".to_string(),
			12,
			WikiTransport::DirectMediawikiApi,
			"2026-07-27T01:00:00Z".to_string(),
			vec![sample_page()],
		)
		.unwrap();
		assert_ne!(pack.manifest.pack_id, other_version.manifest.pack_id);
		let first = tempfile::NamedTempFile::new().unwrap();
		let second = tempfile::NamedTempFile::new().unwrap();
		write_knowledge_archive(&pack, first.path()).unwrap();
		write_knowledge_archive(&pack, second.path()).unwrap();
		assert_eq!(
			fs::read(first.path()).unwrap(),
			fs::read(second.path()).unwrap()
		);
		let verified = verify_knowledge_archive(first.path()).unwrap();
		assert_eq!(verified.manifest.pack_id, pack.manifest.pack_id);
		assert_eq!(verified.manifest.game_version, "1.37.5");
	}

	#[test]
	fn verification_detects_tampered_page() {
		let pack = sample_pack();
		let mut entries = archive_entries(&pack).unwrap();
		let page_path = pack.manifest.pages[0].record_path.clone();
		let page = entries.get_mut(&page_path).unwrap();
		let position = page
			.windows("country_event".len())
			.position(|window| window == b"country_event")
			.unwrap();
		page[position] = b'X';
		let bytes = encode_archive_entries(&entries).unwrap();
		let archive = tempfile::NamedTempFile::new().unwrap();
		fs::write(archive.path(), bytes).unwrap();
		let error = verify_knowledge_archive(archive.path()).unwrap_err();
		assert!(error.to_string().contains("page record hash mismatch"));
	}

	#[test]
	fn rendered_html_chunking_preserves_clausewitz_identifiers() {
		let chunks = chunk_pages(&[sample_page()], "1.37.5").unwrap();
		assert_eq!(chunks.len(), 1);
		assert_eq!(chunks[0].section, "Scripted effects");
		assert!(chunks[0].text.contains("country_event"));
		assert!(
			chunks[0]
				.text
				.contains("common/scripted_effects/example_effect.txt")
		);
		assert!(chunks[0].text.contains("event_target:owner"));

		let tokens = ClausewitzTokenizer.tokenize(&chunks[0].text);
		assert!(tokens.contains(&"country_event".to_string()));
		assert!(tokens.contains(&"common/scripted_effects/example_effect.txt".to_string()));
		assert!(tokens.contains(&"event_target:owner".to_string()));
	}

	#[test]
	fn bm25_search_obeys_limit_and_character_budget() {
		let mut chunks = chunk_pages(&[sample_page()], "1.37.5").unwrap();
		let mut second = chunks[0].clone();
		second.chunk_id = "f".repeat(64);
		second.section = "Second".to_string();
		second.text = "country_event ".repeat(20);
		chunks.push(second);
		let hits = search_chunks(
			&chunks,
			"country_event",
			SearchOptions {
				limit: 1,
				max_chars: 24,
			},
		);
		assert_eq!(hits.len(), 1);
		assert_eq!(hits[0].game_version, "1.37.5");
		assert!(hits[0].text.chars().count() <= 24);
		assert_eq!(
			hits.iter()
				.map(|hit| hit.text.chars().count())
				.sum::<usize>(),
			24
		);
	}
}
