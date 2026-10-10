use clap::{Parser, Subcommand, ValueEnum};
use clap_verbosity_flag::{Verbosity, WarnLevel};
use foch::game::eu4::{CWT_SCHEMA_OVERRIDE_ENV, EMBEDDED_CWT_SCHEMA_ID, cwt_schema_override};
use foch::model::SymbolKind;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

#[derive(Parser, Debug)]
#[command(
	author,
	version,
	long_version = long_version(),
	about = "Foch: Paradox mod analysis and merge toolkit",
	long_about = None
)]
pub struct FochCli {
	/// Without a subcommand, `foch` opens the read-only analysis browser.
	#[command(subcommand)]
	pub command: Option<FochCliCommands>,

	#[command(flatten)]
	pub verbose: Verbosity<WarnLevel>,
}

/// `--version` names the CWT schema compiled into the binary: the schema
/// decides output bytes, so two builds write the same mod only if both match.
fn long_version() -> &'static str {
	static VERSION: OnceLock<String> = OnceLock::new();
	VERSION.get_or_init(|| {
		let mut version = format!(
			"{}\ncwt-schema {EMBEDDED_CWT_SCHEMA_ID} (embedded)",
			env!("CARGO_PKG_VERSION")
		);
		if let Some(dir) = cwt_schema_override() {
			version.push_str(&format!(
				"\ncwt-schema overridden by {CWT_SCHEMA_OVERRIDE_ENV}={}",
				dir.display()
			));
		}
		version
	})
}

#[derive(Subcommand, Debug)]
pub enum FochCliCommands {
	Test(TestArgs),
	Check(CheckArgs),
	Merge(MergeArgs),
	Graph(GraphArgs),
	Simplify(SimplifyArgs),
	Data(DataArgs),
	Cache(FochCliCacheArgs),
	Config(ConfigArgs),
	Input(InputArgs),
	Lsp(LspArgs),
}

#[derive(Parser, Debug)]
#[command(
	about = "Collect or run EU4 event tests from #test annotations",
	after_help = foch_annotation::builtin::TEST_DESCRIPTION
)]
pub struct TestArgs {
	/// Mod directory, its events directory, an event .txt file, or
	/// FILE::EVENT[::NAME] to select inside a file.
	#[arg(default_value = ".", value_name = "PATH")]
	pub path: PathBuf,
	#[arg(long, conflicts_with_all = ["out", "no_run"])]
	pub collect_only: bool,
	/// Generate the test layer and session bundles into --out without
	/// launching the game, like `cargo test --no-run`.
	#[arg(long)]
	pub no_run: bool,
	/// Keyword expression over node IDs, such as "reform and not slow".
	#[arg(short = 'k', value_name = "EXPRESSION")]
	pub filter: Option<String>,
	/// Mark expression over #mark labels, such as "war and not slow".
	#[arg(short = 'm', value_name = "EXPRESSION")]
	pub marks: Option<String>,
	/// Also run #ignore cases.
	#[arg(long, conflicts_with = "ignored")]
	pub include_ignored: bool,
	/// Run only #ignore cases.
	#[arg(long)]
	pub ignored: bool,
	/// Run every case in its own game session.
	#[arg(long)]
	pub isolate: bool,
	/// Launch the game despite findings from Foch's own script model.
	/// Malformed tests still block.
	#[arg(long)]
	pub run_anyway: bool,
	#[arg(long, value_enum, default_value_t = CheckOutputFormat::Text)]
	pub format: CheckOutputFormat,
	/// Describe annotations and parameters without a game installation.
	#[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "NAME", conflicts_with_all = ["collect_only", "out", "filter"])]
	pub api: Option<String>,
	/// EU4 install to launch; defaults to the configured or Steam install.
	#[arg(long, value_name = "PATH")]
	pub game_path: Option<PathBuf>,
	/// Real EU4 user directory to guard; defaults to
	/// Documents/Paradox Interactive/Europa Universalis IV.
	#[arg(long, value_name = "PATH")]
	pub eu4_user_dir: Option<PathBuf>,
	/// Maximum seconds per game launch.
	#[arg(long, default_value = "300", value_parser = clap::value_parser!(u64).range(1..))]
	pub timeout: u64,
	/// Write bundles, logs, result.json and junit.xml here; must be new or
	/// empty and outside the mod. Defaults to a new temporary directory.
	#[arg(long, value_name = "PATH")]
	pub out: Option<PathBuf>,
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
	after_help = "Examples:\n  foch check ./playlist.json\n  foch check ./foch.toml\n  foch check ./playlist.json --strict\n  foch check ./playlist.json --analysis-mode semantic --channel strict\n  foch check ./playlist.json --no-game-base\n  foch check ./playlist.json --format json --output result.json"
)]
pub struct CheckArgs {
	#[arg(default_value = None, value_name = "INPUT_SOURCE")]
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

#[derive(Clone, Parser, Debug, Eq, PartialEq)]
#[command(
	about = "Analyze a merge, review its frozen plan, then optionally commit it",
	after_help = "Examples:\n  foch merge ./playlist.json --out ./merged-mod                 # analyze, review, then confirm in a TTY\n  foch merge ./foch.toml --out ./merged-mod --confirm          # analyze and explicitly commit\n  foch merge ./playlist.json --out ./merged-mod --non-interactive  # analysis only\n  foch merge ./playlist.json --out ./new-merged-mod --confirm --non-interactive  # CI: new/empty path\n  foch merge ./playlist.json --out ./merged-mod --force --confirm\n  foch merge ./playlist.json --out ./merged-mod --no-game-base"
)]
pub struct MergeArgs {
	#[arg(default_value = None, value_name = "INPUT_SOURCE")]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub out: PathBuf,

	/// Emit explicit fallbacks for deferred conflicts where supported. Safe
	/// units are committed with or without this flag.
	#[arg(long)]
	pub force: bool,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
	#[arg(long)]
	pub no_game_base: bool,

	/// Also write unchanged vanilla base-game files into the merged output (off by default; the game already ships them).
	#[arg(long)]
	pub include_base: bool,

	/// Leave a mod of the current EU4 playset out of the analysis, by
	/// Workshop id or `#POSITION` as `foch input inspect` lists them.
	/// Repeatable; only without INPUT_SOURCE. The result does not represent
	/// the full playset.
	#[arg(long = "exclude", value_name = "MOD")]
	pub exclude: Vec<String>,

	/// Merge divergent same-name GUI containers into scroll-stack parents instead of manual conflicts.
	#[arg(long)]
	pub gui_scroll_merge: bool,

	/// Treat replace_path declarations as no-ops; merge as if they were absent.
	#[arg(long)]
	pub ignore_replace_path: bool,

	/// Drop one declared dependency edge from the local merge DAG (format: mod:dep).
	#[arg(long = "ignore-dep", value_name = "MOD:DEP")]
	pub ignore_dep: Vec<IgnoreDepArg>,

	/// Load local foch.toml overrides from this file instead of the default search path.
	#[arg(long, value_name = "PATH")]
	pub config: Option<PathBuf>,

	/// Annotate merged definitions with their source mods using inline
	/// comments plus `.foch/foch-provenance.json` and
	/// `.foch/foch-merge-trace.json` sidecars. Supported GUI controls append
	/// sources to their static tooltips; diplomatic actions expose `Base:` /
	/// `Modified by:` in tooltip localisation. Off by default; output is
	/// byte-identical when omitted.
	#[arg(long)]
	pub provenance: bool,

	/// Commit without the review prompt. A non-empty --out still requires a
	/// separate TTY overwrite confirmation.
	#[arg(long)]
	pub confirm: bool,

	/// Disable TTY-detected prompts. This does not imply --confirm.
	#[arg(long, alias = "no-interactive")]
	pub non_interactive: bool,

	/// Show every review unit. By default, show the first 20 of each
	/// disposition, with totals and explicit counts of omitted units.
	#[arg(long)]
	pub review_all: bool,

	/// Use the simple stdin/stderr prompt instead of the ratatui interactive UI.
	#[arg(long)]
	pub cli_prompt: bool,

	/// Most merge units to analyze at once (default: the detected CPU count).
	/// Fewer run while their estimated memory would not fit; results are
	/// applied in plan order, so output does not depend on it. Interactive
	/// prompts still come in plan order, with the other units paused.
	#[arg(long, value_name = "N")]
	pub jobs: Option<NonZeroUsize>,

	/// Write the complete review as JSON to this file before confirmation:
	/// the mods and their dependency edges, every unit with its ordered
	/// contributors, each deferred unit's conflict tree with the competing
	/// candidates, and every decision point with the foch.toml resolutions
	/// that would persist it.
	#[arg(long, value_name = "PATH")]
	pub review_json: Option<PathBuf>,
}

impl MergeArgs {
	/// The arguments after `foch` that parse back to exactly these. Bare
	/// `foch` shows this for the analysis it runs, so a script or agent can
	/// run the same one.
	pub fn command_line(&self) -> Vec<String> {
		let mut args = vec!["merge".to_string()];
		if let Some(path) = &self.playset_path {
			args.push(path.display().to_string());
		}
		args.push("--out".to_string());
		args.push(self.out.display().to_string());
		let flags = [
			(self.force, "--force"),
			(self.no_game_base, "--no-game-base"),
			(self.include_base, "--include-base"),
			(self.gui_scroll_merge, "--gui-scroll-merge"),
			(self.ignore_replace_path, "--ignore-replace-path"),
			(self.provenance, "--provenance"),
			(self.confirm, "--confirm"),
			(self.non_interactive, "--non-interactive"),
			(self.review_all, "--review-all"),
			(self.cli_prompt, "--cli-prompt"),
		];
		args.extend(
			flags
				.into_iter()
				.filter(|(set, _)| *set)
				.map(|(_, flag)| flag.to_string()),
		);
		for dep in &self.ignore_dep {
			args.push("--ignore-dep".to_string());
			args.push(format!("{}:{}", dep.mod_id, dep.dep_id));
		}
		if let Some(config) = &self.config {
			args.push("--config".to_string());
			args.push(config.display().to_string());
		}
		if let Some(jobs) = self.jobs {
			args.push("--jobs".to_string());
			args.push(jobs.to_string());
		}
		if let Some(path) = &self.review_json {
			args.push("--review-json".to_string());
			args.push(path.display().to_string());
		}
		for name in &self.exclude {
			args.push("--exclude".to_string());
			args.push(name.clone());
		}
		args
	}
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
	after_help = "Examples:\n  foch graph ./playlist.json --out ./graphs\n  foch graph ./foch.toml --out ./graphs\n  foch graph ./playlist.json --out ./graphs --scope mods --format both\n  foch graph ./playlist.json --out ./graphs --root scripted_effect:shared_effect\n  foch graph ./playlist.json --out ./graphs --mode semantic --family common/client_states"
)]
pub struct GraphArgs {
	#[arg(default_value = None, value_name = "INPUT_SOURCE")]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub out: PathBuf,

	/// Skip loading vanilla game files; the lowest-precedence enabled mod
	/// is treated as a synthetic base for diff-and-merge.
	#[arg(long)]
	pub no_game_base: bool,

	/// Write the dependency-graph module partition report instead of graph artifacts.
	#[arg(long)]
	pub modules: bool,

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

	#[arg(long = "definition-kinds", value_delimiter = ',', value_parser = parse_definition_kind_arg)]
	pub definition_kinds: Vec<SymbolKind>,
}

const DEFINITION_KIND_VALUES: &[&str] = &[
	"event",
	"scripted_effect",
	"scripted_trigger",
	"decision",
	"diplomatic_action",
	"triggered_modifier",
];

fn parse_definition_kind_arg(value: &str) -> Result<SymbolKind, String> {
	let normalized = value.trim().to_ascii_lowercase();
	match normalized.as_str() {
		"event" => Ok(SymbolKind::Event),
		"scripted_effect" => Ok(SymbolKind::ScriptedEffect),
		"scripted_trigger" => Ok(SymbolKind::ScriptedTrigger),
		"decision" => Ok(SymbolKind::Decision),
		"diplomatic_action" => Ok(SymbolKind::DiplomaticAction),
		"triggered_modifier" => Ok(SymbolKind::TriggeredModifier),
		"" => Err("definition kind must not be empty".to_string()),
		_ => Err(format!(
			"unsupported definition kind '{value}'; expected one of: {}",
			DEFINITION_KIND_VALUES.join(", ")
		)),
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphModeArg {
	Calls,
	Semantic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum GraphScopeArg {
	Input,
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
	after_help = "Examples:\n  foch simplify ./playlist.json --target 1234 --out ./mod-clean\n  foch simplify ./foch.toml --target local_patch --out ./mod-clean\n  foch simplify ./playlist.json --target 1234 --in-place"
)]
pub struct SimplifyArgs {
	#[arg(default_value = None, value_name = "INPUT_SOURCE")]
	pub playset_path: Option<PathBuf>,

	#[arg(long)]
	pub target: String,

	/// Separate directory for the simplified copy; the source mod is never
	/// modified.
	#[arg(long)]
	pub out: PathBuf,

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

#[derive(Clone, Parser, Debug, Eq, PartialEq)]
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

impl DataBuildArgs {
	/// The arguments after `foch` that parse back to exactly these.
	pub fn command_line(&self) -> Vec<String> {
		let mut args = vec![
			"data".to_string(),
			"build".to_string(),
			self.game_name.clone(),
			"--from-game-path".to_string(),
			self.from_game_path.display().to_string(),
			"--game-version".to_string(),
			self.game_version.clone(),
		];
		if self.install {
			args.push("--install".to_string());
		}
		if let Some(dir) = &self.output_dir {
			args.push("--output-dir".to_string());
			args.push(dir.display().to_string());
		}
		if self.release_asset {
			args.push("--release-asset".to_string());
		}
		if let Some(path) = &self.profile_out {
			args.push("--profile-out".to_string());
			args.push(path.display().to_string());
		}
		args
	}
}

#[derive(Parser, Debug)]
pub struct DataListArgs {
	#[arg(long)]
	pub json: bool,
}

#[derive(Parser, Debug)]
#[command(
	about = "Inspect and maintain local foch caches",
	after_help = "Examples:\n  foch cache stats --layer all\n  foch cache list --layer mods\n  foch cache clean --layer all --byte-cap 1073741824\n  foch cache clear --layer all --yes\n  foch cache where"
)]
pub struct FochCliCacheArgs {
	#[command(subcommand)]
	pub command: FochCliCacheCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliCacheCommands {
	Stats(FochCliCacheStatsArgs),
	List(FochCliCacheListArgs),
	Clean(FochCliCacheCleanArgs),
	Clear(FochCliCacheClearArgs),
	Where,
}

#[derive(Parser, Debug)]
pub struct FochCliCacheStatsArgs {
	#[arg(long, value_enum)]
	pub layer: Option<FochCliCacheLayerArg>,
}

#[derive(Parser, Debug)]
pub struct FochCliCacheListArgs {
	#[arg(long, value_enum)]
	pub layer: Option<FochCliCacheLayerArg>,
}

#[derive(Parser, Debug)]
pub struct FochCliCacheCleanArgs {
	#[arg(long, value_enum)]
	pub layer: Option<FochCliCacheLayerArg>,

	#[arg(long, default_value_t = 30)]
	pub older_than: u32,

	#[arg(long)]
	pub byte_cap: Option<u64>,
}

#[derive(Parser, Debug)]
pub struct FochCliCacheClearArgs {
	#[arg(long, value_enum)]
	pub layer: Option<FochCliCacheLayerArg>,

	#[arg(long)]
	pub yes: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum FochCliCacheLayerArg {
	Parse,
	Mods,
	Diffs,
	DagBase,
	CwtRules,
	All,
}

#[derive(Parser, Debug)]
#[command(
	about = "Inspect a Foch input source",
	after_help = "Examples:\n  foch input inspect ./foch.toml\n  foch input inspect ~/Documents/Paradox\\ Interactive/Europa\\ Universalis\\ IV/dlc_load.json"
)]
pub struct InputArgs {
	#[command(subcommand)]
	pub command: FochCliInputCommands,
}

#[derive(Subcommand, Debug)]
pub enum FochCliInputCommands {
	Inspect(InputInspectArgs),
	Repair(InputRepairArgs),
}

/// Guide the repair of current-playset mods that cannot be analyzed. Foch
/// changes nothing itself: it names each broken Workshop item and can open
/// its page in Steam, where unsubscribing and subscribing again makes Steam
/// download it afresh.
#[derive(Parser, Debug)]
#[command(
	about = "Guide the repair of current-playset mods that cannot be analyzed",
	after_help = "Bare `foch` runs the same repair: R opens every broken mod, w the selected one.\n\nExamples:\n  foch input repair\n  foch input repair --open\n  foch input repair --open --mod 1804289844"
)]
pub struct InputRepairArgs {
	/// Open each Workshop page in Steam (`steam://url/CommunityFilePage/<id>`).
	#[arg(long)]
	pub open: bool,

	/// Only this mod, by Workshop id or `#POSITION`; repeatable. Any mod can
	/// be named, broken or not.
	#[arg(long = "mod", value_name = "MOD")]
	pub mods: Vec<String>,
}

#[derive(Parser, Debug)]
#[command(
	about = "Show the game and ordered mod inputs Foch will use",
	after_help = "Without INPUT_SOURCE, inspects the installed EU4 game and its current launcher playset exactly as bare `foch` does, reporting each mod's position, Workshop id and whether it can be analyzed.\n\nExamples:\n  foch input inspect\n  foch input inspect --format json\n  foch input inspect ./foch.toml"
)]
pub struct InputInspectArgs {
	#[arg(value_name = "INPUT_SOURCE")]
	pub source_path: Option<PathBuf>,

	/// `json` describes the current EU4 input for scripts and agents; it is
	/// available only without INPUT_SOURCE.
	#[arg(long, value_enum, default_value_t = CheckOutputFormat::Text)]
	pub format: CheckOutputFormat,
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

		let FochCliCommands::Merge(args) = cli.command.expect("subcommand") else {
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
	fn merge_command_accepts_non_interactive_flag() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--non-interactive",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command.expect("subcommand") else {
			panic!("expected merge command");
		};
		assert!(args.non_interactive);
		assert!(!args.confirm);
	}

	#[test]
	fn merge_command_accepts_confirm_flag() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--confirm",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command.expect("subcommand") else {
			panic!("expected merge command");
		};
		assert!(args.confirm);
		assert!(!args.non_interactive);
	}

	#[test]
	fn merge_command_accepts_no_interactive_alias() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--no-interactive",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command.expect("subcommand") else {
			panic!("expected merge command");
		};
		assert!(args.non_interactive);
	}

	#[test]
	fn merge_command_accepts_cli_prompt_flag() {
		let cli = FochCli::try_parse_from([
			"foch",
			"merge",
			"playlist.json",
			"--out",
			"merged",
			"--cli-prompt",
		])
		.expect("parse cli");

		let FochCliCommands::Merge(args) = cli.command.expect("subcommand") else {
			panic!("expected merge command");
		};
		assert!(args.cli_prompt);
	}

	#[test]
	fn graph_command_accepts_definition_kinds_flag() {
		let cli = FochCli::try_parse_from([
			"foch",
			"graph",
			"playlist.json",
			"--out",
			"graphs",
			"--definition-kinds=event,scripted_effect",
			"--definition-kinds",
			"triggered_modifier",
		])
		.expect("parse cli");

		let FochCliCommands::Graph(args) = cli.command.expect("subcommand") else {
			panic!("expected graph command");
		};
		assert_eq!(
			args.definition_kinds,
			vec![
				SymbolKind::Event,
				SymbolKind::ScriptedEffect,
				SymbolKind::TriggeredModifier,
			]
		);
	}

	#[test]
	fn graph_command_rejects_unknown_definition_kind() {
		let err = FochCli::try_parse_from([
			"foch",
			"graph",
			"playlist.json",
			"--out",
			"graphs",
			"--definition-kinds=garbage",
		])
		.expect_err("garbage kind should fail");
		let message = err.to_string();
		assert!(
			message.contains("unsupported definition kind 'garbage'"),
			"error: {message}"
		);
	}
}
