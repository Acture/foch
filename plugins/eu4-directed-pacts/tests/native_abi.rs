use foch_plugin_directed_pacts::foch_plugin_get_api;
use foch_plugin_sdk::{ABI_MAJOR, FochHostApi, FochStatus, result, state};

#[test]
fn native_entry_rejects_wrong_major_and_short_host_without_reading_callbacks() {
	unsafe {
		assert!(foch_plugin_get_api(ABI_MAJOR + 1).is_null());
		let api = &*foch_plugin_get_api(ABI_MAJOR);
		assert_eq!((api.init)(std::ptr::null()), result::E_INVALID);
		let prefix = 4u32;
		assert_eq!(
			(api.init)((&prefix as *const u32).cast::<FochHostApi>()),
			result::E_UNSUPPORTED
		);
	}
}

#[test]
fn status_does_not_claim_game_support_without_a_verified_native_bridge() {
	unsafe {
		let api = &*foch_plugin_get_api(ABI_MAJOR);
		let mut status = FochStatus::new(state::INITIALIZING);
		(api.status)(&mut status);
		assert_eq!(status.state, state::REFUSED);
		assert_ne!(status.reason_code, 0);
		assert!(status.detail.as_str().unwrap().contains("war"));
	}
}
