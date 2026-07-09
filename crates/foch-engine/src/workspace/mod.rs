pub(crate) mod cache;
pub(crate) mod file_filter;
pub(crate) mod resolve;
pub(crate) mod scripts;
pub(crate) mod session;

pub(crate) use cache::LoadedModSnapshot;
pub use file_filter::FileFilter;

pub(crate) use resolve::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceInventory,
	build_workspace_inventory_with_hash_cache, normalize_relative_path, resolve_workspace,
	resolve_workspace_from_inventory,
};
pub use resolve::{
	WorkspaceResolveError, WorkspaceResolveErrorKind, WorkspaceResolveSummary,
	WorkspaceResolvedMod, WorkspaceTarget, WorkspaceTargetRole, resolve_workspace_summary,
	resolve_workspace_targets,
};
pub(crate) use scripts::WorkspaceScriptCache;

pub use session::WorkspaceSession;
