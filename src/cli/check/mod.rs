use crate::{cli::arg::CheckArgs, config::Config};

pub fn handle_check(check_args: &CheckArgs, config: Config) {
	tracing::info!("检查 Playset: {:?}", check_args.playset_path);
}
