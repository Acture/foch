use clap::{Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct ModManagerCli {
	#[command(subcommand)]
	pub command: ModManagerCliCommands,

	#[arg(short, long, global = true)]
	pub game_path: Option<String>,

	#[command(flatten)]
	pub verbose: Verbosity<InfoLevel>,
}

#[derive(Subcommand, Debug)]
pub enum ModManagerCliCommands {
	Check,
}
