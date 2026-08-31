use clap::Parser;
use foch::check::CHECK_PROGRESS_TARGET;
use foch::graph::SEMANTIC_GRAPH_PROGRESS_TARGET;
use foch::input::{load_config_read_only, load_or_init_config};
use foch_cli::cli::arg;
use foch_cli::cli::handler;
use std::io;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{fmt, registry};

fn main() {
	// Use a larger stack to handle deeply nested Clausewitz ASTs
	// (some EU4 files have 20+ nesting levels).
	let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
	let handler = builder
		.spawn(|| {
			let exit_code = match run() {
				Ok(code) => code,
				Err(err) => {
					eprintln!("error: {err}");
					1
				}
			};
			std::process::exit(exit_code);
		})
		.expect("failed to spawn main thread with larger stack");
	handler.join().expect("main thread panicked");
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
	let cliargs = arg::FochCli::parse();
	let verbose_level = cliargs.verbose.tracing_level_filter();
	let show_semantic_graph_progress = matches!(&cliargs.command, arg::FochCliCommands::Graph(graph_args) if graph_args.mode == arg::GraphModeArg::Semantic);
	let show_check_progress = matches!(
		&cliargs.command,
		arg::FochCliCommands::Check(_) | arg::FochCliCommands::Merge(_)
	);
	let semantic_graph_progress_level = if show_semantic_graph_progress {
		LevelFilter::INFO
	} else {
		LevelFilter::OFF
	};
	let check_progress_level = if show_check_progress {
		LevelFilter::INFO
	} else {
		LevelFilter::OFF
	};
	let subscriber = registry()
		.with(
			fmt::layer()
				.with_writer(io::stderr)
				.with_target(false)
				.without_time()
				.with_filter(
					Targets::new()
						.with_default(verbose_level)
						.with_target(SEMANTIC_GRAPH_PROGRESS_TARGET, LevelFilter::OFF)
						.with_target(CHECK_PROGRESS_TARGET, LevelFilter::OFF),
				),
		)
		.with(
			fmt::layer()
				.with_writer(io::stderr)
				.with_target(false)
				.without_time()
				.with_level(false)
				.with_filter(Targets::new().with_default(LevelFilter::OFF).with_target(
					SEMANTIC_GRAPH_PROGRESS_TARGET,
					semantic_graph_progress_level,
				)),
		)
		.with(
			fmt::layer()
				.with_writer(io::stderr)
				.with_target(false)
				.without_time()
				.with_level(false)
				.with_filter(
					Targets::new()
						.with_default(LevelFilter::OFF)
						.with_target(CHECK_PROGRESS_TARGET, check_progress_level),
				),
		);

	tracing::subscriber::set_global_default(subscriber)?;

	match &cliargs.command {
		arg::FochCliCommands::Cache(cache_args) => return handler::cache::handle_cache(cache_args),
		arg::FochCliCommands::Input(input_args) => {
			return handler::input::handle_input(input_args, load_config_read_only()?);
		}
		arg::FochCliCommands::Lsp(_lsp_args) => return Ok(foch_cli::lsp::run()),
		_ => {}
	}

	let (mut config, config_file) = load_or_init_config()?;
	match &cliargs.command {
		arg::FochCliCommands::Check(check_args) => handler::check::handle_check(check_args, config),
		arg::FochCliCommands::Merge(merge_args) => handler::merge::handle_merge(merge_args, config),
		arg::FochCliCommands::Graph(graph_args) => handler::graph::handle_graph(graph_args, config),
		arg::FochCliCommands::Simplify(simplify_args) => {
			handler::simplify::handle_simplify(simplify_args, config)
		}
		arg::FochCliCommands::Data(data_args) => handler::data::handle_data(data_args, config),
		arg::FochCliCommands::Cache(_)
		| arg::FochCliCommands::Input(_)
		| arg::FochCliCommands::Lsp(_) => unreachable!("handled before config initialization"),
		arg::FochCliCommands::Config(config_args) => {
			handler::config::handle_config(config_args, &mut config, &config_file)
		}
	}
}
