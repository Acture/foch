//! Crate-private EU4 layout selection.
//!
//! Offsets are not hardcoded in this module. [`super::profile`] parses the
//! compiled-in JSON profiles once and views read the profile bound to a
//! game. [`profile_for_build`] matches `exe_sha256` exactly; the version
//! string is never a lookup key, and nothing here reads a profile from disk.

pub(crate) use super::profile::{default_profile, profile_field_status, profile_for_build};
