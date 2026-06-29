//! `foch-mq` — the merge-quality harness CLI.
//!
//! Offline subcommands (`run`, `learn`, `extract-fixtures`) always build; the
//! network ones (`discover`, `fetch`, `all`) require the `steam` feature.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use foch_merge_quality::{CmdResult, config, fixtures, orchestrate};

#[derive(Parser)]
#[command(
	name = "foch-mq",
	about = "Measure foch merge quality against community compatches"
)]
struct Cli {
	/// Path to corpus.json.
	#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus.json"))]
	corpus: PathBuf,
	/// Steam Workshop content dir (default: platform guess / $STEAM_WORKSHOP_DIR).
	#[arg(long)]
	workshop_dir: Option<PathBuf>,
	/// Directory for results.json / report.md / rules.md.
	#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/results"))]
	results_dir: PathBuf,
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
	/// Score every locally-available compatch; write results.json + report.md.
	Run {
		/// Cap on cases scored (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
		/// Keep per-case temp merge dirs.
		#[arg(long)]
		keep: bool,
	},
	/// Classify how humans resolved overlaps (results.json -> rules.md).
	Learn,
	/// Extract scored-file slices into the test fixture tree.
	ExtractFixtures {
		/// Output fixtures dir.
		#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))]
		out: PathBuf,
		/// Compatch id(s) to extract (repeat; empty = all fully-local).
		#[arg(long = "id")]
		ids: Vec<String>,
	},
	/// Discover compatches from the Steam Workshop and (re)build corpus.json.
	#[cfg(feature = "steam")]
	Discover {
		#[arg(long, default_value_t = 300)]
		max_items: usize,
	},
	/// Download curated compatches + their mods via SteamCMD (no subscribe).
	#[cfg(feature = "steam")]
	Fetch {
		#[arg(long, default_value_t = 15)]
		fetch_n: usize,
		#[arg(long, default_value_t = 100)]
		min_subs: i64,
	},
	/// discover, then run.
	#[cfg(feature = "steam")]
	All,
}

fn main() -> CmdResult {
	let cli = Cli::parse();
	let workshop = cli
		.workshop_dir
		.clone()
		.unwrap_or_else(config::default_workshop_dir);

	match cli.cmd {
		Cmd::Run { limit, keep } => orchestrate::run(&orchestrate::RunOptions {
			corpus: &cli.corpus,
			workshop_dir: &workshop,
			results_dir: &cli.results_dir,
			limit,
			keep,
		}),
		Cmd::Learn => orchestrate::learn(&cli.results_dir),
		Cmd::ExtractFixtures { out, ids } => fixtures::extract(&cli.corpus, &workshop, &out, &ids),
		#[cfg(feature = "steam")]
		Cmd::Discover { max_items } => foch_merge_quality::steam::discover(&cli.corpus, max_items),
		#[cfg(feature = "steam")]
		Cmd::Fetch { fetch_n, min_subs } => {
			foch_merge_quality::fetch::fetch(&cli.corpus, &workshop, fetch_n, min_subs)
		}
		#[cfg(feature = "steam")]
		Cmd::All => {
			foch_merge_quality::steam::discover(&cli.corpus, 300)?;
			orchestrate::run(&orchestrate::RunOptions {
				corpus: &cli.corpus,
				workshop_dir: &workshop,
				results_dir: &cli.results_dir,
				limit: 0,
				keep: false,
			})
		}
	}
}
