//! Native DLL plugin management: manifests, the version store, and load
//! planning, shared by the CLI and the desktop application.
//!
//! This module manages plugin identity, versions, and the order Foch asks its
//! in-game host to load them in. It does not load DLLs or inspect a running
//! game; that is the host's job, driven by the frozen plan this module
//! produces (see [`planner`]).

pub mod builtin;
pub mod companion;
pub mod deployment;
pub mod github;
pub mod manifest;
pub mod paths;
pub mod planner;
pub mod preferences;
pub mod registry;
pub mod selection;
pub mod service;
pub mod store;
pub mod transport;

pub use companion::{DeclaredModPlugin, discover_declared_plugins};
pub use manifest::{Manifest, ManifestError};
pub use planner::{
	Diagnostic, GameIdentity, Resolution, Resolved, Selection, WINDOWS_X64, parse_game_version,
	plan,
};
pub use preferences::{
	LanguagePreference, MirrorPreference, TransportPreference, UiLanguage, UserPreferences,
	detect_system_language, preferences_path, resolve_language,
};
pub use selection::Selections;
pub use service::{
	ConfigFieldSummary, PluginBadge, PluginEntrySummary, PluginOverview, PluginPlanSummary,
	ServiceError, build_overview, set_plugin_config_value, set_plugin_enabled,
};
pub use store::{ImportError, Installed, ValidatedPackage, catalog, install, validate};
pub use transport::{
	ApiQuery, ConnectivityProbeOptions, ConnectivityReport, DownloadRoute, FetchOptions,
	InstallTransport, MAX_DOWNLOAD_BYTES, MirrorPolicy, ProbeRouteKind, ResolvedRelease,
	RouteProbeOutcome, TransportConfig, TransportError, detect_china_region, fetch_release_asset,
	probe_connectivity, resolve_dynamic_http, select_mirror, select_transport, system_locale_hints,
	verify_sha256,
};
