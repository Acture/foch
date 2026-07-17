use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use foch_core::config::FochConfig;
use foch_core::model::{MergeReport, MergeReportStatus};
use foch_engine::{
	CheckRequest, Config, FileFilter, MergeKernelMode, installed_base_snapshot_identity,
	load_installed_base_snapshot, resolve_workspace_summary,
};
use foch_language::analyzer::content_family::{
	ContentLoadPolicy, GameProfile, module_name_for_descriptor,
};
use foch_language::analyzer::eu4_profile::eu4_profile;
use serde::{Deserialize, Serialize};

use crate::score::run_merge_with_kernel;

pub const SHADOW_COMPARE_SCHEMA: &str = "2.0.0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowFileIdentity {
	pub relative_path: String,
	pub content_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowModIdentity {
	pub mod_id: String,
	pub root_path: Option<PathBuf>,
	pub content_hash: Option<String>,
	pub descriptor_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowComparisonInputs {
	pub playset: PathBuf,
	pub playset_hash: String,
	pub launcher_descriptors: Vec<ShadowFileIdentity>,
	pub mods: Vec<ShadowModIdentity>,
	pub game_root: PathBuf,
	pub game_version: String,
	pub base_snapshot_identity: String,
	pub base_files: Vec<ShadowFileIdentity>,
	pub foch_config_hash: String,
	pub resolution_files: Vec<ShadowFileIdentity>,
	pub executable: PathBuf,
	pub executable_hash: String,
	pub retained_paths: Vec<String>,
	pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowInputManifest {
	pub schema: String,
	pub comparison_id: String,
	pub inputs: ShadowComparisonInputs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDiagnosticKind {
	Error,
	Fatal,
	Warning,
	Conflict,
	HandlerResolution,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowDiagnostic {
	pub kind: ShadowDiagnosticKind,
	pub path: Option<String>,
	pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowRunRecord {
	pub schema: String,
	pub comparison_id: String,
	pub kernel: String,
	pub output_dir: PathBuf,
	pub output_valid: bool,
	pub elapsed_ms: u64,
	pub status: String,
	pub exit_code: Option<i32>,
	pub manual_conflict_count: Option<usize>,
	pub handler_resolution_count: Option<usize>,
	pub generated_file_count: Option<usize>,
	pub fatal_reason: Option<String>,
	pub error: Option<String>,
	pub diagnostics: Vec<ShadowDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowFileDelta {
	pub relative_path: String,
	pub legacy_hash: Option<String>,
	pub structured_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShadowComparisonReport {
	pub schema: String,
	pub comparison_id: String,
	pub inputs: ShadowComparisonInputs,
	pub legacy: ShadowRunRecord,
	pub structured: ShadowRunRecord,
	pub outputs_compared: bool,
	pub file_deltas: Vec<ShadowFileDelta>,
}

pub struct ShadowCaptureRequest<'a> {
	pub playset: &'a Path,
	pub game_root: &'a Path,
	pub game_version: &'a str,
	pub retained_paths: &'a BTreeSet<String>,
	pub retained_base_paths: &'a BTreeSet<String>,
	pub base_snapshot_identity: &'a str,
	pub force: bool,
	pub executable: &'a Path,
}

pub struct VerifiedRetainedBaseSnapshot {
	pub identity: String,
	pub retained_paths: BTreeSet<String>,
}

pub struct ShadowRunRequest<'a> {
	pub manifest: &'a ShadowInputManifest,
	pub output_dir: &'a Path,
	pub executable: &'a Path,
	pub kernel: MergeKernelMode,
}

#[derive(Debug, Deserialize)]
struct DlcLoadIdentity {
	#[serde(default)]
	enabled_mods: Vec<String>,
}

pub fn capture_input_manifest(
	request: ShadowCaptureRequest<'_>,
) -> io::Result<ShadowInputManifest> {
	let playset = fs::canonicalize(request.playset)?;
	let game_root = fs::canonicalize(request.game_root)?;
	let executable = fs::canonicalize(request.executable)?;
	let playset_bytes = fs::read(&playset)?;
	let dlc_load = serde_json::from_slice::<DlcLoadIdentity>(&playset_bytes)
		.map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
	let launcher_descriptors = launcher_descriptor_identities(&playset, &dlc_load.enabled_mods)?;
	let base_files = file_identities(&game_root, request.retained_base_paths)?;

	let mut game_path = HashMap::new();
	game_path.insert("eu4".to_string(), game_root.clone());
	let summary = resolve_workspace_summary(&CheckRequest::from_playset_path(
		playset.clone(),
		Config {
			steam_root_path: None,
			paradox_data_path: None,
			game_path,
			extra_ignore_patterns: Vec::new(),
		},
	))
	.map_err(io::Error::other)?;
	let filter = FileFilter::for_game(summary.game);
	let mods = summary
		.mods
		.into_iter()
		.map(|mod_item| {
			let root_path = mod_item
				.root_path
				.as_deref()
				.map(fs::canonicalize)
				.transpose()?;
			let content_hash = root_path
				.as_deref()
				.map(|root| hash_mod_tree(root, &filter))
				.transpose()?;
			Ok(ShadowModIdentity {
				mod_id: mod_item.mod_id,
				root_path,
				content_hash,
				descriptor_error: mod_item.descriptor_error,
			})
		})
		.collect::<io::Result<Vec<_>>>()?;
	let (foch_config_hash, resolution_files) = effective_foch_config_inputs(&playset)?;

	let inputs = ShadowComparisonInputs {
		playset,
		playset_hash: blake3::hash(&playset_bytes).to_hex().to_string(),
		launcher_descriptors,
		mods,
		game_root,
		game_version: request.game_version.to_string(),
		base_snapshot_identity: request.base_snapshot_identity.to_string(),
		base_files,
		foch_config_hash,
		resolution_files,
		executable_hash: digest_file(&executable)?,
		executable,
		retained_paths: request.retained_paths.iter().cloned().collect(),
		force: request.force,
	};
	let comparison_id = comparison_id_for_inputs(&inputs)?;
	Ok(ShadowInputManifest {
		schema: SHADOW_COMPARE_SCHEMA.to_string(),
		comparison_id,
		inputs,
	})
}

pub fn verified_retained_base_snapshot(
	game_version: &str,
	expected: Option<&str>,
	requested_paths: &BTreeSet<String>,
) -> io::Result<VerifiedRetainedBaseSnapshot> {
	let identity = installed_base_snapshot_identity("eu4", game_version)
		.map_err(io::Error::other)?
		.ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::NotFound,
				format!("missing installed base snapshot for eu4 {game_version}"),
			)
		})?;
	let actual = identity.as_label();
	if let Some(expected) = expected
		&& expected != actual
	{
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			format!(
				"installed base snapshot identity mismatch: expected {expected}, found {actual}"
			),
		));
	}
	let snapshot = load_installed_base_snapshot("eu4", game_version, Some(&identity))
		.map_err(io::Error::other)?
		.ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::NotFound,
				format!("missing installed base snapshot for eu4 {game_version}"),
			)
		})?;
	Ok(VerifiedRetainedBaseSnapshot {
		identity: actual,
		retained_paths: select_retained_base_paths(
			requested_paths,
			&snapshot.snapshot.inventory_paths,
		),
	})
}

pub fn run_shadow_arm(request: ShadowRunRequest<'_>) -> ShadowRunRecord {
	let started = Instant::now();
	if let Err(error) = reset_output_dir(request.output_dir) {
		return error_record(
			request.manifest,
			request.output_dir,
			request.kernel,
			started,
			format!("failed to clear shadow output: {error}"),
		);
	}
	if let Err(error) = verify_manifest_inputs(request.manifest, request.executable) {
		return error_record(
			request.manifest,
			request.output_dir,
			request.kernel,
			started,
			error.to_string(),
		);
	}

	let inputs = &request.manifest.inputs;
	let retained_paths = inputs.retained_paths.iter().cloned().collect();
	let result = run_merge_with_kernel(
		&inputs.playset,
		request.output_dir,
		Some(&inputs.game_root),
		inputs.force,
		Some(retained_paths),
		Some(&inputs.base_snapshot_identity),
		request.kernel,
	);
	let mut record = match result {
		Ok(result) => {
			let output_valid = report_output_valid(result.report.status);
			ShadowRunRecord {
				schema: SHADOW_COMPARE_SCHEMA.to_string(),
				comparison_id: request.manifest.comparison_id.clone(),
				kernel: request.kernel.as_str().to_string(),
				output_dir: request.output_dir.to_path_buf(),
				output_valid,
				elapsed_ms: elapsed_ms(started),
				status: report_status_name(result.report.status).to_string(),
				exit_code: Some(result.exit_code),
				manual_conflict_count: Some(result.merge_status.manual_conflict_count),
				handler_resolution_count: Some(result.merge_status.handler_resolution_count),
				generated_file_count: Some(result.merge_status.generated_file_count),
				fatal_reason: result.report.fatal_reason.clone(),
				error: None,
				diagnostics: report_diagnostics(&result.report),
			}
		}
		Err(error) => error_record(
			request.manifest,
			request.output_dir,
			request.kernel,
			started,
			error.to_string(),
		),
	};

	if !record.output_valid
		&& let Err(error) = reset_output_dir(request.output_dir)
	{
		record.diagnostics.push(ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Error,
			path: Some(request.output_dir.display().to_string()),
			message: format!("failed to clear failed shadow output: {error}"),
		});
	}
	if let Err(error) = verify_manifest_inputs(request.manifest, request.executable) {
		let message = format!("shadow inputs changed while arm was running: {error}");
		if let Err(cleanup_error) = reset_output_dir(request.output_dir) {
			record.diagnostics.push(ShadowDiagnostic {
				kind: ShadowDiagnosticKind::Error,
				path: Some(request.output_dir.display().to_string()),
				message: format!("failed to clear invalid shadow output: {cleanup_error}"),
			});
		}
		record.output_valid = false;
		record.status = "error".to_string();
		record.exit_code = None;
		record.error = Some(message.clone());
		record.diagnostics.push(ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Error,
			path: None,
			message,
		});
	}
	record.elapsed_ms = elapsed_ms(started);
	record
}

pub fn reset_output_dir(path: &Path) -> io::Result<()> {
	match fs::symlink_metadata(path) {
		Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
			fs::remove_dir_all(path)
		}
		Ok(_) => fs::remove_file(path),
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
		Err(error) => Err(error),
	}
}

pub fn build_comparison_report(
	manifest: ShadowInputManifest,
	legacy: ShadowRunRecord,
	structured: ShadowRunRecord,
) -> io::Result<ShadowComparisonReport> {
	validate_manifest(&manifest)?;
	for record in [&legacy, &structured] {
		if record.schema != SHADOW_COMPARE_SCHEMA || record.comparison_id != manifest.comparison_id
		{
			return Err(io::Error::new(
				io::ErrorKind::InvalidData,
				"shadow run records do not belong to this comparison",
			));
		}
	}
	let outputs_compared = legacy.output_valid && structured.output_valid;
	let file_deltas = if outputs_compared {
		if !legacy.output_dir.is_dir() || !structured.output_dir.is_dir() {
			return Err(io::Error::new(
				io::ErrorKind::NotFound,
				"a valid shadow arm is missing its output directory",
			));
		}
		diff_output_dirs(&legacy.output_dir, &structured.output_dir)?
	} else {
		Vec::new()
	};
	Ok(ShadowComparisonReport {
		schema: SHADOW_COMPARE_SCHEMA.to_string(),
		comparison_id: manifest.comparison_id,
		inputs: manifest.inputs,
		legacy,
		structured,
		outputs_compared,
		file_deltas,
	})
}

pub fn diff_output_dirs(legacy: &Path, structured: &Path) -> io::Result<Vec<ShadowFileDelta>> {
	let legacy_files = output_hashes(legacy)?;
	let structured_files = output_hashes(structured)?;
	let paths = legacy_files
		.keys()
		.chain(structured_files.keys())
		.cloned()
		.collect::<BTreeSet<_>>();
	Ok(paths
		.into_iter()
		.filter_map(|relative_path| {
			let legacy_hash = legacy_files.get(&relative_path).cloned();
			let structured_hash = structured_files.get(&relative_path).cloned();
			(legacy_hash != structured_hash).then_some(ShadowFileDelta {
				relative_path,
				legacy_hash,
				structured_hash,
			})
		})
		.collect())
}

fn verify_manifest_inputs(manifest: &ShadowInputManifest, executable: &Path) -> io::Result<()> {
	validate_manifest(manifest)?;
	let retained_paths = manifest.inputs.retained_paths.iter().cloned().collect();
	let retained_base_paths = manifest
		.inputs
		.base_files
		.iter()
		.map(|file| file.relative_path.clone())
		.collect();
	let actual = capture_input_manifest(ShadowCaptureRequest {
		playset: &manifest.inputs.playset,
		game_root: &manifest.inputs.game_root,
		game_version: &manifest.inputs.game_version,
		retained_paths: &retained_paths,
		retained_base_paths: &retained_base_paths,
		base_snapshot_identity: &manifest.inputs.base_snapshot_identity,
		force: manifest.inputs.force,
		executable,
	})?;
	if actual.comparison_id == manifest.comparison_id {
		return Ok(());
	}
	let changed = changed_input_fields(&manifest.inputs, &actual.inputs);
	Err(io::Error::new(
		io::ErrorKind::InvalidData,
		format!(
			"shadow input mismatch in {}: expected comparison {}, captured {}",
			changed.join(", "),
			manifest.comparison_id,
			actual.comparison_id
		),
	))
}

fn validate_manifest(manifest: &ShadowInputManifest) -> io::Result<()> {
	if manifest.schema != SHADOW_COMPARE_SCHEMA {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			format!(
				"unsupported shadow input schema {}; expected {SHADOW_COMPARE_SCHEMA}",
				manifest.schema
			),
		));
	}
	let actual = comparison_id_for_inputs(&manifest.inputs)?;
	if actual != manifest.comparison_id {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"shadow input manifest comparison_id does not match its contents",
		));
	}
	Ok(())
}

fn comparison_id_for_inputs(inputs: &ShadowComparisonInputs) -> io::Result<String> {
	let encoded = serde_json::to_vec(inputs).map_err(io::Error::other)?;
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-shadow-compare-v2\0");
	hash_field(&mut hasher, &encoded);
	Ok(hasher.finalize().to_hex().to_string())
}

fn changed_input_fields(
	expected: &ShadowComparisonInputs,
	actual: &ShadowComparisonInputs,
) -> Vec<&'static str> {
	let mut changed = Vec::new();
	if expected.playset != actual.playset || expected.playset_hash != actual.playset_hash {
		changed.push("playset");
	}
	if expected.launcher_descriptors != actual.launcher_descriptors {
		changed.push("launcher_descriptors");
	}
	if expected.mods != actual.mods {
		changed.push("mod_contents");
	}
	if expected.game_root != actual.game_root || expected.game_version != actual.game_version {
		changed.push("game");
	}
	if expected.base_snapshot_identity != actual.base_snapshot_identity {
		changed.push("base_snapshot_identity");
	}
	if expected.base_files != actual.base_files {
		changed.push("base_game_files");
	}
	if expected.foch_config_hash != actual.foch_config_hash {
		changed.push("foch_config");
	}
	if expected.resolution_files != actual.resolution_files {
		changed.push("resolution_files");
	}
	if expected.executable != actual.executable
		|| expected.executable_hash != actual.executable_hash
	{
		changed.push("executable");
	}
	if expected.retained_paths != actual.retained_paths {
		changed.push("retained_paths");
	}
	if expected.force != actual.force {
		changed.push("force");
	}
	if changed.is_empty() {
		changed.push("unknown_fields");
	}
	changed
}

fn launcher_descriptor_identities(
	playset: &Path,
	enabled_mods: &[String],
) -> io::Result<Vec<ShadowFileIdentity>> {
	let parent = playset.parent().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"playset path has no parent directory",
		)
	})?;
	enabled_mods
		.iter()
		.map(|relative_path| {
			let path = parent.join(relative_path);
			let content_hash = match digest_file(&path) {
				Ok(hash) => Some(hash),
				Err(error) if error.kind() == io::ErrorKind::NotFound => None,
				Err(error) => return Err(error),
			};
			Ok(ShadowFileIdentity {
				relative_path: relative_path.replace('\\', "/"),
				content_hash,
			})
		})
		.collect()
}

fn file_identities(
	root: &Path,
	relative_paths: &BTreeSet<String>,
) -> io::Result<Vec<ShadowFileIdentity>> {
	relative_paths
		.iter()
		.map(|relative_path| {
			let content_hash = match digest_file(&root.join(relative_path)) {
				Ok(hash) => Some(hash),
				Err(error) if error.kind() == io::ErrorKind::NotFound => None,
				Err(error) => return Err(error),
			};
			Ok(ShadowFileIdentity {
				relative_path: normalize_relative_path(Path::new(relative_path)),
				content_hash,
			})
		})
		.collect()
}

fn select_retained_base_paths(
	requested_paths: &BTreeSet<String>,
	base_inventory_paths: &[String],
) -> BTreeSet<String> {
	let requested = requested_paths
		.iter()
		.map(|path| normalize_relative_path(Path::new(path)))
		.collect::<BTreeSet<_>>();
	let selected_modules = requested
		.iter()
		.filter_map(|path| definition_module_id(path))
		.collect::<BTreeSet<_>>();
	let mut selected = requested
		.iter()
		.filter(|path| definition_module_id(path).is_none())
		.cloned()
		.collect::<BTreeSet<_>>();
	selected.extend(
		base_inventory_paths
			.iter()
			.map(|path| normalize_relative_path(Path::new(path)))
			.filter(|path| {
				requested.contains(path)
					|| definition_module_id(path)
						.is_some_and(|module| selected_modules.contains(&module))
			}),
	);
	selected
}

fn definition_module_id(relative_path: &str) -> Option<(String, String)> {
	let descriptor = eu4_profile().classify_content_family(Path::new(relative_path))?;
	if !matches!(
		descriptor.load_policy,
		ContentLoadPolicy::DefinitionModule(_)
	) {
		return None;
	}
	Some((
		descriptor.id.as_str().to_string(),
		module_name_for_descriptor(Path::new(relative_path), descriptor),
	))
}

fn normalize_relative_path(path: &Path) -> String {
	path.to_string_lossy().replace('\\', "/")
}

fn effective_foch_config_inputs(playset: &Path) -> io::Result<(String, Vec<ShadowFileIdentity>)> {
	let playset_root = playset.parent().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			"playset path has no parent directory",
		)
	})?;
	let config = FochConfig::try_load(playset_root).map_err(io::Error::other)?;
	let encoded = serde_json::to_vec(&config).map_err(io::Error::other)?;
	let paths = config
		.resolutions
		.iter()
		.filter_map(|resolution| resolution.use_file.clone())
		.collect::<BTreeSet<_>>();
	let resolution_files = paths
		.into_iter()
		.map(|path| {
			let content_hash = match digest_file(&path) {
				Ok(hash) => Some(hash),
				Err(error) if error.kind() == io::ErrorKind::NotFound => None,
				Err(error) => return Err(error),
			};
			Ok(ShadowFileIdentity {
				relative_path: normalize_relative_path(&path),
				content_hash,
			})
		})
		.collect::<io::Result<Vec<_>>>()?;
	Ok((
		blake3::hash(&encoded).to_hex().to_string(),
		resolution_files,
	))
}

fn hash_mod_tree(root: &Path, filter: &FileFilter) -> io::Result<String> {
	let mut files = Vec::new();
	for entry in walkdir::WalkDir::new(root).follow_links(false) {
		let entry = entry.map_err(io::Error::other)?;
		if !entry.file_type().is_file() {
			continue;
		}
		let relative = entry.path().strip_prefix(root).map_err(io::Error::other)?;
		if relative != Path::new("descriptor.mod") && !filter.accepts(relative) {
			continue;
		}
		files.push((relative.to_path_buf(), entry.path().to_path_buf()));
	}
	files.sort_by(|left, right| left.0.cmp(&right.0));
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"foch-shadow-mod-v1\0");
	for (relative, absolute) in files {
		hash_field(
			&mut hasher,
			relative.to_string_lossy().replace('\\', "/").as_bytes(),
		);
		hash_file_payload(&mut hasher, &absolute)?;
	}
	Ok(hasher.finalize().to_hex().to_string())
}

fn digest_file(path: &Path) -> io::Result<String> {
	let mut hasher = blake3::Hasher::new();
	hash_file_payload(&mut hasher, path)?;
	Ok(hasher.finalize().to_hex().to_string())
}

fn hash_file_payload(hasher: &mut blake3::Hasher, path: &Path) -> io::Result<()> {
	let mut file = fs::File::open(path)?;
	hasher.update(&file.metadata()?.len().to_le_bytes());
	let mut buffer = vec![0_u8; 64 * 1024];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(())
}

fn output_hashes(root: &Path) -> io::Result<BTreeMap<String, String>> {
	if !root.is_dir() {
		return Ok(BTreeMap::new());
	}
	let mut hashes = BTreeMap::new();
	for entry in walkdir::WalkDir::new(root).follow_links(false) {
		let entry = entry.map_err(io::Error::other)?;
		if !entry.file_type().is_file() {
			continue;
		}
		let relative = entry.path().strip_prefix(root).map_err(io::Error::other)?;
		if relative
			.components()
			.any(|component| component.as_os_str() == ".foch")
		{
			continue;
		}
		let normalized = relative.to_string_lossy().replace('\\', "/");
		hashes.insert(normalized, digest_file(entry.path())?);
	}
	Ok(hashes)
}

fn report_diagnostics(report: &MergeReport) -> Vec<ShadowDiagnostic> {
	let mut diagnostics = Vec::new();
	if let Some(reason) = report.fatal_reason.as_ref() {
		diagnostics.push(ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Fatal,
			path: None,
			message: reason.clone(),
		});
	}
	diagnostics.extend(
		report
			.warnings
			.iter()
			.cloned()
			.map(|message| ShadowDiagnostic {
				kind: ShadowDiagnosticKind::Warning,
				path: None,
				message,
			}),
	);
	diagnostics.extend(
		report
			.conflict_resolutions
			.iter()
			.map(|resolution| ShadowDiagnostic {
				kind: ShadowDiagnosticKind::Conflict,
				path: Some(resolution.path.clone()),
				message: resolution.reason.clone(),
			}),
	);
	diagnostics.extend(report.handler_resolutions.iter().map(|resolution| {
		ShadowDiagnostic {
			kind: ShadowDiagnosticKind::HandlerResolution,
			path: Some(resolution.path.clone()),
			message: resolution
				.rationale
				.clone()
				.unwrap_or_else(|| resolution.action.clone()),
		}
	}));
	diagnostics
}

fn error_record(
	manifest: &ShadowInputManifest,
	output_dir: &Path,
	kernel: MergeKernelMode,
	started: Instant,
	message: String,
) -> ShadowRunRecord {
	ShadowRunRecord {
		schema: SHADOW_COMPARE_SCHEMA.to_string(),
		comparison_id: manifest.comparison_id.clone(),
		kernel: kernel.as_str().to_string(),
		output_dir: output_dir.to_path_buf(),
		output_valid: false,
		elapsed_ms: elapsed_ms(started),
		status: "error".to_string(),
		exit_code: None,
		manual_conflict_count: None,
		handler_resolution_count: None,
		generated_file_count: None,
		fatal_reason: None,
		error: Some(message.clone()),
		diagnostics: vec![ShadowDiagnostic {
			kind: ShadowDiagnosticKind::Error,
			path: None,
			message,
		}],
	}
}

fn report_status_name(status: MergeReportStatus) -> &'static str {
	match status {
		MergeReportStatus::Ready => "ready",
		MergeReportStatus::PartialSuccess => "partial_success",
		MergeReportStatus::Blocked => "blocked",
		MergeReportStatus::Fatal => "fatal",
	}
}

fn report_output_valid(status: MergeReportStatus) -> bool {
	!matches!(status, MergeReportStatus::Fatal)
}

fn elapsed_ms(started: Instant) -> u64 {
	u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
	hasher.update(&(value.len() as u64).to_le_bytes());
	hasher.update(value);
}

#[cfg(test)]
mod tests {
	use super::*;
	use foch_core::model::MergeReportConflictResolution;

	struct InputFixture {
		_temp: tempfile::TempDir,
		playset: PathBuf,
		game_root: PathBuf,
		executable: PathBuf,
		mod_file: PathBuf,
		base_file: PathBuf,
		retained_paths: BTreeSet<String>,
		retained_base_paths: BTreeSet<String>,
	}

	fn input_fixture() -> InputFixture {
		let temp = tempfile::tempdir().unwrap();
		let data_root = temp.path().join("Europa Universalis IV");
		let game_root = temp.path().join("game");
		let mod_root = temp.path().join("mod-a");
		let mod_file = mod_root.join("events/a.txt");
		let base_file = game_root.join("events/a.txt");
		let executable = temp.path().join("foch-mq");
		fs::create_dir_all(data_root.join("mod")).unwrap();
		fs::create_dir_all(mod_file.parent().unwrap()).unwrap();
		fs::create_dir_all(base_file.parent().unwrap()).unwrap();
		fs::write(&mod_file, "test.1 = { trigger = { always = yes } }\n").unwrap();
		fs::write(&base_file, "test.1 = { trigger = { always = no } }\n").unwrap();
		fs::write(
			mod_root.join("descriptor.mod"),
			"name=\"mod-a\"\nremote_file_id=\"1\"\n",
		)
		.unwrap();
		fs::write(
			data_root.join("mod/ugc_1.mod"),
			format!(
				"name=\"mod-a\"\npath=\"{}\"\nremote_file_id=\"1\"\n",
				mod_root.display()
			),
		)
		.unwrap();
		let playset = data_root.join("dlc_load.json");
		fs::write(
			&playset,
			r#"{"enabled_mods":["mod/ugc_1.mod"],"disabled_dlcs":[]}"#,
		)
		.unwrap();
		fs::write(&executable, "binary-v1").unwrap();
		InputFixture {
			_temp: temp,
			playset,
			game_root,
			executable,
			mod_file,
			base_file,
			retained_paths: BTreeSet::from(["events/a.txt".to_string()]),
			retained_base_paths: BTreeSet::from(["events/a.txt".to_string()]),
		}
	}

	fn capture(fixture: &InputFixture, force: bool, base: &str) -> ShadowInputManifest {
		capture_input_manifest(ShadowCaptureRequest {
			playset: &fixture.playset,
			game_root: &fixture.game_root,
			game_version: "1.37.5",
			retained_paths: &fixture.retained_paths,
			retained_base_paths: &fixture.retained_base_paths,
			base_snapshot_identity: base,
			force,
			executable: &fixture.executable,
		})
		.unwrap()
	}

	fn test_record(
		manifest: &ShadowInputManifest,
		output_dir: &Path,
		status: &str,
		output_valid: bool,
	) -> ShadowRunRecord {
		ShadowRunRecord {
			schema: SHADOW_COMPARE_SCHEMA.to_string(),
			comparison_id: manifest.comparison_id.clone(),
			kernel: "test".to_string(),
			output_dir: output_dir.to_path_buf(),
			output_valid,
			elapsed_ms: 1,
			status: status.to_string(),
			exit_code: output_valid.then_some(0),
			manual_conflict_count: None,
			handler_resolution_count: None,
			generated_file_count: None,
			fatal_reason: None,
			error: (!output_valid).then(|| "failed".to_string()),
			diagnostics: Vec::new(),
		}
	}

	#[test]
	fn output_diff_is_sorted_and_ignores_internal_reports() {
		let legacy = tempfile::tempdir().unwrap();
		let structured = tempfile::tempdir().unwrap();
		fs::create_dir_all(legacy.path().join("events")).unwrap();
		fs::create_dir_all(structured.path().join("events")).unwrap();
		fs::create_dir_all(legacy.path().join(".foch")).unwrap();
		fs::create_dir_all(structured.path().join(".foch")).unwrap();
		fs::write(legacy.path().join("events/a.txt"), "legacy").unwrap();
		fs::write(structured.path().join("events/a.txt"), "structured").unwrap();
		fs::write(legacy.path().join("events/only-left.txt"), "left").unwrap();
		fs::write(legacy.path().join(".foch/report.json"), "one").unwrap();
		fs::write(structured.path().join(".foch/report.json"), "two").unwrap();

		let deltas = diff_output_dirs(legacy.path(), structured.path()).unwrap();

		assert_eq!(
			deltas
				.iter()
				.map(|delta| delta.relative_path.as_str())
				.collect::<Vec<_>>(),
			vec!["events/a.txt", "events/only-left.txt"]
		);
	}

	#[test]
	fn comparison_identity_changes_with_mod_contents() {
		let fixture = input_fixture();
		let before = capture(&fixture, false, "sha256:base-v1");
		fs::write(
			&fixture.mod_file,
			"test.1 = { trigger = { always = no } }\n",
		)
		.unwrap();
		let after = capture(&fixture, false, "sha256:base-v1");

		assert_ne!(before.comparison_id, after.comparison_id);
		assert_ne!(before.inputs.mods, after.inputs.mods);
	}

	#[test]
	fn comparison_identity_changes_with_retained_base_contents() {
		let fixture = input_fixture();
		let before = capture(&fixture, false, "sha256:base-v1");
		fs::write(
			&fixture.base_file,
			"test.1 = { trigger = { has_dlc = \"Emperor\" } }\n",
		)
		.unwrap();
		let after = capture(&fixture, false, "sha256:base-v1");

		assert_ne!(before.comparison_id, after.comparison_id);
		assert_ne!(before.inputs.base_files, after.inputs.base_files);
	}

	#[test]
	fn comparison_identity_changes_with_effective_foch_config() {
		let fixture = input_fixture();
		let before = capture(&fixture, false, "sha256:base-v1");
		fs::write(
			fixture.playset.parent().unwrap().join("foch.toml"),
			"[emit]\nindent = \"  \"\n",
		)
		.unwrap();
		let after = capture(&fixture, false, "sha256:base-v1");

		assert_ne!(before.comparison_id, after.comparison_id);
		assert_ne!(
			before.inputs.foch_config_hash,
			after.inputs.foch_config_hash
		);
	}

	#[test]
	fn comparison_identity_changes_with_external_resolution_contents() {
		let fixture = input_fixture();
		let resolution = fixture.playset.parent().unwrap().join("resolution.txt");
		fs::write(&resolution, "test.1 = { always = yes }\n").unwrap();
		fs::write(
			fixture.playset.parent().unwrap().join("foch.toml"),
			format!(
				"[[resolutions]]\nfile = \"events/a.txt\"\nuse_file = {:?}\n",
				resolution.to_string_lossy()
			),
		)
		.unwrap();
		let before = capture(&fixture, false, "sha256:base-v1");
		fs::write(&resolution, "test.1 = { always = no }\n").unwrap();
		let after = capture(&fixture, false, "sha256:base-v1");

		assert_ne!(before.comparison_id, after.comparison_id);
		assert_ne!(
			before.inputs.resolution_files,
			after.inputs.resolution_files
		);
	}

	#[test]
	fn retained_base_paths_expand_selected_definition_module() {
		let requested =
			BTreeSet::from(["common/governments/zz_foch_merged_governments.txt".to_string()]);
		let inventory = vec![
			"common/governments/00_governments.txt".to_string(),
			"common/governments/01_reforms.txt".to_string(),
			"events/Unrelated.txt".to_string(),
		];

		assert_eq!(
			select_retained_base_paths(&requested, &inventory),
			BTreeSet::from([
				"common/governments/00_governments.txt".to_string(),
				"common/governments/01_reforms.txt".to_string(),
			])
		);
	}

	#[test]
	fn retained_base_paths_keep_exact_path_absent_from_snapshot_inventory() {
		let requested = BTreeSet::from(["events/NewEvent.txt".to_string()]);

		assert_eq!(
			select_retained_base_paths(&requested, &[]),
			requested,
			"an exact path must record an absent hash so later appearance changes identity"
		);
	}

	#[test]
	fn comparison_identity_binds_executable_force_and_base_snapshot() {
		let fixture = input_fixture();
		let baseline = capture(&fixture, false, "sha256:base-v1");
		let forced = capture(&fixture, true, "sha256:base-v1");
		let new_base = capture(&fixture, false, "sha256:base-v2");
		fs::write(&fixture.executable, "binary-v2").unwrap();
		let new_executable = capture(&fixture, false, "sha256:base-v1");

		assert_ne!(baseline.comparison_id, forced.comparison_id);
		assert_ne!(baseline.comparison_id, new_base.comparison_id);
		assert_ne!(baseline.comparison_id, new_executable.comparison_id);
	}

	#[test]
	fn failed_arm_removes_preexisting_output_and_records_input_reason() {
		let fixture = input_fixture();
		let manifest = capture(&fixture, false, "sha256:base-v1");
		let output = fixture.game_root.join("structured");
		fs::create_dir_all(output.join("events")).unwrap();
		fs::write(output.join("events/stale.txt"), "stale").unwrap();
		let different_executable = fixture.game_root.join("different-foch-mq");
		fs::write(&different_executable, "different-binary").unwrap();

		let record = run_shadow_arm(ShadowRunRequest {
			manifest: &manifest,
			output_dir: &output,
			executable: &different_executable,
			kernel: MergeKernelMode::Structured,
		});

		assert_eq!(record.status, "error");
		assert!(!record.output_valid);
		assert!(!output.exists());
		assert!(
			record
				.diagnostics
				.iter()
				.any(|diagnostic| diagnostic.message.contains("executable"))
		);
	}

	#[test]
	fn failed_arm_output_is_never_compared() {
		let fixture = input_fixture();
		let manifest = capture(&fixture, false, "sha256:base-v1");
		let legacy_dir = fixture.game_root.join("legacy");
		let structured_dir = fixture.game_root.join("structured");
		fs::create_dir_all(&legacy_dir).unwrap();
		fs::create_dir_all(&structured_dir).unwrap();
		fs::write(legacy_dir.join("stale.txt"), "old-left").unwrap();
		fs::write(structured_dir.join("stale.txt"), "old-right").unwrap();
		let legacy = test_record(&manifest, &legacy_dir, "ready", true);
		let structured = test_record(&manifest, &structured_dir, "error", false);

		let report = build_comparison_report(manifest, legacy, structured).unwrap();

		assert!(!report.outputs_compared);
		assert!(report.file_deltas.is_empty());
	}

	#[test]
	fn fatal_reports_are_not_valid_comparison_outputs() {
		assert!(!report_output_valid(MergeReportStatus::Fatal));
		assert!(report_output_valid(MergeReportStatus::Blocked));
	}

	#[test]
	fn report_diagnostics_preserve_warning_and_conflict_reasons() {
		let mut report = MergeReport::default();
		report
			.warnings
			.push("structured merge unsupported: expected exactly two final sinks".to_string());
		report
			.conflict_resolutions
			.push(MergeReportConflictResolution {
				path: "events/a.txt".to_string(),
				reason: "ordering conflict".to_string(),
				..MergeReportConflictResolution::default()
			});

		let diagnostics = report_diagnostics(&report);

		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.kind == ShadowDiagnosticKind::Warning
				&& diagnostic.message.contains("unsupported")
		}));
		assert!(diagnostics.iter().any(|diagnostic| {
			diagnostic.kind == ShadowDiagnosticKind::Conflict
				&& diagnostic.path.as_deref() == Some("events/a.txt")
				&& diagnostic.message == "ordering conflict"
		}));
	}
}
