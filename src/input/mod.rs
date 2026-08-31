//! Product input discovery, resolution, and cached snapshots.

pub mod config;
mod inspect;
pub mod request;

mod file_filter;
pub(crate) mod mod_snapshot;
mod resolve;
mod scripts;

pub use config::{
	CONFIG_DIR_ENV, Config, ValidationItem, ValidationStatus, get_config_dir_path,
	load_config_read_only, load_or_init_config,
};
pub use file_filter::FileFilter;
pub use inspect::{
	BaseDataInspection, BaseDataState, CurrentEu4Input, DetectedPlayset, DetectedPlaysetMod,
	InputReadiness, InputReadinessIssue, InstalledGameInspection, inspect_current_eu4_input,
};
pub(crate) use mod_snapshot::LoadedModSnapshot;
pub use mod_snapshot::{CacheError, default_mod_snapshot_cache_dir};
pub use request::{CheckOptions, InputRequest, InputSource};
pub(crate) use resolve::{
	InputInventory, ResolvedInput, ResolvedInputContributor, build_input_inventory_for_paths,
	normalize_relative_path, resolve_input, resolve_input_from_inventory,
};
pub use resolve::{
	InputResolveError, InputResolveErrorKind, InputResolveSummary, InputTarget, InputTargetRole,
	ResolvedInputMod, resolve_input_summary, resolve_input_targets, resolve_product_input_manifest,
};
pub(crate) use scripts::InputScriptCache;
