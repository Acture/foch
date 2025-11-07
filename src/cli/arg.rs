use clap::{Parser, Subcommand};
use clap_verbosity_flag::{Verbosity, WarnLevel};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct ModManagerCli {
	#[command(subcommand)]
	pub command: ModManagerCliCommands,

	#[command(flatten)]
	pub verbose: Verbosity<WarnLevel>,
}

#[derive(Subcommand, Debug)]
pub enum ModManagerCliCommands {
	Check(CheckArgs),
	Config(ConfigArgs),
}

#[derive(Parser, Debug)]
pub struct CheckArgs {
	pub playset_path: PathBuf,
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
	#[command(subcommand)]
	pub command: ModManagerCliConfigCommands,
}

#[derive(Subcommand, Debug)]
pub enum ModManagerCliConfigCommands {
	Set(SetConfigArgs),
	Show,
}

#[derive(Parser, Debug)]
pub struct SetConfigArgs {
	#[command(subcommand)]
	pub command: ModManagerCliSetCommands,
}

#[derive(Subcommand, Debug)]
pub enum ModManagerCliSetCommands {
	SteamPath(PathArgs),
	ParadoxDataPath(PathArgs),
	GamePath(GamePathArgs),
}
#[derive(Parser, Debug)]
pub struct PathArgs {
	pub path: PathBuf,
}

#[derive(Parser, Debug)]
pub struct GamePathArgs {
	pub game_name: String,
	pub path: PathBuf,
}
