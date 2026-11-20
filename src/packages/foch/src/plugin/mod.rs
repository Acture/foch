//! Native DLL plugin management: manifests, the version store, and load
//! planning, shared by the CLI and the desktop application.
//!
//! This module manages plugin identity, versions, and the order Foch asks its
//! in-game host to load them in. It does not load DLLs or inspect a running
//! game; that is the host's job, driven by the frozen plan this module
//! produces (see [`planner`]).

pub mod builtin;
pub mod deployment;
pub mod github;
pub mod manifest;
pub mod paths;
pub mod planner;
pub mod registry;
pub mod selection;
pub mod store;
pub mod transport;

pub use manifest::{Manifest, ManifestError};
pub use planner::{
	Diagnostic, GameIdentity, Resolution, Resolved, Selection, WINDOWS_X64, parse_game_version,
	plan,
};
pub use selection::Selections;
pub use store::{ImportError, Installed, ValidatedPackage, catalog, install, validate};
pub use transport::{
	ApiQuery, DownloadRoute, FetchOptions, InstallTransport, MAX_DOWNLOAD_BYTES, MirrorPolicy,
	ResolvedRelease, TransportError, detect_china_region, fetch_release_asset,
	resolve_dynamic_http, select_mirror, select_transport, verify_sha256,
};
