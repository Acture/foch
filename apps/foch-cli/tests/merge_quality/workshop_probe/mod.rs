//! Automatic, resumable discovery probes. Separate from the fixed acceptance cohort.
mod account;
mod process;
mod selection;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use foch::game::eu4::base::snapshot::{self, InstalledBaseDataMetadata};
use foch::model::{
	MERGE_REPORT_ARTIFACT_PATH, MergeReport, MergeReportBaseSnapshot, ProductInputManifest,
	ProductInputMod,
};
use foch::playset::descriptor::load_descriptor;
use foch::playset::steam::{SteamWorkshopCatalog, SteamWorkshopError};
use foch::project::{Project, ProjectConfig, ProjectMod};
use serde::{Deserialize, Serialize};

use crate::merge_quality::config::{
	DiscoveryOverrides, Eu4GameDiscovery, WorkshopCatalog, WorkshopItemVersion, discover_eu4_game,
};
use crate::merge_quality::lifecycle::executable_hash;
use selection::{PAGE_URL, Selection};

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;
const TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Deserialize)]
struct LibraryPair {
	content_root: PathBuf,
	manifest_path: PathBuf,
}

struct ProbeConfig {
	root: PathBuf,
	url: String,
	count: usize,
	game: Eu4GameDiscovery,
	libraries: Vec<LibraryPair>,
	base_data: PathBuf,
	steamcmd: PathBuf,
	steam_account: Option<String>,
}

#[derive(Serialize)]
struct UnavailableInput {
	id: String,
	reason: String,
}

#[derive(Serialize)]
struct ProbeReport {
	status: String,
	stage: String,
	error: Option<String>,
	selection: PathBuf,
	executable_blake3: String,
	game_version: String,
	steam_build_id: Option<u64>,
	input: Option<ProductInputManifest>,
	unavailable: Vec<UnavailableInput>,
	process: Option<process::ProcessResult>,
	merge_report: Option<PathBuf>,
	semantic_correctness: &'static str,
}

#[test]
#[ignore = "network, SteamCMD downloads and real merges; run cargo workshop-probe"]
fn workshop_recent_page() {
	crate::require_acceptance("workshop-recent-probe");
	let root: PathBuf = std::env::var_os("FOCH_WORKSHOP_PROBE_DIR")
		.map(PathBuf::from)
		.unwrap_or_else(|| {
			PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/workshop-probe")
		});
	fs::create_dir_all(&root).expect("create probe directory");
	let root = root.canonicalize().unwrap();
	// Discovery failures are durable too, before any Workshop or merge work.
	let config = live_config(root.clone()).unwrap_or_else(|error| {
		fs::write(
			root.join("discovery-error.json"),
			serde_json::to_vec_pretty(
				&serde_json::json!({"stage":"game_discovery", "error":error.to_string()}),
			)
			.unwrap(),
		)
		.unwrap();
		panic!("{error}");
	});
	let outcome = execute(&config, download_missing);
	assert!(outcome.is_ok(), "probe failed: {}", outcome.unwrap_err());
}

fn live_config(root: PathBuf) -> ProbeResult<ProbeConfig> {
	let game = discover_eu4_game(&DiscoveryOverrides::default())?;
	let libraries: Vec<LibraryPair> = match game.steam_root.as_ref() {
		Some(steam) => match SteamWorkshopCatalog::discover_from_steam_root(steam, 236850) {
			Ok(catalog) => catalog
				.libraries()
				.iter()
				.map(|library| LibraryPair {
					content_root: library.content_root.clone(),
					manifest_path: library.manifest_path.clone(),
				})
				.collect(),
			Err(SteamWorkshopError::NoWorkshopLibraries { .. }) => Vec::new(),
			Err(error) => return Err(error.into()),
		},
		None => Vec::new(),
	};
	Ok(ProbeConfig {
		root,
		url: PAGE_URL.into(),
		count: 30,
		game,
		libraries,
		base_data: snapshot::data_root(),
		steamcmd: std::env::var_os("FOCH_STEAMCMD")
			.map(PathBuf::from)
			.unwrap_or_else(|| "steamcmd".into()),
		steam_account: std::env::var("FOCH_STEAM_ACCOUNT").ok(),
	})
}

fn execute(
	config: &ProbeConfig,
	download: impl FnOnce(&ProbeConfig, &Path, &[String]) -> ProbeResult<Vec<LibraryPair>>,
) -> ProbeResult<PathBuf> {
	fs::create_dir_all(config.root.join("runs"))?;
	let lock = fs::OpenOptions::new()
		.create(true)
		.truncate(false)
		.read(true)
		.write(true)
		.open(config.root.join("probe.lock"))?;
	lock.try_lock().map_err(|error| {
		format!("probe directory is already in use or cannot be locked: {error}")
	})?;
	let run: PathBuf = tempfile::Builder::new()
		.prefix("run-")
		.tempdir_in(config.root.join("runs"))?
		.keep();
	eprintln!("[workshop-probe] artifacts: {}", run.display());
	let mut report = ProbeReport {
		status: "running".into(),
		stage: "selection".into(),
		error: None,
		selection: config.root.join("selection.json"),
		executable_blake3: executable_hash(Path::new(env!("CARGO_BIN_EXE_foch")))?,
		game_version: config.game.game_version.clone(),
		steam_build_id: config.game.steam_build_id,
		input: None,
		unavailable: Vec::new(),
		process: None,
		merge_report: None,
		semantic_correctness: "not_scored; diagnostics are candidates for investigation, not proof of compatibility",
	};
	let result = workflow(config, &run, &mut report, download);
	match &result {
		Ok(()) => report.status = "completed".into(),
		Err(error) => {
			report.status = "failed".into();
			report.error = Some(error.to_string());
		}
	}
	write_json(&run.join("report.json"), &report)?;
	eprintln!(
		"[workshop-probe] {}: {}",
		report.status,
		run.join("report.json").display()
	);
	result?;
	Ok(run)
}

fn workflow(
	config: &ProbeConfig,
	run: &Path,
	report: &mut ProbeReport,
	download: impl FnOnce(&ProbeConfig, &Path, &[String]) -> ProbeResult<Vec<LibraryPair>>,
) -> ProbeResult<()> {
	let selection: Selection = selection::load_or_fetch(&config.root, &config.url, config.count)?;
	// Each attempt retains exactly the selected input, independent of later runs.
	write_json(&run.join("selection.json"), &selection)?;
	report.stage = "input_preparation".into();
	let locations = config.root.join("download-locations.json");
	let mut pairs: Vec<LibraryPair> = config.libraries.clone();
	if locations.exists() {
		pairs.extend(serde_json::from_slice::<Vec<LibraryPair>>(&fs::read(
			&locations,
		)?)?);
	}
	pairs = normalize_pairs(pairs)?;
	let (_, missing) = resolve_selection(&selection, &pairs)?;
	report.unavailable = missing;
	write_json(&run.join("report.json"), report)?;
	if !report.unavailable.is_empty() {
		let ids: Vec<String> = report
			.unavailable
			.iter()
			.map(|item| item.id.clone())
			.collect();
		eprintln!(
			"[workshop-probe] preparing {} of {} selected items",
			ids.len(),
			selection.items.len()
		);
		report.stage = "download".into();
		let downloaded: Vec<LibraryPair> = download(config, run, &ids)?;
		pairs.extend(downloaded);
		pairs = normalize_pairs(pairs)?;
		write_json(&locations, &pairs)?;
	}
	report.stage = "input_validation".into();
	let (sources, missing) = resolve_selection(&selection, &pairs)?;
	report.unavailable = missing;
	if !report.unavailable.is_empty() {
		return Err("selected Workshop inputs remain unavailable; merge was not started".into());
	}
	let input = ProductInputManifest::new(
		sources
			.iter()
			.enumerate()
			.map(|(position, source)| ProductInputMod {
				mod_id: source.identity.workshop_id.to_string(),
				precedence: position + 1,
				workshop_identity: source.identity.clone(),
			})
			.collect(),
	);
	report.input = Some(input.clone());
	write_json(&run.join("input.json"), &input)?;
	let descriptors: Vec<serde_json::Value> = sources
		.iter()
		.map(|source| {
			let descriptor = load_descriptor(&source.content_path.join("descriptor.mod"))?;
			Ok(
				serde_json::json!({"id": source.identity.workshop_id, "name": descriptor.name,
			"dependencies": descriptor.dependencies, "replace_path": descriptor.replace_path,
			"supported_version": descriptor.supported_version}),
			)
		})
		.collect::<ProbeResult<_>>()?;
	write_json(&run.join("descriptors.json"), &descriptors)?;
	let project = Project {
		project: Some(ProjectConfig {
			game: Some(foch::game::eu4::Eu4),
			game_path: Some(config.game.game_root.clone()),
			paradox_data_path: Some(run.join("launcher")),
			mods: sources
				.iter()
				.enumerate()
				.map(|(position, source)| ProjectMod {
					id: Some(source.identity.workshop_id.to_string()),
					steam_id: Some(source.identity.workshop_id.to_string()),
					path: Some(source.content_path.clone()),
					workshop_identity: Some(source.identity.clone()),
					enabled: true,
					position: Some(position),
				})
				.collect(),
			..ProjectConfig::default()
		}),
		..Project::default()
	};
	fs::write(run.join("foch.toml"), toml::to_string_pretty(&project)?)?;
	report.stage = "base_preparation".into();
	ensure_base(config, run)?;
	report.stage = "merge".into();
	let mut command = foch_command(config, run);
	command
		.arg("merge")
		.arg(run.join("foch.toml"))
		.arg("--out")
		.arg(run.join("merged"))
		.args(["--confirm", "--non-interactive", "--provenance"]);
	let result = process::run(&mut command, run, "merge", TIMEOUT)?;
	let failure = result.failure("merge", run);
	report.process = Some(result);
	if let Some(error) = failure {
		return Err(error.into());
	}
	report.stage = "output_validation".into();
	let report_path = run.join("merged").join(MERGE_REPORT_ARTIFACT_PATH);
	let merged: MergeReport = serde_json::from_slice(&fs::read(&report_path)?)?;
	report.merge_report = Some(report_path);
	if merged.input.as_ref() != Some(&input.attestation()) {
		return Err("merge output did not attest the complete ordered input".into());
	}
	if !matches!(
		merged
			.execution
			.as_ref()
			.map(|execution| &execution.base_snapshot),
		Some(MergeReportBaseSnapshot::Resolved { .. })
	) {
		return Err("merge output did not attest an analyzed game base".into());
	}
	write_json(
		&run.join("gaps.json"),
		&serde_json::json!({
			"deferred_units": merged.conflict_resolutions,
			"validation": merged.validation,
			"dependency_findings": merged.dep_misuse,
			"version_mismatches": merged.version_mismatch,
			"note": "Review reasons and emitted source provenance; this is not a semantic correctness score."
		}),
	)?;
	report.stage = "input_revalidation".into();
	let (after, missing) = resolve_selection(&selection, &pairs)?;
	if !missing.is_empty() || after != sources {
		return Err("Workshop input identity changed during the probe".into());
	}
	if executable_hash(Path::new(env!("CARGO_BIN_EXE_foch")))? != report.executable_blake3 {
		return Err("CLI artifact changed during the probe".into());
	}
	if snapshot::detect_game_version(&config.game.game_root).as_deref()
		!= Some(&config.game.game_version)
	{
		return Err("game version changed during the probe".into());
	}
	if config.game.steam_build_id.is_some() {
		let after = discover_eu4_game(&DiscoveryOverrides {
			game_root: Some(config.game.game_root.clone()),
			steam_root: config.game.steam_root.clone(),
			..DiscoveryOverrides::default()
		})?;
		if after.steam_build_id != config.game.steam_build_id {
			return Err("Steam game build changed during the probe".into());
		}
	}
	report.stage = "output_validation".into();
	if merged.engine_failure_count > 0
		|| merged.validation.fatal_errors > 0
		|| merged.validation.parse_errors > 0
	{
		return Err("merge produced engine failures or invalid output; inspect gaps.json".into());
	}
	report.stage = "complete".into();
	Ok(())
}

fn normalize_pairs(pairs: Vec<LibraryPair>) -> ProbeResult<Vec<LibraryPair>> {
	pairs
		.into_iter()
		.map(|pair| {
			Ok(LibraryPair {
				content_root: pair.content_root.canonicalize()?,
				manifest_path: pair.manifest_path.canonicalize()?,
			})
		})
		.collect::<ProbeResult<BTreeSet<_>>>()
		.map(|pairs| pairs.into_iter().collect())
}

fn resolve_selection(
	selection: &Selection,
	pairs: &[LibraryPair],
) -> ProbeResult<(Vec<WorkshopItemVersion>, Vec<UnavailableInput>)> {
	let catalog = if pairs.is_empty() {
		None
	} else {
		Some(WorkshopCatalog::new(
			SteamWorkshopCatalog::from_library_paths(
				236850,
				pairs
					.iter()
					.map(|pair| (pair.content_root.clone(), pair.manifest_path.clone())),
			)?,
		))
	};
	let mut sources = Vec::new();
	let mut missing = Vec::new();
	for item in &selection.items {
		match catalog
			.as_ref()
			.ok_or_else(|| "no installed Workshop catalog".to_string())
			.and_then(|catalog| catalog.require_item(&item.publishedfileid))
			.and_then(|source| {
				load_descriptor(&source.content_path.join("descriptor.mod"))
					.map(|_| source)
					.map_err(|error| error.to_string())
			}) {
			Ok(source) => sources.push(source),
			Err(reason) => missing.push(UnavailableInput {
				id: item.publishedfileid.clone(),
				reason,
			}),
		}
	}
	Ok((sources, missing))
}

fn download_missing(
	config: &ProbeConfig,
	run: &Path,
	ids: &[String],
) -> ProbeResult<Vec<LibraryPair>> {
	let account = account::resolve(
		config.steam_account.as_deref(),
		config.game.steam_root.as_deref(),
	)?;
	eprintln!("[workshop-probe] requesting cached Steam login for {account}");
	let mut command = Command::new(&config.steamcmd);
	command
		.args([
			"+@NoPromptForPassword",
			"1",
			"+@ShutdownOnFailedCommand",
			"1",
			"+force_install_dir",
		])
		.arg(config.root.join("steam"))
		.arg("+login")
		.arg(&account);
	for (position, id) in ids.iter().enumerate() {
		eprintln!(
			"[workshop-probe] queued download {}/{}, Workshop {id}",
			position + 1,
			ids.len()
		);
		command.args(["+workshop_download_item", "236850", id]);
	}
	command.arg("+quit");
	let result = process::run(&mut command, run, "download", TIMEOUT)?;
	let text = fs::read_to_string(run.join("download.stdout.log"))?;
	let failures = download_failures(&text, ids)?;
	write_json(
		&run.join("download-result.json"),
		&serde_json::json!({"process": result, "failed_items": failures}),
	)?;
	let pairs = download_locations(&text, ids)?;
	// Persist successful locations even if another item failed or timed out.
	let path = config.root.join("download-locations.json");
	let mut saved: Vec<LibraryPair> = if path.exists() {
		serde_json::from_slice(&fs::read(&path)?)?
	} else {
		Vec::new()
	};
	saved.extend(pairs.clone());
	write_json(&path, &normalize_pairs(saved)?)?;
	if text.contains("Cached credentials not found") {
		return Err(format!(
			"SteamCMD has no cached login for {account}. Log in to SteamCMD interactively once, then rerun cargo workshop-probe; see download.stdout.log"
		).into());
	}
	if result.timed_out || result.code != Some(0) {
		return Err("SteamCMD failed; inspect download logs. If its cached login has expired, log in to SteamCMD interactively and rerun cargo workshop-probe.".into());
	}
	if !failures.is_empty() {
		return Err(format!(
			"SteamCMD reported {} failed items (first: {}); see download-result.json",
			failures.len(),
			failures[0].reason
		)
		.into());
	}
	Ok(pairs)
}

fn download_failures(text: &str, requested: &[String]) -> ProbeResult<Vec<UnavailableInput>> {
	let pattern = regex::Regex::new(r"ERROR!\s+Download item (\d+) failed \(([^)\r\n]+)\)")?;
	Ok(pattern
		.captures_iter(text)
		.filter(|capture| requested.iter().any(|id| id == &capture[1]))
		.map(|capture| UnavailableInput {
			id: capture[1].into(),
			reason: capture[2].into(),
		})
		.collect())
}

fn download_locations(text: &str, requested: &[String]) -> ProbeResult<Vec<LibraryPair>> {
	let pattern = regex::Regex::new(r#"Downloaded item (\d+) to "([^"]+)""#)?;
	let mut pairs = Vec::new();
	for captures in pattern.captures_iter(text) {
		let id = &captures[1];
		if !requested.iter().any(|wanted| wanted == id) {
			continue;
		}
		let path = Path::new(&captures[2]);
		if path.file_name().and_then(|name| name.to_str()) != Some(id) {
			return Err("SteamCMD returned a mismatched item path".into());
		}
		let content = path.parent().ok_or("SteamCMD item has no content root")?;
		let workshop = content
			.parent()
			.and_then(Path::parent)
			.ok_or("SteamCMD item has no Workshop root")?;
		pairs.push(LibraryPair {
			content_root: content.into(),
			manifest_path: workshop.join("appworkshop_236850.acf"),
		});
	}
	Ok(pairs)
}

fn foch_command(config: &ProbeConfig, run: &Path) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_foch"));
	command
		.current_dir(run)
		.env("FOCH_DATA_DIR", &config.base_data)
		.env("FOCH_CACHE_ROOT", config.root.join("cache"))
		.env("FOCH_CONFIG_DIR", run.join("config"));
	command
}

fn ensure_base(config: &ProbeConfig, run: &Path) -> ProbeResult<()> {
	let version = &config.game.game_version;
	if version.is_empty()
		|| !version.chars().any(|c| c.is_ascii_digit())
		|| !version
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
	{
		return Err("invalid EU4 version for installed base lookup".into());
	}
	let dir = config.base_data.join("eu4").join(version);
	let current = if dir.join(snapshot::INSTALLED_METADATA_FILE_NAME).is_file() {
		let metadata: InstalledBaseDataMetadata =
			serde_json::from_slice(&fs::read(dir.join(snapshot::INSTALLED_METADATA_FILE_NAME))?)?;
		metadata.schema_version == snapshot::BASE_DATA_SCHEMA_VERSION
			&& metadata.analysis_rules_version == foch::game::eu4::base::analysis_rules_version()
			&& dir.join(snapshot::INSTALLED_SNAPSHOT_FILE_NAME).is_file()
	} else {
		false
	};
	if !current {
		let result = process::run(
			foch_command(config, run)
				.args(["data", "build", "eu4", "--from-game-path"])
				.arg(&config.game.game_root)
				.args(["--game-version", "auto", "--install", "--output-dir"])
				.arg(run.join("base-build")),
			run,
			"base-build",
			TIMEOUT,
		)?;
		if let Some(failure) = result.failure("base-build", run) {
			return Err(failure.into());
		}
	}
	write_json(
		&run.join("base-metadata.json"),
		&serde_json::from_slice::<serde_json::Value>(&fs::read(
			dir.join(snapshot::INSTALLED_METADATA_FILE_NAME),
		)?)?,
	)
}

fn write_json(path: &Path, value: &impl Serialize) -> ProbeResult<()> {
	let mut temporary =
		tempfile::NamedTempFile::new_in(path.parent().ok_or("JSON artifact has no parent")?)?;
	temporary.write_all(&serde_json::to_vec_pretty(value)?)?;
	temporary.persist(path)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::write_file;

	fn fixture(root: &Path) -> ProbeConfig {
		let root = root.canonicalize().unwrap();
		let game = root.join("game");
		write_file(&game, "version.txt", "v1.37.5.0");
		write_file(
			&game,
			"common/static_modifiers/base.txt",
			"static_shared = { global_tax_modifier = 0.1 }\n",
		);
		write_file(
			&game,
			"common/event_modifiers/base.txt",
			"event_shared = { land_morale = 0.1 }\n",
		);
		let selection = Selection {
			url: PAGE_URL.into(),
			collected_at_unix: 1,
			page_blake3: "0".repeat(64),
			items: ["22", "11"]
				.into_iter()
				.map(|id| selection::Item {
					publishedfileid: id.into(),
					title: format!("fixture {id}"),
					time_updated: 1,
					file_size: "1".into(),
				})
				.collect(),
		};
		write_json(&root.join("selection.json"), &selection).unwrap();
		ProbeConfig {
			game: Eu4GameDiscovery {
				game_root: game,
				game_version: "v1.37.5.0".into(),
				steam_build_id: None,
				steam_root: None,
			},
			base_data: root.join("data"),
			root,
			url: PAGE_URL.into(),
			count: 2,
			libraries: Vec::new(),
			steamcmd: "unused-steamcmd".into(),
			steam_account: None,
		}
	}

	fn install_fixture(config: &ProbeConfig, ids: &[String]) -> ProbeResult<Vec<LibraryPair>> {
		let workshop = config.root.join("steam/steamapps/workshop");
		let content_root = workshop.join("content/236850");
		fs::create_dir_all(&content_root)?;
		let mut entries = String::new();
		for id in ids {
			let root = content_root.join(id);
			write_file(
				&root,
				"descriptor.mod",
				&format!("name = \"fixture {id}\"\nremote_file_id = \"{id}\"\n"),
			);
			let (path, text) = if id == "22" {
				(
					"common/static_modifiers/source.txt",
					"static_shared = { global_tax_modifier = 0.2 }\n",
				)
			} else {
				(
					"common/event_modifiers/source.txt",
					"event_shared = { land_morale = 0.3 }\n",
				)
			};
			write_file(&root, path, text);
			entries.push_str(&format!(
				"\"{id}\" {{ \"manifest\" \"100{id}\" \"size\" \"1\" \"timeupdated\" \"1\" }}\n"
			));
		}
		let manifest_path = workshop.join("appworkshop_236850.acf");
		fs::write(
			&manifest_path,
			format!(
				"\"AppWorkshop\" {{ \"appid\" \"236850\" \"WorkshopItemsInstalled\" {{ {entries} }} }}"
			),
		)?;
		Ok(vec![LibraryPair {
			content_root,
			manifest_path,
		}])
	}

	#[test]
	fn automatic_probe_prepares_base_merges_and_resumes_without_downloading_again() {
		let temp = tempfile::tempdir().unwrap();
		let config = fixture(temp.path());
		let run = execute(&config, |config, _, ids| {
			assert_eq!(ids, ["22", "11"]);
			install_fixture(config, ids)
		})
		.unwrap();
		let report: serde_json::Value =
			serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
		assert_eq!(report["status"], "completed");
		assert_eq!(report["input"]["mods"][0]["mod_id"], "22");
		assert_eq!(report["input"]["mods"][1]["mod_id"], "11");
		assert!(run.join("gaps.json").exists());
		let static_file = "merged/common/static_modifiers/zzz_foch_static_modifiers.txt";
		let event_file = "merged/common/event_modifiers/zzz_foch_event_modifiers.txt";
		assert!(
			fs::read_to_string(run.join(static_file))
				.unwrap()
				.contains("global_tax_modifier = 0.2")
		);
		assert!(
			fs::read_to_string(run.join(event_file))
				.unwrap()
				.contains("land_morale = 0.3")
		);
		let resumed =
			execute(&config, |_, _, _| panic!("installed inputs must be reused")).unwrap();
		assert_ne!(run, resumed);
		assert_eq!(
			fs::read(run.join(static_file)).unwrap(),
			fs::read(resumed.join(static_file)).unwrap()
		);
		assert!(!resumed.join("base-build.stdout.log").exists());
	}

	#[test]
	fn downloader_success_with_missing_items_fails_without_starting_merge() {
		let temp = tempfile::tempdir().unwrap();
		let config = fixture(temp.path());
		let error = execute(&config, |config, _, _| {
			install_fixture(config, &["22".into()])
		})
		.unwrap_err();
		assert!(error.to_string().contains("remain unavailable"));
		let run = fs::read_dir(config.root.join("runs"))
			.unwrap()
			.next()
			.unwrap()
			.unwrap()
			.path();
		let report: serde_json::Value =
			serde_json::from_slice(&fs::read(run.join("report.json")).unwrap()).unwrap();
		assert_eq!(report["status"], "failed");
		assert_eq!(report["unavailable"][0]["id"], "11");
		assert!(!run.join("merged").exists());
		assert!(!run.join("merge.stdout.log").exists());
		assert!(!run.join("base-build.stdout.log").exists());
	}

	#[test]
	fn semantic_deferrals_are_reported_with_paths_and_reasons() {
		let temp = tempfile::tempdir().unwrap();
		let config = fixture(temp.path());
		let run = execute(&config, |config, _, ids| {
			let pairs = install_fixture(config, ids)?;
			write_file(
				&pairs[0].content_root.join("22"),
				"common/static_modifiers/clash.txt",
				"cross_directory_clash = { global_tax_modifier = 0.4 }\n",
			);
			write_file(
				&pairs[0].content_root.join("11"),
				"common/event_modifiers/clash.txt",
				"cross_directory_clash = { land_morale = 0.5 }\n",
			);
			Ok(pairs)
		})
		.unwrap();
		let gaps: serde_json::Value =
			serde_json::from_slice(&fs::read(run.join("gaps.json")).unwrap()).unwrap();
		assert_eq!(
			gaps["deferred_units"][0]["deferred_reason"],
			"unsupported_input"
		);
		assert!(
			!gaps["deferred_units"][0]["reason"]
				.as_str()
				.unwrap()
				.is_empty()
		);
		assert!(
			!gaps["deferred_units"][0]["path"]
				.as_str()
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn steamcmd_paths_are_bound_to_requested_item_and_same_library_acf() {
		let pairs = download_locations(
			"Success. Downloaded item 22 to \"/a library/steamapps/workshop/content/236850/22\" (12 bytes)",
			&["22".into()],
		)
		.unwrap();
		assert_eq!(
			pairs[0].manifest_path,
			Path::new("/a library/steamapps/workshop/appworkshop_236850.acf")
		);
		assert!(download_locations("Downloaded item 22 to \"/foo/23\"", &["22".into()]).is_err());
		assert!(
			download_locations("ERROR: Not logged on", &["22".into()])
				.unwrap()
				.is_empty()
		);
		let failures = download_failures("ERROR! Download item 22 failed (No Connection).Downloading item 11 ...\nERROR! Download item 11 failed (No Connection).", &["22".into(), "11".into()]).unwrap();
		assert_eq!(failures.len(), 2);
		assert_eq!(failures[0].reason, "No Connection");
	}
}
