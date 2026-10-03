//! Count work at traversal boundaries without timing or cross-test interference.
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Work {
	pub indexed_statements: usize,
	pub indexed_resolutions: usize,
	pub trivia_statements: usize,
	pub lineage_tree_checks: usize,
}

thread_local! {
	static WORK: Cell<Work> = const { Cell::new(Work { indexed_statements: 0, indexed_resolutions: 0, trivia_statements: 0, lineage_tree_checks: 0 }) };
}

pub(super) fn compare_lineage_tree() {
	let mut work = WORK.get();
	work.lineage_tree_checks += 1;
	WORK.set(work);
}

pub(super) fn index_resolution() {
	let mut work = WORK.get();
	work.indexed_resolutions += 1;
	WORK.set(work);
}

pub(super) fn index_statement() {
	let mut work = WORK.get();
	work.indexed_statements += 1;
	WORK.set(work);
}

pub(super) fn trivia_statement() {
	let mut work = WORK.get();
	work.trivia_statements += 1;
	WORK.set(work);
}

pub(super) fn measure<T>(operation: impl FnOnce() -> T) -> (T, Work) {
	WORK.set(Work::default());
	let result = operation();
	(result, WORK.get())
}
