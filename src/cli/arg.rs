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
	MergePlan(MergePlanArgs),
	Merge(MergeArgs),
	Data(DataArgs),
	Config(ConfigArgs),
}

#[derive(Parser, Debug)]
#[command(
	about = "检查 playset 并输出规则发现",
	after_help = "示例:\n  foch check ./playlist.json\n  foch check ./playlist.json --strict\n  foch check ./playlist.json --analysis-mode semantic --channel strict\n  foch check ./playlist.json --no-game-base\n  foch check ./playlist.json --graph-out graph.dot --graph-format dot\n  foch check ./playlist.json --format json --output result.json"
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
	pub parse_issue_report: Option<PathBuf>,

	#[arg(long)]
	pub no_game_base: bool,

	#[arg(long)]
	pub no_color: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CheckOutputFormat {
	Text,
	Json,
}

#[derive(Parser, Debug)]
#[command(
	about = "Generate a deterministic merge plan for a playset",
	after_help = "Examples:\n  foch merge-plan ./playlist.json\n  foch merge-plan ./playlist.json --format json --output plan.json\n  foch merge-plan ./playlist.json --no-game-base"
)]
pub struct MergePlanArgs {
	pub playset_path: PathBuf,

	#[arg(long, value_enum, default_value_t = MergePlanOutputFormat::Text)]
	pub format: MergePlanOutputFormat,

	#[arg(long)]
	pub output: Option<PathBuf>,

	#[arg(long)]
	pub no_game_base: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum MergePlanOutputFormat {
	Text,
	Json,
}

#[derive(Parser, Debug)]
#[command(
	about = "Generate a merged mod directory and revalidate it",
	after_help = "Examples:\n  foch merge ./playlist.json --out ./merged-mod\n  foch merge ./playlist.json --out ./merged-mod --force\n  foch merge ./playlist.json --out ./merged-mod --no-game-base"
)]
pub struct MergeArgs {
	pub playset_path: PathBuf,

	#[arg(long)]
	pub out: PathBuf,

	#[arg(long)]
	pub force: bool,

	#[arg(long)]
	pub no_game_base: bool,
}

#[derive(Parser, Debug)]
#[command(
	about = "管理可分发的基础游戏数据",
	after_help = "Examples:\n  foch data list\n  foch data install eu4 --game-version auto\n  foch data build eu4 --from-game-path /path/to/eu4 --game-version auto --install\n  foch data build eu4 --from-game-path /path/to/eu4 --game-version auto --profile-out ./build-profile.json --output-dir ./dist/data --release-asset"
)]
pub struct DataArgs {
	#[command(subcommand)]
	pub command: FochCliDataCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliDataCommands {
	Install(DataInstallArgs),
	Build(DataBuildArgs),
	List(DataListArgs),
}

#[derive(Parser, Debug)]
pub struct DataInstallArgs {
	pub game_name: String,

	#[arg(long, default_value = "auto")]
	pub game_version: String,

	#[arg(long)]
	pub release_tag: Option<String>,
}

#[derive(Parser, Debug)]
pub struct DataBuildArgs {
	pub game_name: String,

	#[arg(long)]
	pub from_game_path: PathBuf,

	#[arg(long, default_value = "auto")]
	pub game_version: String,

	#[arg(long)]
	pub install: bool,

	#[arg(long)]
	pub output_dir: Option<PathBuf>,

	#[arg(long)]
	pub release_asset: bool,

	#[arg(long)]
	pub profile_out: Option<PathBuf>,
}

#[derive(Parser, Debug)]
pub struct DataListArgs {
	#[arg(long)]
	pub json: bool,
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
