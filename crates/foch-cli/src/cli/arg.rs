use clap::{Parser, Subcommand, ValueEnum};
use clap_verbosity_flag::{Verbosity, WarnLevel};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(
	author,
	version,
	about = "Foch: Paradox mod analysis and merge toolkit",
	long_about = None
)]
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
	Graph(GraphArgs),
	Simplify(SimplifyArgs),
	Data(DataArgs),
	Cache(FochCliCacheArgs),
	Config(ConfigArgs),
	Lsp(LspArgs),
}

/// Run the foch language server on stdio. The subcommand intentionally
/// accepts (and ignores) any trailing arguments so that LSP clients which
/// append transport-mode hints like `--stdio` to the spawn command line do
/// not trip clap's unknown-argument check.
#[derive(Parser, Debug)]
#[command(
	about = "Run the foch language server on stdio",
	after_help = "VS Code extension and other LSP clients spawn this with stdio transport.\nNo arguments are required; trailing args (e.g. `--stdio`) are accepted and ignored."
)]
pub struct LspArgs {
	#[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
	pub _passthrough: Vec<String>,
}

#[derive(Parser, Debug)]
#[command(
	about = "Check a playset and report findings",
	after_help = "Examples:\n  foch check ./playlist.json\n  foch check ./playlist.json --strict\n  foch check ./playlist.json --analysis-mode semantic --channel strict\n  foch check ./playlist.json --no-game-base\n  foch check ./playlist.json --format json --output result.json"
)]
pub struct CheckArgs {
	#[arg(default_value = None)]
	pub playset_path: Option<PathBuf>,

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
	pub parse_issue_report: Option<PathBuf>,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
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
	#[arg(default_value = None)]
	pub playset_path: Option<PathBuf>,

	#[arg(long, value_enum, default_value_t = MergePlanOutputFormat::Text)]
	pub format: MergePlanOutputFormat,

	#[arg(long)]
	pub output: Option<PathBuf>,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
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
	after_help = "Examples:\n  foch merge ./playlist.json --out ./merged-mod\n  foch merge ./playlist.json --out ./merged-mod --interactive\n  foch merge ./playlist.json --out ./merged-mod --fallback\n  foch merge ./playlist.json --out ./merged-mod --force  # implies --fallback\n  foch merge ./playlist.json --out ./merged-mod --no-game-base"
)]
pub struct MergeArgs {
	#[arg(default_value = None)]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub out: PathBuf,

	#[arg(long)]
	pub force: bool,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
	#[arg(long)]
	pub no_game_base: bool,

	/// Treat replace_path declarations as no-ops; merge as if they were absent.
	#[arg(long)]
	pub ignore_replace_path: bool,

	/// Drop one declared dependency edge from the local merge DAG (format: mod:dep).
	#[arg(long = "ignore-dep", value_name = "MOD:DEP")]
	pub ignore_dep: Vec<IgnoreDepArg>,

	/// Load local foch.toml overrides from this file instead of the default search path.
	#[arg(long, value_name = "PATH")]
	pub config: Option<PathBuf>,

	/// Prompt for unresolved structural conflicts and persist decisions to foch.toml.
	#[arg(short = 'i', long)]
	pub interactive: bool,

	/// Enable last-writer fallback for unresolved structural merge conflicts.
	///
	/// `--force` also enables this fallback.
	#[arg(long)]
	pub fallback: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IgnoreDepArg {
	pub mod_id: String,
	pub dep_id: String,
}

impl FromStr for IgnoreDepArg {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		if value.matches(':').count() != 1 {
			return Err("expected exactly one ':' in MOD:DEP".to_string());
		}
		let (mod_id, dep_id) = value
			.split_once(':')
			.expect("exactly one ':' was validated above");
		let mod_id = mod_id.trim();
		let dep_id = dep_id.trim();
		if mod_id.is_empty() || dep_id.is_empty() {
			return Err("MOD and DEP must both be non-empty".to_string());
		}
		Ok(Self {
			mod_id: mod_id.to_string(),
			dep_id: dep_id.to_string(),
		})
	}
}

#[derive(Parser, Debug)]
#[command(
	about = "Export runtime graphs and family semantic graph reports",
	after_help = "Examples:\n  foch graph ./playlist.json --out ./graphs\n  foch graph ./playlist.json --out ./graphs --scope mods --format both\n  foch graph ./playlist.json --out ./graphs --root scripted_effect:shared_effect\n  foch graph ./playlist.json --out ./graphs --mode semantic --family common/client_states"
)]
pub struct GraphArgs {
	#[arg(default_value = None)]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub out: PathBuf,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
	#[arg(long)]
	pub no_game_base: bool,

	#[arg(long, value_enum, default_value_t = GraphModeArg::Calls)]
	pub mode: GraphModeArg,

	#[arg(long, value_enum, default_value_t = GraphScopeArg::All)]
	pub scope: GraphScopeArg,

	#[arg(long, value_enum, default_value_t = GraphArtifactFormatArg::Both)]
	pub format: GraphArtifactFormatArg,

	#[arg(long)]
	pub root: Option<String>,

	#[arg(long)]
	pub family: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphModeArg {
	Calls,
	Semantic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphScopeArg {
	Workspace,
	Base,
	Mods,
	All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphArtifactFormatArg {
	Json,
	Dot,
	Both,
}

#[derive(Parser, Debug)]
#[command(
	about = "Remove base-equivalent definitions from a target mod",
	after_help = "Examples:\n  foch simplify ./playlist.json --target 1234 --out ./mod-clean\n  foch simplify ./playlist.json --target 1234 --in-place"
)]
pub struct SimplifyArgs {
	#[arg(default_value = None)]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub target: String,

	#[arg(long)]
	pub out: Option<PathBuf>,

	#[arg(long)]
	pub in_place: bool,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
	#[arg(long)]
	pub no_game_base: bool,
}

#[derive(Parser, Debug)]
#[command(
	about = "Manage distributable base game data",
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

#[derive(Parser, Debug)]
#[command(
	about = "Inspect and maintain the local parse cache",
	after_help = "Examples:\n  foch cache stats\n  foch cache gc\n  foch cache gc --cap-bytes 536870912\n  foch cache clean"
)]
pub struct FochCliCacheArgs {
	#[command(subcommand)]
	pub command: FochCliCacheCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliCacheCommands {
	Stats,
	Gc(FochCliCacheGcArgs),
	Clean,
}

#[derive(Parser, Debug)]
pub struct FochCliCacheGcArgs {
	#[arg(long)]
	pub cap_bytes: Option<u64>,
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

#[derive(Parser, Debug)]
#[command(about = "Inspect and maintain local configuration")]
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

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	#[test]
	fn ignore_dep_arg_parses_mod_dep_pair() {
		let parsed = IgnoreDepArg::from_str("3378403419:1999055990").expect("parse pair");

		assert_eq!(parsed.mod_id, "3378403419");
		assert_eq!(parsed.dep_id, "1999055990");
	}

	#[test]
	fn ignore_dep_arg_rejects_invalid_format() {
		assert!(IgnoreDepArg::from_str("3378403419").is_err());
		assert!(IgnoreDepArg::from_str("3378403419:1999055990:extra").is_err());
		assert!(IgnoreDepArg::from_str("3378403419:").is_err());
	}

	#[test]
	fn merge_command_accepts_repeatable_ignore_dep_flags() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--ignore-dep",
			"a:b",
			"--ignore-dep",
			"c:d",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command else {
			panic!("expected merge command");
		};
		assert_eq!(
			args.ignore_dep,
			vec![
				IgnoreDepArg {
					mod_id: "a".to_string(),
					dep_id: "b".to_string(),
				},
				IgnoreDepArg {
					mod_id: "c".to_string(),
					dep_id: "d".to_string(),
				},
			]
		);
	}

	#[test]
	fn merge_command_accepts_interactive_flag() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--interactive",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command else {
			panic!("expected merge command");
		};
		assert!(args.interactive);
	}
}
