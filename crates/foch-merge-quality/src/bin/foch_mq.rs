//! `foch-mq` — the merge-quality harness CLI.
//!
//! Offline subcommands (`run`, `learn`, `extract-fixtures`) always build; the
//! network ones (`discover`, `fetch`, `all`) require the `steam` feature.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use foch_engine::MergeKernelMode;
use foch_merge_quality::{
	CmdResult, archive, common_probe, config, corpus_shadow, fixtures, lifecycle, orchestrate,
	shadow, symbols,
};

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
		/// Select the provisional oracle cohort or every broad candidate.
		#[arg(long, value_enum, default_value_t = ReportCohortKind::Scorable)]
		cohort: ReportCohortKind,
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
		/// Explicitly run the fallback merge without a vanilla ancestor. Results
		/// from this mode are not a product-quality baseline.
		#[arg(long)]
		no_game_base: bool,
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
		#[arg(long)]
		keep: bool,
		#[arg(long)]
		basegame_root: Option<PathBuf>,
		#[arg(long)]
		no_game_base: bool,
	},
	/// Score one immutable snapshot (internal baseline worker).
	#[command(hide = true)]
	MeasureOne {
		#[arg(long)]
		snapshot_id: String,
		#[arg(long)]
		output_dir: PathBuf,
		#[arg(long)]
		basegame_root: PathBuf,
		#[arg(long)]
		base_snapshot_identity: String,
	},
	/// Compare two files with the corpus scorer's semantic AST policy.
	SemanticDiff {
		#[arg(long)]
		relative_path: String,
		#[arg(long)]
		left: PathBuf,
		#[arg(long)]
		right: PathBuf,
		/// Compare sibling statements as an unordered multiset.
		#[arg(long)]
		ignore_order: bool,
	},
	/// Run legacy and structured merge kernels in isolated child processes.
	ShadowCompare {
		#[arg(long)]
		playset: PathBuf,
		#[arg(long)]
		out_dir: PathBuf,
		#[arg(long = "retained-path", required = true)]
		retained_paths: Vec<String>,
		#[arg(long)]
		base_snapshot_identity: Option<String>,
		#[arg(long, default_value_t = 600)]
		timeout_secs: u64,
		#[arg(long)]
		force: bool,
	},
	/// Compare both kernels for one immutable corpus scoring unit.
	ShadowCase {
		#[arg(long = "id")]
		id: String,
		#[arg(long)]
		retained_path: String,
		#[arg(long)]
		out_dir: PathBuf,
		#[arg(long, default_value_t = 600)]
		timeout_secs: u64,
		#[arg(long)]
		force: bool,
		#[arg(long)]
		record: bool,
	},
	/// Project selected Structured candidates over a fixed Legacy corpus baseline.
	ShadowCorpus {
		#[arg(long)]
		out_dir: PathBuf,
		/// Canonical per-file Legacy scores frozen from the base-aware fixture.
		#[arg(
			long,
			default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-baseline.json")
		)]
		legacy_baseline: PathBuf,
		/// Committed verdict counts used to validate the per-file baseline.
		#[arg(
			long,
			default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/expected.json")
		)]
		expected_verdicts: PathBuf,
		/// Structured candidate to execute, as CASE_ID:RELATIVE_PATH. Repeatable.
		#[arg(long, required = true)]
		candidate: Vec<corpus_shadow::CorpusShadowSelection>,
		#[arg(long, default_value_t = 600)]
		timeout_secs: u64,
		#[arg(long)]
		expect_multi_source_units: Option<usize>,
		#[arg(long)]
		force: bool,
		#[arg(long)]
		record: bool,
	},
	/// Test the provisional common/<folder> module boundary with Structured.
	CommonProbe {
		#[arg(long)]
		out_dir: PathBuf,
		/// Fixed per-unit corpus denominator and Legacy evidence.
		#[arg(
			long,
			default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-baseline.json")
		)]
		legacy_baseline: PathBuf,
	},
	/// Execute one isolated shadow-comparison arm.
	#[command(hide = true)]
	ShadowRunOne {
		#[arg(long)]
		input_manifest: PathBuf,
		#[arg(long)]
		out_dir: PathBuf,
		#[arg(long, value_enum)]
		kernel: ShadowKernelKind,
	},
	/// Extract full local cases and pack the committed corpus archives.
	ExtractFixtures {
		/// Workshop/corpus archive (gzip-compressed tar).
		#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/corpus.tar.gz"))]
		out: PathBuf,
		/// Full vanilla text archive (gzip-compressed tar).
		#[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/basegame-text.tar.gz"))]
		basegame_out: PathBuf,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ReportCohortKind {
	Scorable,
	AllCandidates,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ShadowKernelKind {
	Legacy,
	Structured,
}

impl From<ShadowKernelKind> for MergeKernelMode {
	fn from(value: ShadowKernelKind) -> Self {
		match value {
			ShadowKernelKind::Legacy => Self::Legacy,
			ShadowKernelKind::Structured => Self::Structured,
		}
	}
}

impl From<ReportCohortKind> for lifecycle::ReportCohort {
	fn from(value: ReportCohortKind) -> Self {
		match value {
			ReportCohortKind::Scorable => Self::Scorable,
			ReportCohortKind::AllCandidates => Self::AllCandidates,
		}
	}
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
				basegame_root: &game.game_root,
			})?;
			eprintln!(
				"[measure] selected={} cached={} measured={} failed={}",
				summary.selected, summary.cached, summary.measured, summary.failed
			);
			Ok(())
		}
		Cmd::Report {
			cohort,
			executable_hash,
			config_hash,
			limit,
		} => lifecycle::report(&lifecycle::ReportOptions {
			dataset_root: &dataset_root,
			output_dir: &results_dir,
			executable_hash: executable_hash.as_deref(),
			config_hash: config_hash.as_deref(),
			cohort: cohort.into(),
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
				basegame_root: &discovery.game_root,
			})?;
			lifecycle::report(&lifecycle::ReportOptions {
				dataset_root: &dataset_root,
				output_dir: &results_dir,
				executable_hash: Some(&measured.executable_hash),
				config_hash: Some(&measured.config_hash),
				cohort: lifecycle::ReportCohort::Scorable,
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
		Cmd::Run {
			limit,
			keep,
			no_game_base,
		} => {
			let workshop = legacy_workshop(&game_root, &workshop_dir, &steam_root)?;
			let discovered_game = if no_game_base {
				None
			} else {
				Some(discover_game(&game_root, &steam_root)?)
			};
			let base_game = discovered_game
				.as_ref()
				.map_or(orchestrate::BaseGameMode::ExplicitlyDisabled, |game| {
					orchestrate::BaseGameMode::Path(&game.game_root)
				});
			orchestrate::run(&orchestrate::RunOptions {
				corpus: &corpus,
				workshop_dir: &workshop,
				results_dir: &results_dir,
				base_game,
				limit,
				keep,
				isolate: true,
			})
		}
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
		Cmd::ScoreOne {
			id,
			keep,
			basegame_root,
			no_game_base,
		} => {
			let base_game = match (basegame_root.as_deref(), no_game_base) {
				(Some(path), false) => orchestrate::BaseGameMode::Path(path),
				(None, true) => orchestrate::BaseGameMode::ExplicitlyDisabled,
				(Some(_), true) => {
					return Err("--basegame-root conflicts with --no-game-base".into());
				}
				(None, false) => {
					return Err(
						"score-one requires --basegame-root unless --no-game-base is explicit"
							.into(),
					);
				}
			};
			orchestrate::score_one(
				&corpus,
				&legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
				base_game,
				&id,
				keep,
			)
		}
		Cmd::MeasureOne {
			snapshot_id,
			output_dir,
			basegame_root,
			base_snapshot_identity,
		} => lifecycle::measure_one(
			&dataset_root,
			&snapshot_id,
			&output_dir,
			&basegame_root,
			&base_snapshot_identity,
		),
		Cmd::SemanticDiff {
			relative_path,
			left,
			right,
			ignore_order,
		} => {
			let diff = foch_merge_quality::score::semantic_atom_diff(
				&relative_path,
				&left,
				&right,
				ignore_order,
			)
			.ok_or("semantic comparison unavailable for one or both files")?;
			println!("{}", serde_json::to_string_pretty(&diff)?);
			Ok(())
		}
		Cmd::ShadowCompare {
			playset,
			out_dir,
			retained_paths,
			base_snapshot_identity,
			timeout_secs,
			force,
		} => {
			let game = discover_game(&game_root, &steam_root)?;
			let report = shadow::run_shadow_comparison(shadow::ShadowCompareRequest {
				playset: &playset,
				output_dir: &out_dir,
				game_root: &game.game_root,
				game_version: &game.game_version,
				retained_paths: retained_paths.into_iter().collect::<BTreeSet<_>>(),
				expected_base_snapshot_identity: base_snapshot_identity.as_deref(),
				force,
				executable: &std::env::current_exe()?,
				timeout: Duration::from_secs(timeout_secs),
			})?;
			println!("{}", serde_json::to_string_pretty(&report)?);
			Ok(())
		}
		Cmd::ShadowCase {
			id,
			retained_path,
			out_dir,
			timeout_secs,
			force,
			record,
		} => {
			let game = discover_game(&game_root, &steam_root)?;
			let executable = std::env::current_exe()?;
			let result = corpus_shadow::run_case(
				&corpus_shadow::CorpusShadowOptions {
					dataset_root: &dataset_root,
					output_dir: &out_dir,
					game: &game,
					executable: &executable,
					timeout: Duration::from_secs(timeout_secs),
					force,
					record,
				},
				&id,
				&retained_path,
			)?;
			println!("{}", serde_json::to_string_pretty(&result)?);
			Ok(())
		}
		Cmd::ShadowCorpus {
			out_dir,
			legacy_baseline,
			expected_verdicts,
			candidate,
			timeout_secs,
			expect_multi_source_units,
			force,
			record,
		} => {
			let game = discover_game(&game_root, &steam_root)?;
			let executable = std::env::current_exe()?;
			let candidates = candidate.into_iter().collect::<BTreeSet<_>>();
			let report = corpus_shadow::run_corpus(
				&corpus_shadow::CorpusShadowCorpusOptions {
					shadow: corpus_shadow::CorpusShadowOptions {
						dataset_root: &dataset_root,
						output_dir: &out_dir,
						game: &game,
						executable: &executable,
						timeout: Duration::from_secs(timeout_secs),
						force,
						record,
					},
					legacy_baseline: &legacy_baseline,
					expected_verdicts: &expected_verdicts,
					candidates: &candidates,
				},
				expect_multi_source_units,
			)?;
			println!("{}", serde_json::to_string_pretty(&report.summary)?);
			Ok(())
		}
		Cmd::CommonProbe {
			out_dir,
			legacy_baseline,
		} => {
			let game = discover_game(&game_root, &steam_root)?;
			let report = common_probe::run_common_applicability_probe(
				&common_probe::CommonApplicabilityOptions {
					dataset_root: &dataset_root,
					output_dir: &out_dir,
					legacy_baseline: &legacy_baseline,
					game: &game,
				},
			)?;
			println!("{}", serde_json::to_string_pretty(&report.summary)?);
			Ok(())
		}
		Cmd::ShadowRunOne {
			input_manifest,
			out_dir,
			kernel,
		} => {
			let manifest = serde_json::from_slice(&fs::read(input_manifest)?)?;
			let executable = std::env::current_exe()?;
			let record = shadow::run_shadow_arm(shadow::ShadowRunRequest {
				manifest: &manifest,
				output_dir: &out_dir,
				executable: &executable,
				kernel: kernel.into(),
			});
			println!("{}", serde_json::to_string(&record)?);
			Ok(())
		}
		Cmd::ExtractFixtures {
			out,
			basegame_out,
			ids,
		} => {
			// Keep the already-near-limit Workshop archive separate from the
			// version-bound vanilla text snapshot.
			let staging = tempfile::tempdir()?;
			let game = discover_game(&game_root, &steam_root)?;
			let workshop = legacy_workshop(&game_root, &workshop_dir, &steam_root)?;
			fixtures::extract(&corpus, &workshop, &game.game_root, staging.path(), &ids)?;
			let basegame_staging = tempfile::tempdir()?;
			fs::rename(
				staging.path().join("basegame"),
				basegame_staging.path().join("basegame"),
			)?;
			fs::rename(
				staging.path().join("basegame-manifest.json"),
				basegame_staging.path().join("basegame-manifest.json"),
			)?;
			archive::pack_dir(staging.path(), &out)?;
			archive::pack_dir(basegame_staging.path(), &basegame_out)?;
			let manifest_out = basegame_out
				.parent()
				.unwrap_or_else(|| std::path::Path::new("."))
				.join("basegame-manifest.json");
			fs::copy(
				basegame_staging.path().join("basegame-manifest.json"),
				&manifest_out,
			)?;
			eprintln!("[extract] packed corpus -> {}", out.display());
			eprintln!(
				"[extract] packed vanilla text -> {}",
				basegame_out.display()
			);
			eprintln!("[extract] wrote manifest -> {}", manifest_out.display());
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
			let game = discover_game(&game_root, &steam_root)?;
			orchestrate::run(&orchestrate::RunOptions {
				corpus: &corpus,
				workshop_dir: &legacy_workshop(&game_root, &workshop_dir, &steam_root)?,
				results_dir: &results_dir,
				base_game: orchestrate::BaseGameMode::Path(&game.game_root),
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
