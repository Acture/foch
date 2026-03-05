use crate::cli::config::{Config, ValidationStatus};
use crate::cli::handler::HandlerResult;

pub fn handle_validate(config: &Config) -> HandlerResult {
	let items = config.validate();
	for item in items {
		let status = match item.status {
			ValidationStatus::Ok => "OK",
			ValidationStatus::Warning => "WARN",
			ValidationStatus::Error => "ERROR",
		};
		println!("[{status}] {} - {}", item.key, item.message);
	}

	Ok(0)
}
