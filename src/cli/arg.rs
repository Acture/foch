use clap::{Parser, Subcommand, ValueEnum};
use clap_verbosity_flag::{Verbosity, WarnLevel};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = "Foch: Paradox Mod 静态分析工具", long_about = None)]
pub struct FochCli {
	#[command(subcommand)]
	pub command: FochCliCommands,

	#[command(flatten)]
	pub verbose: Verbosity<WarnLevel>,
}

#[derive(Subcommand, Debug)]
pub enum FochCliCommands {
	Check(CheckArgs),
	Config(ConfigArgs),
}

#[derive(Parser, Debug)]
#[command(
	about = "检查 playset 并输出规则发现",
	after_help = "示例:\n  foch check ./playlist.json\n  foch check ./playlist.json --strict\n  foch check ./playlist.json --analysis-mode semantic --channel strict\n  foch check ./playlist.json --include-game-base\n  foch check ./playlist.json --graph-out graph.dot --graph-format dot\n  foch check ./playlist.json --format json --output result.json"
)]
pub struct CheckArgs {
	pub playset_path: PathBuf,

	#[arg(long, value_enum, default_value_t = CheckOutputFormat::Text)]
	pub format: CheckOutputFormat,

	#[arg(long)]
	pub output: Option<PathBuf>,

	#[arg(long)]
	pub strict: bool,

	#[arg(long, value_enum, default_value_t = AnalysisModeArg::Semantic)]
	pub analysis_mode: AnalysisModeArg,

	#[arg(long, value_enum, default_value_t = CheckChannelArg::All)]
	pub channel: CheckChannelArg,

	#[arg(long)]
	pub graph_out: Option<PathBuf>,

	#[arg(long, value_enum, default_value_t = GraphFormatArg::Json)]
	pub graph_format: GraphFormatArg,

	#[arg(long)]
	pub include_game_base: bool,

	#[arg(long)]
	pub no_color: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CheckOutputFormat {
	Text,
	Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AnalysisModeArg {
	Basic,
	Semantic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CheckChannelArg {
	Strict,
	All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphFormatArg {
	Json,
	Dot,
}

#[derive(Parser, Debug)]
#[command(about = "查看和维护本地配置")]
pub struct ConfigArgs {
	#[command(subcommand)]
	pub command: FochCliConfigCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliConfigCommands {
	Set(SetConfigArgs),
	Show(ShowConfigArgs),
	Validate,
}

#[derive(Parser, Debug)]
pub struct ShowConfigArgs {
	#[arg(long)]
	pub json: bool,
}

#[derive(Parser, Debug)]
pub struct SetConfigArgs {
	#[command(subcommand)]
	pub command: FochCliSetCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliSetCommands {
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
