/// The version of everything foch derives and keeps between runs: the parse,
/// rule-pack, mod-snapshot, merge-base and mod-diff caches, the installed base
/// data, and the analysis rules both are checked against. Data written under
/// another version is never read as current.
///
/// Raise the last number by one when a change makes any of it unreadable or
/// stale, at most once per pull request. Foch is unreleased, so this stays a
/// 0.0.N pre-release version.
pub const DATA_VERSION: &str = "0.0.21";
