use clap::Parser;
use foch::cli::arg;
use foch::cli::config::load_or_init_config;
use foch::cli::handler;
use tracing_subscriber::FmtSubscriber;

fn main() {
	let exit_code = match run() {
		Ok(code) => code,
		Err(err) => {
			eprintln!("错误: {err}");
			1
		}
	};

	std::process::exit(exit_code);
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
	let cliargs = arg::FochCli::parse();

	let subscriber = FmtSubscriber::builder()
		.with_max_level(cliargs.verbose.tracing_level_filter())
		.with_target(false)
		.without_time()
		.finish();

	tracing::subscriber::set_global_default(subscriber)?;

	let (mut config, config_file) = load_or_init_config()?;

	match &cliargs.command {
		arg::FochCliCommands::Check(check_args) => {
			handler::check::handle_check(check_args, config)
		}
		arg::FochCliCommands::Config(config_args) => {
			handler::config::handle_config(config_args, &mut config, &config_file)
		}
	}
}
