use crate::dto::{
	AnalysisInputMode, DesktopConnectivityReport, DesktopError, DesktopLaunchResult,
	DesktopLaunchStatus, DesktopPluginInstall, DesktopPluginOverview, DesktopPreferencesView,
	DesktopSettingsInput, InputInspection, MergeAnalysisSummary, MergeCommitResult,
	MergeDisposition, MergeUnitDetail, MergeUnitPage, StartMergeAnalysisResult,
};
use crate::plugin_backend::{self, PlaysetContext, PluginWorkspace};
use crate::state::DesktopState;
use tauri::State;

async fn blocking_plugin<T, F>(work: F) -> Result<T, DesktopError>
where
	T: Send + 'static,
	F: FnOnce() -> Result<T, DesktopError> + Send + 'static,
{
	tauri::async_runtime::spawn_blocking(work)
		.await
		.map_err(|error| DesktopError::internal(format!("plugin command failed: {error}")))?
}

fn open_plugin(playset: Option<String>) -> Result<(PluginWorkspace, PlaysetContext), DesktopError> {
	Ok((
		plugin_backend::production_workspace()?,
		plugin_backend::production_context(playset)?,
	))
}

#[tauri::command]
pub(crate) async fn inspect_input(
	state: State<'_, DesktopState>,
) -> Result<InputInspection, DesktopError> {
	let desktop = state.inner().clone();
	tauri::async_runtime::spawn_blocking(move || desktop.inspect_input())
		.await
		.map_err(|error| DesktopError::internal(format!("input inspection failed: {error}")))?
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn start_merge_analysis(
	state: State<'_, DesktopState>,
	inspection_id: String,
	input_mode: AnalysisInputMode,
) -> Result<StartMergeAnalysisResult, DesktopError> {
	let desktop = state.inner().clone();
	let started = desktop.queue_analysis(&inspection_id, input_mode)?;
	let worker_state = desktop.clone();
	let worker_id = started.analysis_id.clone();
	tauri::async_runtime::spawn_blocking(move || worker_state.run_analysis(&worker_id));
	Ok(started)
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) fn cancel_merge_analysis(
	state: State<'_, DesktopState>,
	analysis_id: String,
) -> Result<(), DesktopError> {
	state.cancel_analysis(&analysis_id)
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) fn get_merge_analysis_summary(
	state: State<'_, DesktopState>,
	analysis_id: String,
) -> Result<MergeAnalysisSummary, DesktopError> {
	state.summary(&analysis_id)
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) fn list_merge_units(
	state: State<'_, DesktopState>,
	analysis_id: String,
	query: String,
	disposition: Option<MergeDisposition>,
	page: usize,
	page_size: usize,
) -> Result<MergeUnitPage, DesktopError> {
	state.list_units(&analysis_id, &query, disposition, page, page_size)
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) fn get_merge_unit(
	state: State<'_, DesktopState>,
	analysis_id: String,
	unit_id: String,
) -> Result<MergeUnitDetail, DesktopError> {
	state.unit(&analysis_id, &unit_id)
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn commit_merge_analysis(
	state: State<'_, DesktopState>,
	analysis_id: String,
	output_dir: String,
) -> Result<MergeCommitResult, DesktopError> {
	let desktop = state.inner().clone();
	tauri::async_runtime::spawn_blocking(move || desktop.commit_analysis(&analysis_id, &output_dir))
		.await
		.map_err(|error| DesktopError::internal(format!("merge commit failed: {error}")))?
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn get_plugin_overview(
	playset: Option<String>,
) -> Result<DesktopPluginOverview, DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(playset)?;
		plugin_backend::load_overview(&workspace, &context)
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn install_plugin(
	spec: String,
	sha256: Option<String>,
) -> Result<DesktopPluginInstall, DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(None)?;
		plugin_backend::install_plugin_spec(&workspace, &context, &spec, sha256)
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn import_plugin_dir(path: String) -> Result<DesktopPluginInstall, DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(None)?;
		plugin_backend::import_plugin_directory(&workspace, &context, std::path::Path::new(&path))
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn set_plugin_enabled(
	playset: Option<String>,
	id: String,
	enabled: bool,
	version: Option<String>,
) -> Result<(), DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(playset)?;
		plugin_backend::set_enabled(&workspace, &context, &id, enabled, version.as_deref())
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn update_plugin_config(
	playset: Option<String>,
	id: String,
	key: String,
	value: serde_json::Value,
) -> Result<(), DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(playset)?;
		plugin_backend::update_config(&workspace, &context, &id, &key, value)
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn launch_eu4_with_plugins(
	playset: Option<String>,
	prepare_only: bool,
) -> Result<DesktopLaunchResult, DesktopError> {
	blocking_plugin(move || {
		let (workspace, context) = open_plugin(playset)?;
		plugin_backend::launch_plugins_for_player(&workspace, &context, prepare_only)
	})
	.await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn get_launch_status(
	run_dir: String,
) -> Result<DesktopLaunchStatus, DesktopError> {
	blocking_plugin(move || plugin_backend::launch_status(std::path::Path::new(&run_dir))).await
}

#[tauri::command]
pub(crate) async fn get_desktop_settings() -> Result<DesktopPreferencesView, DesktopError> {
	blocking_plugin(plugin_backend::load_production_settings).await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn save_desktop_settings(
	settings: DesktopSettingsInput,
) -> Result<DesktopPreferencesView, DesktopError> {
	blocking_plugin(move || plugin_backend::save_production_settings(settings)).await
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) async fn test_download_connectivity(
	settings: Option<DesktopSettingsInput>,
) -> Result<DesktopConnectivityReport, DesktopError> {
	blocking_plugin(move || plugin_backend::test_download_connectivity(settings)).await
}
