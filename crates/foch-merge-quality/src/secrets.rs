//! Steam credentials: env first, then the system keyring CLI.
//!
//! Mirrors the Python harness: `STEAM_API_KEY` / `keyring get steam api_key`,
//! `STEAM_USERNAME` / `keyring get steam username`.
//!
//! `keyring` is the CLI for the system keyring (macOS Keychain, GNOME Wallet, …).
//! It is optional — if not installed or the key is absent, `None` is returned.

/// Resolve the Steam Web API key.
///
/// Priority: env `STEAM_API_KEY` → `keyring get steam api_key`.
pub fn steam_api_key() -> Option<String> {
	resolve("STEAM_API_KEY", "api_key")
}

/// Resolve the Steam login name for steamcmd.
///
/// Priority: env `STEAM_USERNAME` → `keyring get steam username`.
pub fn steam_username() -> Option<String> {
	resolve("STEAM_USERNAME", "username")
}

/// Shared resolution logic: env var first, then keyring CLI.
fn resolve(env_var: &str, keyring_username: &str) -> Option<String> {
	if let Ok(val) = std::env::var(env_var) {
		let val = val.trim().to_string();
		if !val.is_empty() {
			return Some(val);
		}
	}
	keyring_get(keyring_username)
}

/// Shell out to `keyring get steam <username>`, returning the value or `None`.
fn keyring_get(username: &str) -> Option<String> {
	let out = std::process::Command::new("keyring")
		.args(["get", "steam", username])
		.output()
		.ok()?;
	if !out.status.success() {
		return None;
	}
	let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
	if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;

	/// Serialise env mutations — edition 2024's `set_var` / `remove_var` are
	/// `unsafe` and process-global, so parallel test threads would race.
	static ENV_LOCK: Mutex<()> = Mutex::new(());

	#[test]
	fn steam_api_key_prefers_env() {
		let _guard = ENV_LOCK.lock().unwrap();
		// Safety: test-only; ENV_LOCK prevents concurrent access.
		unsafe { std::env::set_var("STEAM_API_KEY", "test_key_abc123") };
		let result = steam_api_key();
		unsafe { std::env::remove_var("STEAM_API_KEY") };
		assert_eq!(result, Some("test_key_abc123".to_string()));
	}

	#[test]
	fn steam_api_key_ignores_empty_env() {
		let _guard = ENV_LOCK.lock().unwrap();
		unsafe { std::env::set_var("STEAM_API_KEY", "   ") };
		// Can't assert a specific value here (depends on keyring) but must not
		// return the whitespace-only string.
		let result = steam_api_key();
		unsafe { std::env::remove_var("STEAM_API_KEY") };
		assert_ne!(result, Some("   ".to_string()));
	}

	#[test]
	fn steam_username_prefers_env() {
		let _guard = ENV_LOCK.lock().unwrap();
		unsafe { std::env::set_var("STEAM_USERNAME", "gabe_newell") };
		let result = steam_username();
		unsafe { std::env::remove_var("STEAM_USERNAME") };
		assert_eq!(result, Some("gabe_newell".to_string()));
	}
}
