use foch_core::model::MergeReportStatus;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendContract {
	pub base_data_schema_version: u32,
	pub initial_merge_status: MergeReportStatus,
}

pub fn backend_contract() -> BackendContract {
	BackendContract {
		base_data_schema_version: foch_engine::BASE_DATA_SCHEMA_VERSION,
		initial_merge_status: MergeReportStatus::Blocked,
	}
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	let _contract = backend_contract();
	tauri::Builder::default()
		.run(tauri::generate_context!())
		.expect("failed to run Foch desktop");
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn desktop_backend_links_shared_product_crates() {
		let contract = backend_contract();

		assert_eq!(
			contract.base_data_schema_version,
			foch_engine::BASE_DATA_SCHEMA_VERSION
		);
		assert_eq!(contract.initial_merge_status, MergeReportStatus::Blocked);
	}
}
