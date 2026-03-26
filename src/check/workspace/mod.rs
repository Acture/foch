pub(crate) mod resolve;

pub(crate) use resolve::{
	ResolvedFileContributor, ResolvedWorkspace, WorkspaceResolveErrorKind, normalize_relative_path,
	resolve_workspace,
};
