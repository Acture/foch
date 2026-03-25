pub(crate) mod resolve;

pub(crate) use resolve::{
	normalize_relative_path, resolve_workspace, ResolvedFileContributor, ResolvedWorkspace,
	WorkspaceResolveErrorKind,
};
