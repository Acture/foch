//! Steam credentials: env first, then the system keyring CLI.
//!
//! STUB — implemented by the MQ-net track. Mirrors the Python harness:
//! `STEAM_API_KEY` / `keyring get steam api_key`, `STEAM_USERNAME` /
//! `keyring get steam username`.

/// Resolve the Steam Web API key (env `STEAM_API_KEY`, then `keyring get steam api_key`).
pub fn steam_api_key() -> Option<String> {
	todo!("MQ-net: implement steam_api_key")
}

/// Resolve the Steam login name (env `STEAM_USERNAME`, then `keyring get steam username`).
pub fn steam_username() -> Option<String> {
	todo!("MQ-net: implement steam_username")
}
