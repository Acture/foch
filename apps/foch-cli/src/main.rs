use clap::Parser;
use foch_cli::cli::arg;
use foch_cli::cli::handler;
use foch_engine::load_or_init_config;
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
		arg::FochCliCommands::Check(check_args) => handler::check::handle_check(check_args, config),
		arg::FochCliCommands::MergePlan(merge_plan_args) => {
			handler::merge_plan::handle_merge_plan(merge_plan_args, config)
		}
		arg::FochCliCommands::Merge(merge_args) => handler::merge::handle_merge(merge_args, config),
		arg::FochCliCommands::Graph(graph_args) => handler::graph::handle_graph(graph_args, config),
		arg::FochCliCommands::Simplify(simplify_args) => {
			handler::simplify::handle_simplify(simplify_args, config)
		}
		arg::FochCliCommands::Data(data_args) => handler::data::handle_data(data_args, config),
		arg::FochCliCommands::Config(config_args) => {
			handler::config::handle_config(config_args, &mut config, &config_file)
		}
	}
}
