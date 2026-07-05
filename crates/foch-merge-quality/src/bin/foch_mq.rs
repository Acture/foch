//! `foch-mq` — the merge-quality harness CLI.
//!
//! Offline subcommands (`run`, `learn`, `extract-fixtures`) always build; the
//! network ones (`discover`, `fetch`, `all`) require the `steam` feature.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use foch_merge_quality::{CmdResult, archive, config, fixtures, orchestrate, symbols};

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
	/// Scan full local mods for compatch-anchored cross-file symbol conflicts.
	Symbols {
		/// Cap on cases scanned (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
	},
	/// Score a single compatch, print its CaseResult JSON (internal: `run`
	/// spawns this per case to isolate foch crashes).
	#[command(hide = true)]
	ScoreOne {
		#[arg(long = "id")]
		id: String,
	},
	/// Extract full local cases and pack them into the committed corpus archive.
	ExtractFixtures {
		/// Output archive (gzip-compressed tar of the fixture tree).
		#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/corpus.tar.gz"))]
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
			isolate: true,
		}),
		Cmd::Learn => orchestrate::learn(&cli.results_dir, &workshop),
		Cmd::Symbols { limit } => symbols::run(&cli.corpus, &workshop, &cli.results_dir, limit),
		Cmd::ScoreOne { id } => orchestrate::score_one(&cli.corpus, &workshop, &id),
		Cmd::ExtractFixtures { out, ids } => {
			// Stage the slices in a temp dir, then pack them into the single
			// committed compressed archive (no loose third-party files in-repo).
			let staging = tempfile::tempdir()?;
			fixtures::extract(&cli.corpus, &workshop, staging.path(), &ids)?;
			archive::pack_dir(staging.path(), &out)?;
			eprintln!("[extract] packed corpus -> {}", out.display());
			Ok(())
		}
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
				isolate: true,
			})
		}
	}
}
