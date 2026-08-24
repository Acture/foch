use crate::model::{AnalysisMode, ChannelMode};
use std::path::{Path, PathBuf};

use crate::game::eu4::base::snapshot::InstalledBaseSnapshotIdentity;
use crate::input::config::Config;

#[derive(Clone, Debug)]
pub struct InputRequest {
	pub source: InputSource,
	pub config: Config,
	/// Content identity supplied by a parent measurement process. Input
	/// resolution verifies it before retaining its own exact load token.
	pub expected_base_snapshot_identity: Option<String>,
	/// Exact parent-owned snapshot lease. Publishing workflows use this for
	/// internal revalidation without recapturing or current-checking the file.
	pub(crate) base_snapshot_lease: Option<InstalledBaseSnapshotIdentity>,
}

impl InputRequest {
	pub fn new(source: InputSource, config: Config) -> Self {
		Self {
			source,
			config,
			expected_base_snapshot_identity: None,
			base_snapshot_lease: None,
		}
	}

	pub fn from_playset_path(playset_path: PathBuf, config: Config) -> Self {
		Self::new(InputSource::DlcLoad(playset_path), config)
	}

	pub fn from_manifest_path(manifest_path: PathBuf, config: Config) -> Self {
		Self::new(InputSource::Manifest(manifest_path), config)
	}

	pub fn with_expected_base_snapshot_identity(mut self, identity: impl Into<String>) -> Self {
		self.expected_base_snapshot_identity = Some(identity.into());
		self
	}

	pub(crate) fn with_base_snapshot_lease(
		mut self,
		lease: Option<InstalledBaseSnapshotIdentity>,
	) -> Self {
		self.base_snapshot_lease = lease;
		self
	}

	pub fn source_path(&self) -> &Path {
		self.source.path()
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputSource {
	DlcLoad(PathBuf),
	Manifest(PathBuf),
}

impl InputSource {
	pub fn from_path(path: PathBuf) -> Self {
		if path.file_name().and_then(|name| name.to_str()) == Some("foch.toml") {
			Self::Manifest(path)
		} else {
			Self::DlcLoad(path)
		}
	}

	pub fn path(&self) -> &Path {
		match self {
			Self::DlcLoad(path) | Self::Manifest(path) => path,
		}
	}
}

#[derive(Clone, Debug)]
pub struct CheckOptions {
	pub analysis_mode: AnalysisMode,
	pub channel_mode: ChannelMode,
	pub include_game_base: bool,
}

impl Default for CheckOptions {
	fn default() -> Self {
		Self {
			analysis_mode: AnalysisMode::default(),
			channel_mode: ChannelMode::default(),
			include_game_base: true,
		}
	}
}
