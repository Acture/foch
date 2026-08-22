#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	tauri::Builder::default()
		.run(tauri::generate_context!())
		.expect("failed to run Foch desktop");
}

#[cfg(test)]
mod tests {
	#[test]
	fn shared_product_dependencies_compile() {
		let _: u32 = foch_engine::BASE_DATA_SCHEMA_VERSION;
		let _: foch_core::model::MergeReportStatus = foch_core::model::MergeReportStatus::Blocked;
	}
}
