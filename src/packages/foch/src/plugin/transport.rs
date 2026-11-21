//! HTTPS and `gh` downloads for `foch plugin install`.
//!
//! Ordinary installs use `reqwest` (rustls) and never require the GitHub CLI.
//! `InstallTransport::Auto` tries HTTPS first and uses `gh` only when that
//! fails and `gh` is already on `PATH`. Mirror prefixes are HTTPS, except
//! loopback `http://127.0.0.1` and `http://localhost` used by tests. A pinned
//! SHA-256 is checked before any archive is returned, and every body is capped
//! at [`MAX_DOWNLOAD_BYTES`].

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Hard cap for one downloaded archive. Matches the zip reader’s expanded-byte cap.
pub const MAX_DOWNLOAD_BYTES: u64 = 32 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_API_BYTES: u64 = 1024 * 1024;
const CHINA_UTC_OFFSET_SECS: i32 = 8 * 60 * 60;

/// HTTPS prefixes tried, in order, before `https://github.com/...` when the
/// China mirror policy is selected.
pub const CHINA_MIRROR_PREFIXES: &[&str] = &["https://gh-proxy.com/", "https://ghfast.top/"];

/// How `foch plugin install` fetches a release asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallTransport {
	/// HTTPS first. `gh` runs only if HTTPS fails and the GitHub CLI is on `PATH`.
	Auto,
	/// HTTPS only. Never spawns `gh`.
	Http,
	/// `gh release download` only.
	Gh,
}

/// Which GitHub download URLs HTTPS should try.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MirrorPolicy {
	/// China when [`detect_china_region`] says so, otherwise GitHub directly.
	Auto,
	/// Always `https://github.com/...`.
	Direct,
	/// Curated China mirrors, then GitHub.
	China,
	/// Caller-supplied prefixes, then GitHub. Each prefix is validated.
	Custom(Vec<String>),
}

/// Locale and timezone inputs for [`MirrorPolicy::Auto`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionHint {
	pub locale_hints: Vec<String>,
	pub tz_offset_secs: Option<i32>,
}

/// Where a successful download came from. The display is the player-facing source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadRoute {
	Http { mirror: Option<String> },
	Gh,
}

impl fmt::Display for DownloadRoute {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Http {
				mirror: Some(prefix),
			} => write!(f, "via http (mirror {prefix})"),
			Self::Http { mirror: None } => write!(f, "via http (direct)"),
			Self::Gh => write!(f, "via gh"),
		}
	}
}

/// Bytes of one release asset and the source that produced them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedAsset {
	pub bytes: Vec<u8>,
	pub via: DownloadRoute,
}

/// A release resolved from the GitHub API (or an equivalent test server).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRelease {
	pub repo: String,
	pub tag: String,
	pub asset: String,
}

/// Mirrors to try, then optionally the canonical GitHub URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirrorChain {
	pub prefixes: Vec<String>,
	pub direct_fallback: bool,
}

/// GitHub REST base URL and how to authenticate. Tokens are never written into errors.
#[derive(Clone, Debug)]
pub struct ApiQuery {
	pub api_base: String,
	pub bearer: Bearer,
}

impl Default for ApiQuery {
	fn default() -> Self {
		Self {
			api_base: "https://api.github.com".to_string(),
			bearer: Bearer::Environment,
		}
	}
}

/// Where the GitHub API token comes from. `Debug` redacts explicit tokens.
#[derive(Clone)]
pub enum Bearer {
	Environment,
	Absent,
	Token(String),
}

impl fmt::Debug for Bearer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Environment => f.write_str("Environment"),
			Self::Absent => f.write_str("Absent"),
			Self::Token(_) => f.write_str("Token(\"***\")"),
		}
	}
}

/// Saved or selected download transport and mirror, shared by the CLI, TUI,
/// and desktop app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportConfig {
	pub transport: InstallTransport,
	pub mirror: MirrorPolicy,
}

impl Default for TransportConfig {
	fn default() -> Self {
		Self {
			transport: InstallTransport::Auto,
			mirror: MirrorPolicy::Auto,
		}
	}
}

/// Locale tags the host exposes for language and China-mirror detection.
pub fn system_locale_hints() -> Vec<String> {
	host_locale_hints()
}

/// Knobs for one install download. `region` and `asset_origin` override host
/// detection and `https://github.com` so tests can stay on loopback.
#[derive(Clone, Debug)]
pub struct FetchOptions {
	pub transport: InstallTransport,
	pub mirror: MirrorPolicy,
	pub sha256: Option<String>,
	pub region: Option<RegionHint>,
	pub asset_origin: Option<String>,
}

impl Default for FetchOptions {
	fn default() -> Self {
		Self {
			transport: InstallTransport::Auto,
			mirror: MirrorPolicy::Auto,
			sha256: None,
			region: None,
			asset_origin: None,
		}
	}
}

/// Why a download or mirror setting cannot be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
	InvalidTransport(String),
	InvalidMirror(String),
	InvalidSha256(String),
	DigestMismatch { expected: String, actual: String },
	DownloadTooLarge { limit: u64 },
	Spec(String),
	Api { status: u16, message: String },
	Network(String),
	Status(u16),
	Exhausted { attempts: Vec<String> },
	Gh(String),
	Both { http: String, gh: String },
	HttpOnly { detail: String },
}

impl fmt::Display for TransportError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidTransport(value) => {
				write!(f, "unknown transport {value}. Use auto, http, or gh")
			}
			Self::InvalidMirror(message) => write!(f, "{message}"),
			Self::InvalidSha256(value) => {
				write!(f, "SHA-256 must be 64 hexadecimal digits, not {value}")
			}
			Self::DigestMismatch { expected, actual } => write!(
				f,
				"SHA-256 mismatch: expected {expected}, got {actual}. The archive was not installed"
			),
			Self::DownloadTooLarge { limit } => {
				write!(
					f,
					"download exceeded the {limit} byte limit and was discarded"
				)
			}
			Self::Spec(message) | Self::Network(message) | Self::Gh(message) => {
				write!(f, "{message}")
			}
			Self::Api { status, message } => {
				write!(f, "{message}")?;
				if *status == 403 || *status == 429 {
					write!(
						f,
						" Set GITHUB_TOKEN or GH_TOKEN to authenticate, or retry with --transport gh if the GitHub CLI is installed"
					)?;
				}
				Ok(())
			}
			Self::Status(status) => write!(f, "HTTP {status}"),
			Self::Exhausted { attempts } => {
				write!(f, "could not download the release")?;
				for attempt in attempts {
					write!(f, "\n  - {attempt}")?;
				}
				Ok(())
			}
			Self::Both { http, gh } => {
				write!(f, "HTTPS download failed ({http}), then gh failed ({gh})")
			}
			Self::HttpOnly { detail } => write!(
				f,
				"{detail} HTTPS was the only transport used. gh is not on PATH and is not required. Set GITHUB_TOKEN or GH_TOKEN if GitHub rate-limited the API, or pass --transport gh when the GitHub CLI is installed"
			),
		}
	}
}

impl std::error::Error for TransportError {}

impl TransportError {
	fn is_fatal(&self) -> bool {
		matches!(
			self,
			Self::InvalidTransport(_)
				| Self::InvalidMirror(_)
				| Self::InvalidSha256(_)
				| Self::DigestMismatch { .. }
				| Self::Spec(_)
		)
	}
}

/// CLI value wins, then the environment, then `auto`.
pub fn select_transport(
	cli: Option<&str>,
	env_value: Option<&str>,
) -> Result<InstallTransport, TransportError> {
	let chosen = first_nonempty(cli, env_value).unwrap_or("auto");
	match chosen.to_ascii_lowercase().as_str() {
		"auto" => Ok(InstallTransport::Auto),
		"http" => Ok(InstallTransport::Http),
		"gh" => Ok(InstallTransport::Gh),
		other => Err(TransportError::InvalidTransport(other.to_string())),
	}
}

/// CLI values win, then `FOCH_PLUGIN_MIRROR`, then [`MirrorPolicy::Auto`].
///
/// One of `auto`, `direct`, `cn`, or `china` selects that policy. Anything else
/// is a mirror prefix, and commas or repeated values build [`MirrorPolicy::Custom`].
pub fn select_mirror(
	cli: &[String],
	env_value: Option<&str>,
) -> Result<MirrorPolicy, TransportError> {
	let parts = if cli.iter().any(|value| !value.trim().is_empty()) {
		split_mirror_list(cli.iter().map(String::as_str))
	} else if let Some(env_value) = env_value.map(str::trim).filter(|value| !value.is_empty()) {
		split_mirror_list(std::iter::once(env_value))
	} else {
		return Ok(MirrorPolicy::Auto);
	};
	policy_from_parts(&parts)
}

/// `true` when a hint names mainland China (`zh_CN` / `zh-CN` and the same
/// tags with a script), or when the offset is `+08:00` and a hint is Chinese
/// without an explicit Taiwan, Hong Kong, Macau, or Singapore region.
/// `+08:00` alone is not enough: that offset is also Singapore and Perth.
pub fn detect_china_region<S: AsRef<str>>(locale_hints: &[S], tz_offset_secs: Option<i32>) -> bool {
	let tags: Vec<String> = locale_hints
		.iter()
		.flat_map(|hint| locale_tags(hint.as_ref()))
		.collect();
	if tags.iter().any(|tag| is_zh_cn(tag)) {
		return true;
	}
	if tags.iter().any(|tag| is_non_mainland_chinese(tag)) {
		return false;
	}
	tz_offset_secs == Some(CHINA_UTC_OFFSET_SECS)
		&& tags.iter().any(|tag| is_unqualified_chinese(tag))
}

/// Resolve a policy to the prefixes HTTPS will try. `Direct` ignores China hints.
pub fn resolve_mirror_chain<S: AsRef<str>>(
	policy: &MirrorPolicy,
	locale_hints: &[S],
	tz_offset_secs: Option<i32>,
) -> Result<MirrorChain, TransportError> {
	let chosen = match policy {
		MirrorPolicy::Direct => ChosenMirror::Direct,
		MirrorPolicy::China => ChosenMirror::China,
		MirrorPolicy::Custom(prefixes) => ChosenMirror::Custom(prefixes.clone()),
		MirrorPolicy::Auto => {
			if detect_china_region(locale_hints, tz_offset_secs) {
				ChosenMirror::China
			} else {
				ChosenMirror::Direct
			}
		}
	};
	match chosen {
		ChosenMirror::Direct => Ok(MirrorChain {
			prefixes: Vec::new(),
			direct_fallback: true,
		}),
		ChosenMirror::China => Ok(MirrorChain {
			prefixes: normalize_all(CHINA_MIRROR_PREFIXES.iter().copied())?,
			direct_fallback: true,
		}),
		ChosenMirror::Custom(prefixes) => {
			if prefixes.is_empty() {
				return Err(TransportError::InvalidMirror(
					"mirror list is empty".to_string(),
				));
			}
			Ok(MirrorChain {
				prefixes: normalize_all(prefixes.iter().map(String::as_str))?,
				direct_fallback: true,
			})
		}
	}
}

/// `https://github.com/{repo}/releases/download/{tag}/{asset}`.
pub fn release_download_url(repo: &str, tag: &str, asset: &str) -> Result<String, TransportError> {
	let repo = encode_repo(repo)?;
	let tag = encode_segment(tag, "tag")?;
	let asset = encode_segment(asset, "asset")?;
	plain_file_name(asset.rsplit('/').next().unwrap_or(asset.as_str()))?;
	Ok(format!(
		"https://github.com/{repo}/releases/download/{tag}/{asset}"
	))
}

/// Prefix concatenation, or a single `{url}` placeholder (percent-encoded).
pub fn apply_mirror(prefix: &str, direct_url: &str) -> String {
	match prefix.find("{url}") {
		Some(index) => {
			let mut out = String::with_capacity(prefix.len() + direct_url.len() * 3);
			out.push_str(&prefix[..index]);
			push_encoded(&mut out, direct_url.as_bytes());
			out.push_str(&prefix[index + "{url}".len()..]);
			out
		}
		None => format!("{prefix}{direct_url}"),
	}
}

/// Compare `bytes` to a 64-digit hex digest. Mismatch is fail-closed.
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), TransportError> {
	let expected = normalize_sha256(expected)?;
	let actual = sha256_hex(bytes);
	if actual == expected {
		Ok(())
	} else {
		Err(TransportError::DigestMismatch { expected, actual })
	}
}

/// Download one release asset. SHA-256, when set, is checked before return.
pub fn fetch_release_asset(
	repo: &str,
	tag: &str,
	asset: &str,
	options: &FetchOptions,
) -> Result<FetchedAsset, TransportError> {
	if let Some(expected) = &options.sha256 {
		normalize_sha256(expected)?;
	}
	let expected = options.sha256.clone();
	with_gh_fallback(
		options.transport,
		|| fetch_http(repo, tag, asset, options, expected.as_deref()),
		|| fetch_gh(repo, tag, asset, expected.as_deref()),
	)
}

/// Resolve `latest` or a tag through the GitHub releases API.
pub fn resolve_dynamic_http(
	repo: &str,
	reference: Option<&str>,
	config: &ApiQuery,
) -> Result<ResolvedRelease, TransportError> {
	let (repo, tag) = dynamic_target(repo, reference)?;
	let url = api_release_url(&config.api_base, &repo, tag.as_deref())?;
	let client = http_client()?;
	let token = resolve_bearer(&config.bearer);
	let mut request = client
		.get(&url)
		.header("Accept", "application/vnd.github+json");
	if let Some(token) = token.as_deref() {
		request = request.bearer_auth(token);
	}
	let response = request
		.send()
		.map_err(|error| TransportError::Network(sanitize(&error.to_string(), token.as_deref())))?;
	let status = response.status().as_u16();
	let body = match read_capped(response, MAX_API_BYTES) {
		Ok(body) => body,
		Err(BodyError::TooLarge) => {
			return Err(TransportError::DownloadTooLarge {
				limit: MAX_API_BYTES,
			});
		}
		Err(BodyError::Io(error)) => {
			return Err(TransportError::Network(sanitize(
				&error.to_string(),
				token.as_deref(),
			)));
		}
	};
	let reference_label = tag.as_deref().unwrap_or("latest");
	if !(200..300).contains(&status) {
		return Err(TransportError::Api {
			status,
			message: format!(
				"GitHub API returned {status} for {repo}@{reference_label}{}",
				api_detail(&body)
			),
		});
	}
	let parsed: ApiRelease = serde_json::from_slice(&body).map_err(|error| {
		TransportError::Spec(format!("GitHub API returned invalid JSON: {error}"))
	})?;
	if parsed.tag_name.is_empty() {
		return Err(TransportError::Spec(format!(
			"{repo}@{reference_label} did not include a tag_name"
		)));
	}
	let names: Vec<String> = parsed
		.assets
		.iter()
		.map(|asset| asset.name.clone())
		.collect();
	let asset = select_zip_asset(&repo, &names).ok_or_else(|| {
		TransportError::Spec(format!(
			"{repo}@{} ships no zip asset (assets: {})",
			parsed.tag_name,
			names.join(", ")
		))
	})?;
	if parsed
		.assets
		.iter()
		.find(|entry| entry.name == asset)
		.and_then(|entry| entry.browser_download_url.as_deref())
		.is_some_and(str::is_empty)
	{
		return Err(TransportError::Spec(format!(
			"{repo}@{asset} has an empty browser_download_url"
		)));
	}
	Ok(ResolvedRelease {
		repo,
		tag: parsed.tag_name,
		asset,
	})
}

pub(crate) fn dynamic_target(
	repo: &str,
	reference: Option<&str>,
) -> Result<(String, Option<String>), TransportError> {
	let repo = repo.trim().trim_end_matches(".git");
	if repo.is_empty()
		|| !repo
			.split('/')
			.all(|segment| !segment.is_empty() && !segment.starts_with('.'))
	{
		return Err(TransportError::Spec(repo.to_string()));
	}
	let tag = match reference {
		None | Some("latest") => None,
		Some(tag)
			if tag.starts_with('v') || tag.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
		{
			Some(tag.to_string())
		}
		Some(other) => {
			return Err(TransportError::Spec(format!(
				"{repo}@{other}: install references are release tags or latest"
			)));
		}
	};
	Ok((repo.to_string(), tag))
}

pub(crate) fn select_zip_asset(repo: &str, names: &[String]) -> Option<String> {
	let short = repo.rsplit('/').next().unwrap_or_default().to_lowercase();
	names
		.iter()
		.find(|name| name.ends_with(".zip") && name.to_lowercase().contains(&short))
		.or_else(|| names.iter().find(|name| name.ends_with(".zip")))
		.cloned()
}

pub(crate) fn gh_download_args(repo: &str, tag: &str, asset: &str, dest: &Path) -> Vec<OsString> {
	vec![
		"release".into(),
		"download".into(),
		tag.into(),
		"--repo".into(),
		repo.into(),
		"--pattern".into(),
		asset.into(),
		"--dir".into(),
		dest.as_os_str().to_os_string(),
		"--clobber".into(),
	]
}

pub(crate) fn with_gh_fallback<T>(
	transport: InstallTransport,
	http: impl FnOnce() -> Result<T, TransportError>,
	gh: impl FnOnce() -> Result<T, TransportError>,
) -> Result<T, TransportError> {
	match transport {
		InstallTransport::Http => http(),
		InstallTransport::Gh => gh(),
		InstallTransport::Auto => match http() {
			Ok(value) => Ok(value),
			Err(error) if error.is_fatal() => Err(error),
			Err(http_error) => {
				if gh_available() {
					match gh() {
						Ok(value) => Ok(value),
						Err(error) if error.is_fatal() => Err(error),
						Err(gh_error) => Err(TransportError::Both {
							http: http_error.to_string(),
							gh: gh_error.to_string(),
						}),
					}
				} else {
					Err(sole_http(http_error))
				}
			}
		},
	}
}

enum ChosenMirror {
	Direct,
	China,
	Custom(Vec<String>),
}

#[derive(serde::Deserialize)]
struct ApiRelease {
	tag_name: String,
	#[serde(default)]
	assets: Vec<ApiAsset>,
}

#[derive(serde::Deserialize)]
struct ApiAsset {
	name: String,
	#[serde(default)]
	browser_download_url: Option<String>,
}

struct Attempt {
	url: String,
	mirror: Option<String>,
}

enum BodyError {
	TooLarge,
	Io(io::Error),
}

fn sole_http(error: TransportError) -> TransportError {
	match error {
		TransportError::Api { .. } => error,
		other => TransportError::HttpOnly {
			detail: other.to_string(),
		},
	}
}

fn first_nonempty<'a>(cli: Option<&'a str>, env_value: Option<&'a str>) -> Option<&'a str> {
	[cli, env_value]
		.into_iter()
		.flatten()
		.map(str::trim)
		.find(|value| !value.is_empty())
}

fn split_mirror_list<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
	values
		.flat_map(|value| value.split(','))
		.map(str::trim)
		.filter(|value| !value.is_empty())
		.map(str::to_string)
		.collect()
}

fn policy_from_parts(parts: &[String]) -> Result<MirrorPolicy, TransportError> {
	if parts.is_empty() {
		return Err(TransportError::InvalidMirror(
			"mirror list is empty".to_string(),
		));
	}
	if parts.len() == 1
		&& let Some(policy) = keyword_policy(&parts[0])
	{
		return Ok(policy);
	}
	if parts.iter().any(|part| keyword_policy(part).is_some()) {
		return Err(TransportError::InvalidMirror(
			"mirror policy cannot mix auto, direct, or cn with URLs".to_string(),
		));
	}
	Ok(MirrorPolicy::Custom(normalize_all(
		parts.iter().map(String::as_str),
	)?))
}

fn keyword_policy(value: &str) -> Option<MirrorPolicy> {
	match value.to_ascii_lowercase().as_str() {
		"auto" => Some(MirrorPolicy::Auto),
		"direct" => Some(MirrorPolicy::Direct),
		"cn" | "china" => Some(MirrorPolicy::China),
		_ => None,
	}
}

fn normalize_all<'a>(
	prefixes: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>, TransportError> {
	prefixes.map(normalize_mirror_prefix).collect()
}

pub(crate) fn normalize_mirror_prefix(raw: &str) -> Result<String, TransportError> {
	let raw = raw.trim();
	if raw.is_empty()
		|| !raw.is_ascii()
		|| raw
			.chars()
			.any(|c| c.is_ascii_whitespace() || c.is_control())
		|| raw.contains('\\')
		|| raw.contains('#')
	{
		return Err(TransportError::InvalidMirror(format!(
			"invalid mirror prefix {raw}: use an https:// URL"
		)));
	}
	let Some((scheme, rest)) = raw.split_once("://") else {
		return Err(TransportError::InvalidMirror(format!(
			"invalid mirror prefix {raw}: scheme is not allowed"
		)));
	};
	let scheme = scheme.to_ascii_lowercase();
	if scheme != "https" && scheme != "http" {
		return Err(TransportError::InvalidMirror(format!(
			"invalid mirror prefix {raw}: scheme is not allowed"
		)));
	}
	let (authority, path) = match rest.find('/') {
		Some(index) => (&rest[..index], &rest[index..]),
		None => (rest, ""),
	};
	let (host, port) = split_authority(authority).map_err(|message| {
		TransportError::InvalidMirror(format!("invalid mirror prefix {raw}: {message}"))
	})?;
	if scheme == "http" && !is_loopback_host(&host) {
		return Err(TransportError::InvalidMirror(format!(
			"invalid mirror prefix {raw}: http is only allowed for 127.0.0.1 and localhost"
		)));
	}
	let template = path.contains("{url}");
	if template {
		if path.matches("{url}").count() != 1
			|| path.matches('{').count() != 1
			|| path.matches('}').count() != 1
		{
			return Err(TransportError::InvalidMirror(format!(
				"invalid mirror prefix {raw}: {{url}} must appear once"
			)));
		}
		if !path.starts_with('/') {
			return Err(TransportError::InvalidMirror(format!(
				"invalid mirror prefix {raw}: a {{url}} template needs a path"
			)));
		}
	} else if path.contains('?') || path.contains('{') || path.contains('}') {
		return Err(TransportError::InvalidMirror(format!(
			"invalid mirror prefix {raw}: use a trailing slash or a {{url}} template"
		)));
	}
	let mut out = match port {
		Some(port) => format!("{scheme}://{host}:{port}"),
		None => format!("{scheme}://{host}"),
	};
	if template {
		out.push_str(path);
	} else if path.is_empty() || path == "/" {
		out.push('/');
	} else {
		out.push_str(path);
		if !path.ends_with('/') {
			out.push('/');
		}
	}
	Ok(out)
}

fn split_authority(authority: &str) -> Result<(String, Option<u16>), String> {
	if authority.is_empty() || authority.contains(['@', '?', '#', '{', '}', ' ']) {
		return Err("credentials, spaces, and placeholders are not allowed in the host".into());
	}
	if let Some(host) = authority.strip_prefix('[') {
		let Some((host, suffix)) = host.split_once(']') else {
			return Err("IPv6 host is invalid".into());
		};
		if host.is_empty() {
			return Err("host is missing".into());
		}
		return Ok((format!("[{host}]"), port_suffix(suffix)?));
	}
	if let Some((host, port)) = authority.rsplit_once(':') {
		if host.is_empty() {
			return Err("host is missing".into());
		}
		return Ok((host.to_string(), Some(parse_port(port)?)));
	}
	Ok((authority.to_string(), None))
}

fn port_suffix(suffix: &str) -> Result<Option<u16>, String> {
	if suffix.is_empty() {
		Ok(None)
	} else if let Some(port) = suffix.strip_prefix(':') {
		Ok(Some(parse_port(port)?))
	} else {
		Err("host is invalid".into())
	}
}

fn parse_port(port: &str) -> Result<u16, String> {
	if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
		return Err("port is invalid".into());
	}
	let port: u16 = port.parse().map_err(|_| "port is invalid".to_string())?;
	if port == 0 {
		return Err("port is invalid".into());
	}
	Ok(port)
}

fn is_loopback_host(host: &str) -> bool {
	host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1"
}

fn normalize_origin(raw: &str) -> Result<String, TransportError> {
	let prefix = normalize_mirror_prefix(raw)?;
	let trimmed = prefix.trim_end_matches('/');
	let Some((_, rest)) = trimmed.split_once("://") else {
		return Err(TransportError::InvalidMirror(format!(
			"{raw} is not an origin"
		)));
	};
	if rest.contains('/') {
		return Err(TransportError::InvalidMirror(format!(
			"{raw} must be an origin without a path"
		)));
	}
	Ok(trimmed.to_string())
}

fn locale_tags(hint: &str) -> Vec<String> {
	hint.split(['.', '@', ',', ';', ' ', '\t'])
		.filter(|part| !part.is_empty())
		.map(|part| part.to_ascii_lowercase().replace('-', "_"))
		.collect()
}

fn language_parts(tag: &str) -> Option<(&str, Vec<&str>)> {
	let mut parts = tag.split('_').filter(|part| !part.is_empty());
	let language = parts.next()?;
	Some((language, parts.collect()))
}

fn is_chinese_language(language: &str) -> bool {
	matches!(language, "zh" | "chinese")
}

fn is_zh_cn(tag: &str) -> bool {
	let Some((language, rest)) = language_parts(tag) else {
		return false;
	};
	is_chinese_language(language)
		&& rest
			.iter()
			.any(|part| matches!(*part, "cn" | "china" | "prc" | "mainland"))
}

fn is_non_mainland_chinese(tag: &str) -> bool {
	let Some((language, rest)) = language_parts(tag) else {
		return false;
	};
	is_chinese_language(language)
		&& rest.iter().any(|part| {
			matches!(
				*part,
				"tw" | "hk" | "mo" | "sg" | "taiwan" | "hongkong" | "macau" | "macao" | "singapore"
			)
		})
}

fn is_unqualified_chinese(tag: &str) -> bool {
	let Some((language, rest)) = language_parts(tag) else {
		return false;
	};
	is_chinese_language(language)
		&& rest
			.iter()
			.all(|part| matches!(*part, "hans" | "hant" | "simplified" | "traditional"))
}

fn encode_repo(repo: &str) -> Result<String, TransportError> {
	let mut encoded = Vec::new();
	for segment in repo.split('/') {
		encoded.push(encode_segment(segment, "repository")?);
	}
	if encoded.len() != 2 {
		return Err(TransportError::Spec(format!(
			"repository must be owner/name, not {repo}"
		)));
	}
	Ok(encoded.join("/"))
}

fn encode_segment(segment: &str, kind: &str) -> Result<String, TransportError> {
	if segment.is_empty()
		|| segment.contains(['/', '\\', '?', '#', '\0'])
		|| segment.chars().any(|c| c.is_whitespace() || c.is_control())
	{
		return Err(TransportError::Spec(format!("invalid {kind} {segment}")));
	}
	let mut out = String::with_capacity(segment.len());
	push_encoded(&mut out, segment.as_bytes());
	Ok(out)
}

fn plain_file_name(name: &str) -> Result<(), TransportError> {
	if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
		return Err(TransportError::Spec(format!(
			"invalid asset file name {name}"
		)));
	}
	Ok(())
}

fn push_encoded(out: &mut String, bytes: &[u8]) {
	for &byte in bytes {
		if is_unreserved(byte) {
			out.push(byte as char);
		} else {
			push_percent(out, byte);
		}
	}
}

fn push_percent(out: &mut String, byte: u8) {
	const HEX: &[u8; 16] = b"0123456789ABCDEF";
	out.push('%');
	out.push(HEX[(byte >> 4) as usize] as char);
	out.push(HEX[(byte & 0x0f) as usize] as char);
}

fn is_unreserved(byte: u8) -> bool {
	matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~')
}

fn normalize_sha256(expected: &str) -> Result<String, TransportError> {
	let expected = expected.trim();
	if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
		return Err(TransportError::InvalidSha256(expected.to_string()));
	}
	Ok(expected.to_ascii_lowercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let digest = Sha256::digest(bytes);
	let mut out = String::with_capacity(64);
	for byte in digest {
		out.push(HEX[(byte >> 4) as usize] as char);
		out.push(HEX[(byte & 0x0f) as usize] as char);
	}
	out
}

fn fetch_http(
	repo: &str,
	tag: &str,
	asset: &str,
	options: &FetchOptions,
	expected: Option<&str>,
) -> Result<FetchedAsset, TransportError> {
	let chain = match &options.region {
		Some(region) => {
			resolve_mirror_chain(&options.mirror, &region.locale_hints, region.tz_offset_secs)?
		}
		None if matches!(options.mirror, MirrorPolicy::Auto) => {
			resolve_mirror_chain(&options.mirror, &host_locale_hints(), host_tz_offset_secs())?
		}
		None => resolve_mirror_chain(&options.mirror, &[] as &[String], None)?,
	};
	let canonical = release_download_url(repo, tag, asset)?;
	let direct = match &options.asset_origin {
		Some(origin) => {
			let origin = normalize_origin(origin)?;
			format!(
				"{origin}{}",
				canonical.trim_start_matches("https://github.com")
			)
		}
		None => canonical.clone(),
	};
	let mut attempts = Vec::with_capacity(chain.prefixes.len() + 1);
	for prefix in &chain.prefixes {
		attempts.push(Attempt {
			url: apply_mirror(prefix, &canonical),
			mirror: Some(prefix.clone()),
		});
	}
	if chain.direct_fallback {
		attempts.push(Attempt {
			url: direct,
			mirror: None,
		});
	}
	let client = http_client()?;
	let mut failures = Vec::new();
	for attempt in attempts {
		if !url_allowed(&attempt.url) {
			return Err(TransportError::InvalidMirror(format!(
				"{} is not an allowed download URL",
				attempt.url
			)));
		}
		match download_once(&client, &attempt.url) {
			Ok(bytes) => {
				if let Some(expected) = expected {
					verify_sha256(&bytes, expected)?;
				}
				return Ok(FetchedAsset {
					bytes,
					via: DownloadRoute::Http {
						mirror: attempt.mirror,
					},
				});
			}
			Err(error) if error.is_fatal() => return Err(error),
			Err(error) => {
				let label = attempt.mirror.unwrap_or_else(|| "direct".to_string());
				failures.push(format!("{label}: {error}"));
			}
		}
	}
	Err(TransportError::Exhausted { attempts: failures })
}

fn fetch_gh(
	repo: &str,
	tag: &str,
	asset: &str,
	expected: Option<&str>,
) -> Result<FetchedAsset, TransportError> {
	plain_file_name(asset)?;
	let dir = tempfile::tempdir().map_err(|error| {
		TransportError::Gh(format!("could not create a download directory: {error}"))
	})?;
	let output = Command::new("gh")
		.args(gh_download_args(repo, tag, asset, dir.path()))
		.stdin(Stdio::null())
		.output()
		.map_err(|error| {
			if error.kind() == io::ErrorKind::NotFound {
				TransportError::Gh(
					"--transport gh needs the GitHub CLI (gh) on PATH. The default auto transport downloads over HTTPS without gh"
						.to_string(),
				)
			} else {
				TransportError::Gh(format!("could not run gh: {error}"))
			}
		})?;
	if !output.status.success() {
		return Err(TransportError::Gh(format!(
			"gh release download failed for {repo}@{tag}: {}",
			trim_note(&output.stderr)
		)));
	}
	let path = dir.path().join(asset);
	let bytes = read_file_capped(&path)?;
	if let Some(expected) = expected {
		verify_sha256(&bytes, expected)?;
	}
	Ok(FetchedAsset {
		bytes,
		via: DownloadRoute::Gh,
	})
}

fn read_file_capped(path: &Path) -> Result<Vec<u8>, TransportError> {
	let file = std::fs::File::open(path).map_err(|error| {
		TransportError::Gh(format!(
			"gh did not write {}: {error}",
			path.file_name()
				.and_then(|name| name.to_str())
				.unwrap_or("asset")
		))
	})?;
	if let Ok(metadata) = file.metadata()
		&& metadata.len() > MAX_DOWNLOAD_BYTES
	{
		return Err(TransportError::DownloadTooLarge {
			limit: MAX_DOWNLOAD_BYTES,
		});
	}
	match read_capped(file, MAX_DOWNLOAD_BYTES) {
		Ok(bytes) => Ok(bytes),
		Err(BodyError::TooLarge) => Err(TransportError::DownloadTooLarge {
			limit: MAX_DOWNLOAD_BYTES,
		}),
		Err(BodyError::Io(error)) => Err(TransportError::Gh(error.to_string())),
	}
}

fn download_once(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, TransportError> {
	let response = client
		.get(url)
		.send()
		.map_err(|error| TransportError::Network(error.to_string()))?;
	let status = response.status().as_u16();
	if !(200..300).contains(&status) {
		return Err(TransportError::Status(status));
	}
	if let Some(length) = response.content_length()
		&& length > MAX_DOWNLOAD_BYTES
	{
		return Err(TransportError::DownloadTooLarge {
			limit: MAX_DOWNLOAD_BYTES,
		});
	}
	match read_capped(response, MAX_DOWNLOAD_BYTES) {
		Ok(bytes) => Ok(bytes),
		Err(BodyError::TooLarge) => Err(TransportError::DownloadTooLarge {
			limit: MAX_DOWNLOAD_BYTES,
		}),
		Err(BodyError::Io(error)) => Err(TransportError::Network(error.to_string())),
	}
}

fn read_capped(mut reader: impl Read, limit: u64) -> Result<Vec<u8>, BodyError> {
	let mut buf = Vec::new();
	let mut chunk = [0u8; 8192];
	loop {
		let read = reader.read(&mut chunk).map_err(BodyError::Io)?;
		if read == 0 {
			break;
		}
		let next = (buf.len() as u64).saturating_add(read as u64);
		if next > limit {
			return Err(BodyError::TooLarge);
		}
		buf.extend_from_slice(&chunk[..read]);
	}
	Ok(buf)
}

fn http_client() -> Result<reqwest::blocking::Client, TransportError> {
	// Plugin bytes are verified by rustls and, when pinned, by SHA-256.
	// An ambient HTTP proxy must not be able to redirect that download.
	reqwest::blocking::Client::builder()
		.user_agent(format!("foch/{}", env!("CARGO_PKG_VERSION")))
		.connect_timeout(CONNECT_TIMEOUT)
		.timeout(REQUEST_TIMEOUT)
		.redirect(redirect_policy())
		.no_proxy()
		.build()
		.map_err(|error| {
			TransportError::Network(format!("could not build the HTTP client: {error}"))
		})
}

fn redirect_policy() -> reqwest::redirect::Policy {
	reqwest::redirect::Policy::custom(|attempt| {
		if attempt.previous().len() >= 5 {
			return attempt.error("too many redirects");
		}
		if url_allowed(attempt.url().as_str()) {
			attempt.follow()
		} else {
			attempt.stop()
		}
	})
}

fn url_allowed(url: &str) -> bool {
	let Ok(url) = reqwest::Url::parse(url) else {
		return false;
	};
	if !url.username().is_empty() || url.password().is_some() {
		return false;
	}
	match url.scheme() {
		"https" => true,
		"http" => url.host_str().is_some_and(is_loopback_host),
		_ => false,
	}
}

fn api_release_url(
	api_base: &str,
	repo: &str,
	tag: Option<&str>,
) -> Result<String, TransportError> {
	let base = normalize_origin(api_base)?;
	let repo = encode_repo(repo)?;
	match tag {
		None => Ok(format!("{base}/repos/{repo}/releases/latest")),
		Some(tag) => Ok(format!(
			"{base}/repos/{repo}/releases/tags/{}",
			encode_segment(tag, "tag")?
		)),
	}
}

fn resolve_bearer(bearer: &Bearer) -> Option<String> {
	match bearer {
		Bearer::Absent => None,
		Bearer::Token(token) => usable_token(token).map(str::to_string),
		Bearer::Environment => environment_token(),
	}
}

fn environment_token() -> Option<String> {
	for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
		if let Ok(value) = std::env::var(key)
			&& let Some(token) = usable_token(&value)
		{
			return Some(token.to_string());
		}
	}
	None
}

fn usable_token(token: &str) -> Option<&str> {
	let token = token.trim();
	if token.is_empty()
		|| !token.is_ascii()
		|| token
			.bytes()
			.any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
	{
		None
	} else {
		Some(token)
	}
}

fn sanitize(message: &str, token: Option<&str>) -> String {
	match token {
		Some(token) if !token.is_empty() && message.contains(token) => {
			message.replace(token, "***")
		}
		_ => message.to_string(),
	}
}

fn api_detail(body: &[u8]) -> String {
	let message = serde_json::from_slice::<serde_json::Value>(body)
		.ok()
		.and_then(|value| {
			value
				.get("message")
				.and_then(|item| item.as_str())
				.map(str::to_string)
		})
		.unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string());
	let message = message.trim();
	if message.is_empty() {
		String::new()
	} else {
		format!(": {}", truncate(message, 180))
	}
}

fn truncate(text: &str, max_chars: usize) -> String {
	let mut out: String = text.chars().take(max_chars).collect();
	if text.chars().count() > max_chars {
		out.push('…');
	}
	out
}

fn trim_note(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let text = text.trim();
	if text.is_empty() {
		"gh exited without a message".to_string()
	} else {
		truncate(text, 400)
	}
}

fn gh_available() -> bool {
	Command::new("gh")
		.arg("--version")
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.status()
		.is_ok_and(|status| status.success())
}

fn host_locale_hints() -> Vec<String> {
	let mut hints = Vec::new();
	for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
		if let Ok(value) = std::env::var(key)
			&& meaningful_locale(&value)
		{
			hints.push(value);
		}
	}
	if let Some(locale) = windows_user_locale() {
		hints.push(locale);
	}
	hints
}

fn meaningful_locale(value: &str) -> bool {
	let value = value.trim();
	!value.is_empty()
		&& !value.eq_ignore_ascii_case("c")
		&& !value.eq_ignore_ascii_case("posix")
		&& !value.eq_ignore_ascii_case("c.utf-8")
		&& !value.eq_ignore_ascii_case("c.utf8")
}

fn host_tz_offset_secs() -> Option<i32> {
	#[cfg(windows)]
	{
		windows_tz_offset_secs()
	}
	#[cfg(all(unix, target_pointer_width = "64"))]
	{
		unix_tz_offset_secs()
	}
	#[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
	{
		None
	}
}

#[cfg(windows)]
fn windows_user_locale() -> Option<String> {
	#[link(name = "kernel32")]
	unsafe extern "system" {
		fn GetUserDefaultLocaleName(locale_name: *mut u16, chars: i32) -> i32;
	}
	let mut buf = [0u16; 85];
	let written = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
	if written <= 1 {
		return None;
	}
	let len = (written as usize).saturating_sub(1);
	if len > buf.len() {
		return None;
	}
	String::from_utf16(&buf[..len]).ok()
}

#[cfg(windows)]
fn windows_tz_offset_secs() -> Option<i32> {
	#[repr(C)]
	struct SystemTime {
		year: u16,
		month: u16,
		day_of_week: u16,
		day: u16,
		hour: u16,
		minute: u16,
		second: u16,
		milliseconds: u16,
	}
	#[repr(C)]
	struct TimeZoneInformation {
		bias: i32,
		standard_name: [u16; 32],
		standard_date: SystemTime,
		standard_bias: i32,
		daylight_name: [u16; 32],
		daylight_date: SystemTime,
		daylight_bias: i32,
	}
	#[link(name = "kernel32")]
	unsafe extern "system" {
		fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
	}
	const INVALID: u32 = 0xFFFF_FFFF;
	const STANDARD: u32 = 1;
	const DAYLIGHT: u32 = 2;
	let mut info = unsafe { std::mem::zeroed::<TimeZoneInformation>() };
	let kind = unsafe { GetTimeZoneInformation(&mut info) };
	if kind == INVALID {
		return None;
	}
	let extra = if kind == DAYLIGHT {
		info.daylight_bias
	} else if kind == STANDARD {
		info.standard_bias
	} else {
		0
	};
	info.bias.checked_add(extra)?.checked_mul(-60)
}

#[cfg(all(unix, target_pointer_width = "64"))]
fn unix_tz_offset_secs() -> Option<i32> {
	#[repr(C)]
	#[allow(dead_code)]
	struct Tm {
		tm_sec: i32,
		tm_min: i32,
		tm_hour: i32,
		tm_mday: i32,
		tm_mon: i32,
		tm_year: i32,
		tm_wday: i32,
		tm_yday: i32,
		tm_isdst: i32,
		tm_gmtoff: i64,
		tm_zone: *const std::ffi::c_char,
	}
	unsafe extern "C" {
		fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
	}
	let seconds = i64::try_from(
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.ok()?
			.as_secs(),
	)
	.ok()?;
	let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
	let ptr = unsafe { localtime_r(&seconds, tm.as_mut_ptr()) };
	if ptr.is_null() {
		return None;
	}
	let tm = unsafe { tm.assume_init() };
	i32::try_from(tm.tm_gmtoff).ok()
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::Cursor;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::{Arc, Mutex};
	use std::thread::JoinHandle;

	#[test]
	fn region_policy_is_conservative_and_direct_wins() {
		assert!(detect_china_region(&["zh_CN.UTF-8"], None));
		assert!(detect_china_region(&["zh-CN"], Some(0)));
		assert!(detect_china_region(&["zh_Hans_CN"], None));
		assert!(detect_china_region(&["zh"], Some(CHINA_UTC_OFFSET_SECS)));
		assert!(detect_china_region(
			&["zh-Hans"],
			Some(CHINA_UTC_OFFSET_SECS)
		));
		assert!(!detect_china_region(&["zh"], None));
		assert!(!detect_china_region(
			&["en_US.UTF-8"],
			Some(CHINA_UTC_OFFSET_SECS)
		));
		assert!(!detect_china_region(
			&["zh_TW.UTF-8"],
			Some(CHINA_UTC_OFFSET_SECS)
		));
		assert!(!detect_china_region(
			&["zh_HK"],
			Some(CHINA_UTC_OFFSET_SECS)
		));
		assert!(!detect_china_region(
			&[] as &[&str],
			Some(CHINA_UTC_OFFSET_SECS)
		));

		let china = resolve_mirror_chain(
			&MirrorPolicy::Auto,
			&["zh_CN".to_string()],
			Some(CHINA_UTC_OFFSET_SECS),
		)
		.unwrap();
		assert_eq!(
			china.prefixes,
			CHINA_MIRROR_PREFIXES
				.iter()
				.map(|prefix| (*prefix).to_string())
				.collect::<Vec<_>>()
		);
		assert!(china.direct_fallback);
		let direct = resolve_mirror_chain(&MirrorPolicy::Auto, &["en_GB.UTF-8"], Some(0)).unwrap();
		assert!(direct.prefixes.is_empty());
		assert!(direct.direct_fallback);
		let overridden = resolve_mirror_chain(
			&MirrorPolicy::Direct,
			&["zh_CN"],
			Some(CHINA_UTC_OFFSET_SECS),
		)
		.unwrap();
		assert!(overridden.prefixes.is_empty());
		assert!(overridden.direct_fallback);
	}

	#[test]
	fn mirror_schemes_and_selection_reject_unsafe_urls() {
		assert_eq!(
			select_transport(Some("http"), Some("gh")).unwrap(),
			InstallTransport::Http
		);
		assert_eq!(
			select_transport(None, Some("gh")).unwrap(),
			InstallTransport::Gh
		);
		assert_eq!(
			select_transport(None, None).unwrap(),
			InstallTransport::Auto
		);
		assert!(select_transport(Some("ftp"), None).is_err());

		assert_eq!(
			select_mirror(&["direct".into()], Some("cn")).unwrap(),
			MirrorPolicy::Direct
		);
		assert_eq!(select_mirror(&[], Some("cn")).unwrap(), MirrorPolicy::China);
		assert_eq!(select_mirror(&[], None).unwrap(), MirrorPolicy::Auto);
		let custom = select_mirror(
			&[
				"https://gh-proxy.com".into(),
				"https://example.test/fetch?u={url}".into(),
			],
			None,
		)
		.unwrap();
		assert_eq!(
			custom,
			MirrorPolicy::Custom(vec![
				"https://gh-proxy.com/".into(),
				"https://example.test/fetch?u={url}".into(),
			])
		);
		for rejected in [
			"file:///tmp/plugin.zip",
			"javascript:alert(1)",
			"http://example.com/mirror/",
			"https://user:pass@mirror.example/",
			"https://mirror.example/a b/",
			"http://192.168.1.1/",
		] {
			let error = select_mirror(&[rejected.into()], None).unwrap_err();
			assert!(
				matches!(error, TransportError::InvalidMirror(_)),
				"{rejected}: {error}"
			);
		}
	}

	#[test]
	fn mirror_urls_append_or_substitute_the_github_download() {
		let direct = release_download_url(
			"YozoraTempest/EU4-Unicode-Patch",
			"v0.1.15",
			"EU4UnicodePatch-1.37.5-v0.1.15.zip",
		)
		.unwrap();
		assert_eq!(
			direct,
			"https://github.com/YozoraTempest/EU4-Unicode-Patch/releases/download/v0.1.15/EU4UnicodePatch-1.37.5-v0.1.15.zip"
		);
		assert_eq!(
			apply_mirror("https://gh-proxy.com/", &direct),
			format!("https://gh-proxy.com/{direct}")
		);
		let templated = apply_mirror("https://my-mirror.example/fetch?u={url}", &direct);
		assert!(
			templated.starts_with("https://my-mirror.example/fetch?u=https%3A%2F%2Fgithub.com%2F")
		);
		assert!(!templated.contains("{url}"));
		assert!(verify_sha256(b"plugin", &sha256_hex(b"plugin")).is_ok());
		let mismatch = verify_sha256(b"plugin", &sha256_hex(b"other")).unwrap_err();
		assert!(matches!(mismatch, TransportError::DigestMismatch { .. }));
		assert!(verify_sha256(b"plugin", "abcd").is_err());
	}

	#[test]
	fn http_direct_download_validates_a_synthetic_plugin_without_gh() {
		let zip = sample_plugin_zip();
		let server = serve({
			let zip = zip.clone();
			move |request| {
				assert!(
					request
						.url()
						.contains("/dev/sample/releases/download/v0.1.0/sample.zip")
				);
				respond(request, 200, zip.clone());
			}
		});
		let fetched = fetch_release_asset(
			"dev/sample",
			"v0.1.0",
			"sample.zip",
			&FetchOptions {
				transport: InstallTransport::Http,
				mirror: MirrorPolicy::Direct,
				asset_origin: Some(server.base.clone()),
				..FetchOptions::default()
			},
		)
		.unwrap();
		assert_eq!(fetched.via.to_string(), "via http (direct)");
		let package = crate::plugin::github::package_from_own_zip(&fetched.bytes).unwrap();
		assert_eq!(package.manifest.plugin.id, "dev.foch.sample");
		assert_eq!(package.manifest.plugin.version.to_string(), "0.1.0");
	}

	#[test]
	fn mirror_chain_fails_over_from_502_to_the_next_prefix() {
		let zip = sample_plugin_zip();
		let bad = serve(|request| respond(request, 502, Vec::new()));
		let good =
			serve({
				let zip = zip.clone();
				move |request| {
					assert!(request.url().contains(
						"https://github.com/dev/sample/releases/download/v0.1.0/sample.zip"
					));
					respond(request, 200, zip.clone());
				}
			});
		let fetched = fetch_release_asset(
			"dev/sample",
			"v0.1.0",
			"sample.zip",
			&FetchOptions {
				transport: InstallTransport::Http,
				mirror: MirrorPolicy::Custom(vec![
					format!("{}/", bad.base),
					format!("{}/", good.base),
				]),
				..FetchOptions::default()
			},
		)
		.unwrap();
		assert_eq!(
			fetched.via.to_string(),
			format!("via http (mirror {}/)", good.base)
		);
		assert_eq!(fetched.bytes, zip);
	}

	#[test]
	fn tampered_mirror_is_rejected_before_the_next_mirror() {
		let zip = sample_plugin_zip();
		let mut tampered = zip.clone();
		tampered.push(0xff);
		let hits = Arc::new(AtomicUsize::new(0));
		let bad = serve({
			let tampered = tampered.clone();
			move |request| respond(request, 200, tampered.clone())
		});
		let seen = Arc::clone(&hits);
		let good = serve(move |request| {
			seen.fetch_add(1, Ordering::SeqCst);
			respond(request, 200, b"should-not-be-read".to_vec());
		});
		let error = fetch_release_asset(
			"dev/sample",
			"v0.1.0",
			"sample.zip",
			&FetchOptions {
				transport: InstallTransport::Http,
				mirror: MirrorPolicy::Custom(vec![
					format!("{}/", bad.base),
					format!("{}/", good.base),
				]),
				sha256: Some(sha256_hex(&zip)),
				..FetchOptions::default()
			},
		)
		.unwrap_err();
		assert!(
			matches!(error, TransportError::DigestMismatch { .. }),
			"{error}"
		);
		assert!(error.to_string().contains("was not installed"), "{error}");
		assert_eq!(hits.load(Ordering::SeqCst), 0);
	}

	#[test]
	fn oversized_response_stream_fails_closed() {
		let produced = Arc::new(AtomicUsize::new(0));
		let count = Arc::clone(&produced);
		let server = serve(move |request| {
			let reader = CountingReader {
				left: MAX_DOWNLOAD_BYTES + 2 * 1024 * 1024,
				produced: Arc::clone(&count),
			};
			let response = tiny_http::Response::new(
				tiny_http::StatusCode(200),
				Vec::new(),
				reader,
				None,
				None,
			);
			let _ = request.respond(response);
		});
		let error = fetch_release_asset(
			"dev/sample",
			"v0.1.0",
			"sample.zip",
			&FetchOptions {
				transport: InstallTransport::Http,
				mirror: MirrorPolicy::Direct,
				asset_origin: Some(server.base.clone()),
				..FetchOptions::default()
			},
		)
		.unwrap_err();
		drop(server);
		let message = error.to_string();
		assert!(
			message.contains("exceeded the 33554432 byte limit") && message.contains("discarded"),
			"{message}"
		);
		let sent = produced.load(Ordering::SeqCst) as u64;
		assert!(
			sent < MAX_DOWNLOAD_BYTES + 1024 * 1024,
			"stream was not capped, sent {sent}"
		);
		let limited = read_capped(InfiniteReader { reads: 0 }, 1000).unwrap_err();
		assert!(matches!(limited, BodyError::TooLarge));
	}

	#[test]
	fn dynamic_release_json_picks_the_preferred_zip_and_reports_rate_limits() {
		let seen = Arc::new(Mutex::new(Vec::new()));
		let record = Arc::clone(&seen);
		let server = serve(move |request| {
			let headers: Vec<(String, String)> = request
				.headers()
				.iter()
				.map(|header| {
					(
						header.field.as_str().as_str().to_string(),
						header.value.as_str().to_string(),
					)
				})
				.collect();
			record
				.lock()
				.expect("header lock")
				.push((request.url().to_string(), headers));
			let body = if request.url().ends_with("/releases/latest") {
				release_json(
					"v1.2.3",
					&[
						("README.md", false),
						("other.zip", true),
						("widgets-v1.2.3.zip", true),
					],
				)
			} else if request.url().ends_with("/releases/tags/v9.0.0") {
				release_json("v9.0.0", &[("notes.txt", false), ("extra.zip", true)])
			} else {
				respond(request, 404, br#"{"message":"not found"}"#.to_vec());
				return;
			};
			respond(request, 200, body.into_bytes());
		});
		let config = ApiQuery {
			api_base: server.base.clone(),
			bearer: Bearer::Token("test-token".to_string()),
		};
		let latest = resolve_dynamic_http("acme/widgets", Some("latest"), &config).unwrap();
		assert_eq!(latest.tag, "v1.2.3");
		assert_eq!(latest.asset, "widgets-v1.2.3.zip");
		let pinned = resolve_dynamic_http("acme/widgets", Some("v9.0.0"), &config).unwrap();
		assert_eq!(pinned.tag, "v9.0.0");
		assert_eq!(pinned.asset, "extra.zip");
		let recorded = seen.lock().expect("header lock");
		assert!(
			recorded[0]
				.0
				.ends_with("/repos/acme/widgets/releases/latest")
		);
		assert!(
			recorded[1]
				.0
				.ends_with("/repos/acme/widgets/releases/tags/v9.0.0")
		);
		for (_, headers) in recorded.iter() {
			assert!(headers.iter().any(|(name, value)| {
				name.eq_ignore_ascii_case("user-agent")
					&& value == &format!("foch/{}", env!("CARGO_PKG_VERSION"))
			}));
			assert!(headers.iter().any(|(name, value)| {
				name.eq_ignore_ascii_case("accept") && value == "application/vnd.github+json"
			}));
			assert!(headers.iter().any(|(name, value)| {
				name.eq_ignore_ascii_case("authorization") && value == "Bearer test-token"
			}));
		}
		drop(recorded);
		drop(server);

		let limited = serve(|request| {
			respond(
				request,
				403,
				br#"{"message":"API rate limit exceeded"}"#.to_vec(),
			);
		});
		let error = resolve_dynamic_http(
			"acme/widgets",
			None,
			&ApiQuery {
				api_base: limited.base.clone(),
				bearer: Bearer::Absent,
			},
		)
		.unwrap_err();
		let message = error.to_string();
		assert!(message.contains("403"), "{message}");
		assert!(message.contains("GITHUB_TOKEN"), "{message}");
		assert!(message.contains("--transport gh"), "{message}");
		assert!(!message.contains("must install"), "{message}");
		assert!(
			resolve_dynamic_http("acme/widgets", Some("nightly"), &ApiQuery::default()).is_err()
		);
	}

	#[test]
	fn host_region_probe_does_not_panic() {
		let _ = host_locale_hints();
		let _ = host_tz_offset_secs();
	}

	struct TestServer {
		base: String,
		server: Arc<tiny_http::Server>,
		thread: Option<JoinHandle<()>>,
	}

	impl Drop for TestServer {
		fn drop(&mut self) {
			self.server.unblock();
			if let Some(thread) = self.thread.take() {
				let _ = thread.join();
			}
		}
	}

	fn serve(handler: impl Fn(tiny_http::Request) + Send + 'static) -> TestServer {
		let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
		let addr = server.server_addr().to_ip().expect("tcp listen address");
		let base = format!("http://{addr}");
		let server = Arc::new(server);
		let worker = Arc::clone(&server);
		let thread = std::thread::spawn(move || {
			for request in worker.incoming_requests() {
				handler(request);
			}
		});
		TestServer {
			base,
			server,
			thread: Some(thread),
		}
	}

	fn respond(request: tiny_http::Request, status: u16, body: Vec<u8>) {
		let len = body.len();
		let response = tiny_http::Response::new(
			tiny_http::StatusCode(status),
			Vec::new(),
			Cursor::new(body),
			Some(len),
			None,
		);
		let _ = request.respond(response);
	}

	struct CountingReader {
		left: u64,
		produced: Arc<AtomicUsize>,
	}

	impl Read for CountingReader {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			if self.left == 0 {
				return Ok(0);
			}
			let n = (buf.len() as u64).min(self.left).min(8192) as usize;
			buf[..n].fill(0);
			self.left -= n as u64;
			self.produced.fetch_add(n, Ordering::SeqCst);
			Ok(n)
		}
	}

	struct InfiniteReader {
		reads: u32,
	}

	impl Read for InfiniteReader {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			self.reads += 1;
			assert!(self.reads < 8, "read_capped did not stop");
			buf.fill(1);
			Ok(buf.len())
		}
	}

	fn release_json(tag: &str, assets: &[(&str, bool)]) -> String {
		let assets: Vec<serde_json::Value> = assets
			.iter()
			.map(|(name, zip)| {
				serde_json::json!({
					"name": name,
					"browser_download_url": if *zip {
						format!("https://github.com/acme/widgets/releases/download/{tag}/{name}")
					} else {
						String::new()
					},
				})
			})
			.collect();
		serde_json::json!({ "tag_name": tag, "assets": assets }).to_string()
	}

	fn sample_plugin_zip() -> Vec<u8> {
		let dll = fake_dll();
		let manifest = format!(
			r#"
schema = 1
[plugin]
id = "dev.foch.sample"
name = "Sample"
version = "0.1.0"
[target]
platform = "windows-x86_64"
game = "eu4"
game_versions = "*"
abi_major = 1
[entry]
kind = "native"
path = "sample.dll"
phase = "deferred"
[[files]]
path = "sample.dll"
sha256 = "{}"
"#,
			sha256_hex(&dll)
		);
		stored_zip(&[
			("foch-plugin.toml", manifest.into_bytes()),
			("sample.dll", dll),
		])
	}

	fn fake_dll() -> Vec<u8> {
		let mut bytes = vec![0u8; 0x80];
		bytes[0] = b'M';
		bytes[1] = b'Z';
		bytes[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
		bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
		bytes[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
		bytes
	}

	fn stored_zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
		let mut local = Vec::new();
		let mut members = Vec::new();
		for (name, data) in files {
			let offset = local.len() as u32;
			local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
			local.extend_from_slice(&20u16.to_le_bytes());
			local.extend_from_slice(&0u16.to_le_bytes());
			local.extend_from_slice(&0u16.to_le_bytes());
			local.extend_from_slice(&[0; 8]);
			local.extend_from_slice(&(data.len() as u32).to_le_bytes());
			local.extend_from_slice(&(data.len() as u32).to_le_bytes());
			local.extend_from_slice(&(name.len() as u16).to_le_bytes());
			local.extend_from_slice(&0u16.to_le_bytes());
			local.extend_from_slice(name.as_bytes());
			local.extend_from_slice(data);
			members.push((name, data, offset));
		}
		let cd_offset = local.len() as u32;
		let mut cd = Vec::new();
		for (name, data, offset) in &members {
			cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
			cd.extend_from_slice(&20u16.to_le_bytes());
			cd.extend_from_slice(&20u16.to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&[0; 4]);
			cd.extend_from_slice(&crc32(data).to_le_bytes());
			cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
			cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
			cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&0u16.to_le_bytes());
			cd.extend_from_slice(&[0; 8]);
			cd.extend_from_slice(&offset.to_le_bytes());
			cd.extend_from_slice(name.as_bytes());
		}
		let mut out = local;
		out.extend_from_slice(&cd);
		out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out.extend_from_slice(&(members.len() as u16).to_le_bytes());
		out.extend_from_slice(&(members.len() as u16).to_le_bytes());
		out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
		out.extend_from_slice(&cd_offset.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes());
		out
	}

	fn crc32(data: &[u8]) -> u32 {
		let mut crc = 0xffff_ffffu32;
		for &byte in data {
			crc ^= u32::from(byte);
			for _ in 0..8 {
				let mask = (crc & 1).wrapping_neg();
				crc = (crc >> 1) ^ (0xedb8_8320 & mask);
			}
		}
		!crc
	}
}
