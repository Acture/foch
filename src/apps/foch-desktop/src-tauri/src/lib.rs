mod backend;
mod commands;
mod dto;
mod plugin_backend;
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	tauri::Builder::default()
		.manage(state::DesktopState::production())
		.invoke_handler(tauri::generate_handler![
			commands::inspect_input,
			commands::start_merge_analysis,
			commands::cancel_merge_analysis,
			commands::get_merge_analysis_summary,
			commands::list_merge_units,
			commands::get_merge_unit,
			commands::commit_merge_analysis,
			commands::render_resolutions_toml,
			commands::get_plugin_overview,
			commands::install_plugin,
			commands::import_plugin_dir,
			commands::set_plugin_enabled,
			commands::update_plugin_config,
			commands::launch_eu4_with_plugins,
			commands::get_launch_status,
			commands::get_desktop_settings,
			commands::save_desktop_settings,
			commands::test_download_connectivity,
		])
		.run(tauri::generate_context!())
		.expect("failed to run Foch desktop");
}

#[cfg(test)]
mod tests {
	#[test]
	fn shared_product_dependencies_compile() {
		let _: &str = foch::DATA_VERSION;
		let _: foch::model::MergeReportStatus = foch::model::MergeReportStatus::Blocked;
	}

	#[test]
	fn desktop_backend_never_spawns_the_cli() {
		for source in [
			include_str!("backend.rs"),
			include_str!("commands.rs"),
			include_str!("state.rs"),
			include_str!("plugin_backend.rs"),
		] {
			assert!(!source.contains("std::process::Command"));
			assert!(!source.contains("Command::new"));
		}
	}
}
