//! Select an account from login metadata; SteamCMD manages the credentials.
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use keyvalues_parser::{Obj, Value};

use super::ProbeResult;

pub(super) fn resolve(explicit: Option<&str>, steam_root: Option<&Path>) -> ProbeResult<String> {
	if let Some(account) = explicit {
		if account.trim().is_empty() {
			return Err("FOCH_STEAM_ACCOUNT must not be empty".into());
		}
		return Ok(account.to_owned());
	}
	if let Some(root) = steam_root {
		let path = root.join("config/loginusers.vdf");
		match fs::read_to_string(&path) {
			Ok(text) => {
				if let Some(account) = from_login_users(&text)? {
					return Ok(account);
				}
			}
			Err(error) if error.kind() == ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
	}
	Err("No unambiguous remembered Steam account was found. Set FOCH_STEAM_ACCOUNT to select an account; SteamCMD needs a cached login for downloads.".into())
}

fn from_login_users(text: &str) -> ProbeResult<Option<String>> {
	let parsed = keyvalues_parser::parse(text)?;
	if parsed.key.as_ref() != "users" || !parsed.bases.is_empty() {
		return Err("invalid Steam loginusers.vdf root".into());
	}
	let users = parsed.value.get_obj().ok_or("invalid Steam login users")?;
	let mut candidates: Vec<(String, bool)> = Vec::new();
	for values in users.values() {
		let [Value::Obj(user)] = values.as_slice() else {
			return Err("invalid or duplicate Steam login user".into());
		};
		if field(user, "RememberPassword")? != Some("1")
			|| field(user, "AllowAutoLogin")? != Some("1")
		{
			continue;
		}
		if let Some(name) = field(user, "AccountName")?.filter(|name| !name.trim().is_empty()) {
			candidates.push((name.to_owned(), field(user, "MostRecent")? == Some("1")));
		}
	}
	let recent: Vec<&String> = candidates
		.iter()
		.filter_map(|(name, recent)| recent.then_some(name))
		.collect();
	match recent.as_slice() {
		[name] => Ok(Some((*name).clone())),
		[] if candidates.len() == 1 => Ok(Some(candidates.remove(0).0)),
		_ => Ok(None),
	}
}

fn field<'a>(object: &'a Obj<'_>, key: &str) -> ProbeResult<Option<&'a str>> {
	match object.get(key).map(Vec::as_slice) {
		None => Ok(None),
		Some([Value::Str(value)]) => Ok(Some(value.as_ref())),
		_ => Err(format!("invalid or duplicate Steam login field {key}").into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const USERS: &str = r#""users" {
		"100" { "AccountName" "old" "RememberPassword" "1" "AllowAutoLogin" "1" "MostRecent" "0" }
		"200" { "AccountName" "current" "RememberPassword" "1" "AllowAutoLogin" "1" "MostRecent" "1" }
	}"#;

	#[test]
	fn uses_recent_remembered_account_and_explicit_override() {
		let root = tempfile::tempdir().unwrap();
		fs::create_dir(root.path().join("config")).unwrap();
		let path = root.path().join("config/loginusers.vdf");
		fs::write(&path, USERS).unwrap();
		assert_eq!(resolve(None, Some(root.path())).unwrap(), "current");
		fs::write(
			&path,
			r#""users" { "200" { "AccountName" "sole" "RememberPassword" "1" "AllowAutoLogin" "1" } }"#,
		)
		.unwrap();
		assert_eq!(resolve(None, Some(root.path())).unwrap(), "sole");
		fs::write(&path, "malformed metadata").unwrap();
		assert_eq!(
			resolve(Some("chosen"), Some(root.path())).unwrap(),
			"chosen"
		);
		assert!(resolve(None, Some(root.path())).is_err());
		assert!(resolve(Some(" "), None).is_err());
	}

	#[test]
	fn does_not_guess_among_accounts_or_fall_back_to_anonymous() {
		assert!(
			from_login_users(&USERS.replace("\"MostRecent\" \"1\"", "\"MostRecent\" \"0\""))
				.unwrap()
				.is_none()
		);
		assert!(
			from_login_users(&USERS.replace("\"MostRecent\" \"0\"", "\"MostRecent\" \"1\""))
				.unwrap()
				.is_none()
		);
		let disabled = USERS.replace("\"AllowAutoLogin\" \"1\"", "\"AllowAutoLogin\" \"0\"");
		assert!(from_login_users(&disabled).unwrap().is_none());
		let forgotten = USERS.replace("\"RememberPassword\" \"1\"", "\"RememberPassword\" \"0\"");
		assert!(from_login_users(&forgotten).unwrap().is_none());
		assert!(resolve(None, None).is_err());
		let root = tempfile::tempdir().unwrap();
		assert!(resolve(None, Some(root.path())).is_err());
		assert_eq!(resolve(Some("anonymous"), None).unwrap(), "anonymous");
	}
}
