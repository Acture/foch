//! `foch-mq` — the merge-quality harness CLI.
//!
//! Offline subcommands (`run`, `learn`, `extract-fixtures`) always build; the
//! network ones (`discover`, `fetch`, `all`) require the `steam` feature.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use foch_merge_quality::{CmdResult, archive, config, fixtures, lifecycle, orchestrate, symbols};

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
	/// EU4 install root. Overrides EU4_ROOT, foch config, and Steam discovery.
	#[arg(long)]
	game_root: Option<PathBuf>,
	/// Steam root. Overrides foch config and automatic Steam discovery.
	#[arg(long)]
	steam_root: Option<PathBuf>,
	/// Repository-local append-only dataset and ignored object store.
	#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/dataset"))]
	dataset_root: PathBuf,
	/// Directory for results.json / report.md / rules.md.
	#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/results"))]
	results_dir: PathBuf,
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
	/// Snapshot every fully-local corpus case into the content-addressed dataset.
	Collect {
		/// Cap on cases collected (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
	},
	/// Measure the latest snapshot of every case in isolated child processes.
	Measure {
		/// Per-case timeout in seconds.
		#[arg(long, default_value_t = 600)]
		timeout_secs: u64,
		/// Cap on cases measured (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
	},
	/// Render baseline.json and report.md from terminal measurement records.
	Report {
		/// Restrict to measurements from this executable BLAKE3 hash.
		#[arg(long)]
		executable_hash: Option<String>,
		/// Restrict to measurements from this scorer-config hash.
		#[arg(long)]
		config_hash: Option<String>,
		/// Cap on latest cases included (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
	},
	/// Collect, measure, and report the full latest local corpus.
	Baseline {
		/// Per-case timeout in seconds.
		#[arg(long, default_value_t = 600)]
		timeout_secs: u64,
		/// Cap on cases (0 = all).
		#[arg(long, default_value_t = 0)]
		limit: usize,
	},
	/// Export metadata only, semantic payloads, or full payloads.
	Export {
		#[arg(long, value_enum, default_value_t = ExportKind::Metadata)]
		profile: ExportKind,
		#[arg(long)]
		out: PathBuf,
	},
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
	/// Score one immutable snapshot (internal baseline worker).
	#[command(hide = true)]
	MeasureOne {
		#[arg(long)]
		snapshot_id: String,
		#[arg(long)]
		output_dir: PathBuf,
		#[arg(long)]
		basegame_root: Option<PathBuf>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ExportKind {
	Metadata,
	Semantic,
	Full,
}

fn main() -> CmdResult {
	let Cli {
		corpus,
		workshop_dir,
		game_root,
		steam_root,
		dataset_root,
		results_dir,
		cmd,
	} = Cli::parse();

	match cmd {
		Cmd::Collect { limit } => {
			let discovery = discover(&game_root, &workshop_dir, &steam_root)?;
			let summary = lifecycle::collect(&lifecycle::CollectOptions {
				corpus: &corpus,
				dataset_root: &dataset_root,
				discovery: &discovery,
				limit,
			})?;
			eprintln!(
				"[collect] {} snapshots, {} unique objects, {} logical bytes",
				summary.snapshots, summary.unique_objects, summary.logical_bytes
			);
			Ok(())
		}
		Cmd::Measure {
			timeout_secs,
			limit,
		} => {
			let executable = std::env::current_exe()?;
			let game = discover_game(&game_root, &steam_root)?;
			let summary = lifecycle::measure(&lifecycle::MeasureOptions {
				dataset_root: &dataset_root,
				timeout: Duration::from_secs(timeout_secs),
				limit,
				executable: &executable,
				basegame_root: Some(&game.game_root),
			})?;
			eprintln!(
				"[measure] selected={} cached={} measured={} failed={}",
				summary.selected, summary.cached, summary.measured, summary.failed
			);
			Ok(())
		}
		Cmd::Report {
			executable_hash,
			config_hash,
			limit,
		} => lifecycle::report(&lifecycle::ReportOptions {
			dataset_root: &dataset_root,
			output_dir: &results_dir,
			executable_hash: executable_hash.as_deref(),
			config_hash: config_hash.as_deref(),
			limit,
		}),
		Cmd::Baseline {
			timeout_secs,
			limit,
		} => {
			let discovery = discover(&game_root, &workshop_dir, &steam_root)?;
			let collected = lifecycle::collect(&lifecycle::CollectOptions {
				corpus: &corpus,
				dataset_root: &dataset_root,
				discovery: &discovery,
				limit,
			})?;
			eprintln!("[baseline] collected {} snapshots", collected.snapshots);
			let executable = std::env::current_exe()?;
			let measured = lifecycle::measure(&lifecycle::MeasureOptions {
				dataset_root: &dataset_root,
				timeout: Duration::from_secs(timeout_secs),
				limit,
				executable: &executable,
				basegame_root: Some(&discovery.game_root),
			})?;
			lifecycle::report(&lifecycle::ReportOptions {
				dataset_root: &dataset_root,
				output_dir: &results_dir,
				executable_hash: Some(&measured.executable_hash),
				config_hash: Some(&measured.config_hash),
				limit,
			})
		}
		Cmd::Export { profile, out } => {
			let profile = match profile {
				ExportKind::Metadata => lifecycle::DatasetExportProfile::Metadata,
				ExportKind::Semantic => lifecycle::DatasetExportProfile::Semantic,
				ExportKind::Full => lifecycle::DatasetExportProfile::Full,
			};
			lifecycle::export_dataset(&lifecycle::ExportOptions {
				dataset_root: &dataset_root,
				output_dir: &out,
				profile,
			})
		}
		Cmd::Run { limit, keep } => orchestrate::run(&orchestrate::RunOptions {
			corpus: &corpus,
			workshop_dir: &legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
			results_dir: &results_dir,
			limit,
			keep,
			isolate: true,
		}),
		Cmd::Learn => {
			let discovery = discover(&game_root, &workshop_dir, &steam_root)?;
			let workshop = discovery
				.workshop
				.roots
				.first()
				.ok_or("no Workshop root discovered")?;
			orchestrate::learn(&results_dir, workshop, Some(&discovery.game_root))
		}
		Cmd::Symbols { limit } => symbols::run(
			&corpus,
			&legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
			&results_dir,
			limit,
		),
		Cmd::ScoreOne { id } => orchestrate::score_one(
			&corpus,
			&legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
			&id,
		),
		Cmd::MeasureOne {
			snapshot_id,
			output_dir,
			basegame_root,
		} => lifecycle::measure_one(
			&dataset_root,
			&snapshot_id,
			&output_dir,
			basegame_root.as_deref(),
		),
		Cmd::ExtractFixtures { out, ids } => {
			// Stage the slices in a temp dir, then pack them into the single
			// committed compressed archive (no loose third-party files in-repo).
			let staging = tempfile::tempdir()?;
			fixtures::extract(
				&corpus,
				&legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
				staging.path(),
				&ids,
			)?;
			archive::pack_dir(staging.path(), &out)?;
			eprintln!("[extract] packed corpus -> {}", out.display());
			Ok(())
		}
		#[cfg(feature = "steam")]
		Cmd::Discover { max_items } => foch_merge_quality::steam::discover(&corpus, max_items),
		#[cfg(feature = "steam")]
		Cmd::Fetch { fetch_n, min_subs } => foch_merge_quality::fetch::fetch(
			&corpus,
			&legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
			fetch_n,
			min_subs,
		),
		#[cfg(feature = "steam")]
		Cmd::All => {
			foch_merge_quality::steam::discover(&corpus, 300)?;
			orchestrate::run(&orchestrate::RunOptions {
				corpus: &corpus,
				workshop_dir: &legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
				results_dir: &results_dir,
				limit: 0,
				keep: false,
				isolate: true,
			})
		}
	}
}

fn discover(
	game_root: &Option<PathBuf>,
	workshop_dir: &Option<PathBuf>,
	steam_root: &Option<PathBuf>,
) -> Result<config::Eu4Discovery, Box<dyn std::error::Error>> {
	config::discover_eu4(&config::DiscoveryOverrides {
		game_root: game_root.clone(),
		workshop_dir: workshop_dir.clone(),
		steam_root: steam_root.clone(),
	})
	.map_err(Into::into)
}

fn discover_game(
	game_root: &Option<PathBuf>,
	steam_root: &Option<PathBuf>,
) -> Result<config::Eu4GameDiscovery, Box<dyn std::error::Error>> {
	config::discover_eu4_game(&config::DiscoveryOverrides {
		game_root: game_root.clone(),
		workshop_dir: None,
		steam_root: steam_root.clone(),
	})
	.map_err(Into::into)
}

fn legacy_workshop(
	game_root: &Option<PathBuf>,
	workshop_dir: &Option<PathBuf>,
	steam_root: &Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
	if let Some(workshop_dir) = workshop_dir {
		return Ok(workshop_dir.clone());
	}
	discover(game_root, workshop_dir, steam_root)?
		.workshop
		.roots
		.into_iter()
		.next()
		.ok_or_else(|| "no Workshop root discovered".into())
}
