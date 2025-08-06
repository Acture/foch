use std::path::PathBuf;
pub fn resolve_home_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
	#[cfg(target_os = "macos")]
	{
		std::env::var("HOME")
			.map_err(|e| format!("Could not get HOME environment variable: {}", e).into())
			.map(PathBuf::from)
	}
	#[cfg(target_os = "windows")]
	{
		std::env::var("USERPROFILE")
			.map_err(|e| format!("Could not get USERPROFILE environment variable: {}", e).into())
			.map(PathBuf::from)
	}
	#[cfg(target_os = "linux")]
	{
		std::env::var("HOME")
			.map_err(|e| format!("Could not get HOME environment variable: {}", e).into())
			.map(PathBuf::from)
	}
}

pub fn check_dir_exists(path: PathBuf) -> Result<PathBuf, Box<dyn std::error::Error>> {
	if path.exists() && path.is_dir() {
		Ok(path)
	} else {
		Err(format!(
			"Provided path does not exist or is not a directory: {:?}",
			path
		)
		.into())
	}
}
