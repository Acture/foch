use crate::cli::arg::CheckArgs;
use crate::cli::config::Config;

pub fn handle_check(check_args: &CheckArgs, config: Config) {
	println!("检查 Playset: {:?}", check_args.playset_path);
}
