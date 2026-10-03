#[test]
fn conflict_view_is_addressable_from_engine_root() {
	use foch::merge::ConflictView;
	let _ = std::mem::size_of::<ConflictView>();
}
