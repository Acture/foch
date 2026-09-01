use crate::dto::{
	DesktopError, InputInspection, MergeAnalysisSummary, MergeDisposition, MergeUnitDetail,
	MergeUnitPage, StartMergeAnalysisResult,
};
use crate::state::DesktopState;
use tauri::State;

#[tauri::command]
pub(crate) async fn inspect_input(
	state: State<'_, DesktopState>,
) -> Result<InputInspection, DesktopError> {
	let desktop = state.inner().clone();
	tauri::async_runtime::spawn_blocking(move || desktop.inspect_input())
		.await
		.map_err(|error| DesktopError::internal(format!("input inspection failed: {error}")))?
}

#[tauri::command]
pub(crate) async fn start_merge_analysis(
	state: State<'_, DesktopState>,
) -> Result<StartMergeAnalysisResult, DesktopError> {
	let desktop = state.inner().clone();
	let analysis_id = desktop.queue_analysis()?;
	let worker_state = desktop.clone();
	let worker_id = analysis_id.clone();
	tauri::async_runtime::spawn_blocking(move || worker_state.run_analysis(&worker_id));
	Ok(StartMergeAnalysisResult { analysis_id })
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
