//! Where a merge unit is while it runs, for the materialize heartbeat to show.
//!
//! A worker gives each unit a [`UnitProgress`] and makes it current on its
//! thread while the unit is analyzed. The steps of the analysis note their
//! stage, the item each thread is on and that item's inner counter; a unit
//! reported as long-running then shows the work it is stuck in. Without a
//! current unit, as in tests and on the coordinating thread, every note is a
//! no-op.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

#[derive(Default)]
pub(crate) struct UnitProgress {
	state: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
	stage: String,
	/// The item each thread working on the unit is on, and its inner counter.
	items: HashMap<ThreadId, (String, String)>,
}

impl UnitProgress {
	fn lock(&self) -> MutexGuard<'_, ProgressState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// The stage and the items in progress.
	pub(crate) fn describe(&self) -> String {
		let state = self.lock();
		let mut parts: Vec<String> = Vec::new();
		if !state.stage.is_empty() {
			parts.push(state.stage.clone());
		}
		let mut items: Vec<String> = state
			.items
			.values()
			.map(|(item, detail)| {
				if detail.is_empty() {
					item.clone()
				} else {
					format!("{item}: {detail}")
				}
			})
			.collect();
		items.sort();
		parts.extend(items);
		parts.join("; ")
	}
}

thread_local! {
	static CURRENT: RefCell<Option<Arc<UnitProgress>>> = const { RefCell::new(None) };
}

/// The unit current on this thread, to carry onto threads it fans out to.
pub(crate) fn current() -> Option<Arc<UnitProgress>> {
	CURRENT.with(|current| current.borrow().clone())
}

/// Make `progress` this thread's current unit until the returned guard drops.
pub(crate) fn attach(progress: Option<Arc<UnitProgress>>) -> UnitGuard {
	UnitGuard {
		previous: CURRENT.with(|current| current.replace(progress)),
	}
}

pub(crate) struct UnitGuard {
	previous: Option<Arc<UnitProgress>>,
}

impl Drop for UnitGuard {
	fn drop(&mut self) {
		let previous = self.previous.take();
		CURRENT.with(|current| *current.borrow_mut() = previous);
	}
}

fn with_current(note: impl FnOnce(&UnitProgress)) {
	CURRENT.with(|current| {
		if let Some(progress) = current.borrow().as_deref() {
			note(progress);
		}
	});
}

/// Note the stage the current unit is in.
pub(crate) fn stage(stage: impl FnOnce() -> String) {
	with_current(|progress| progress.lock().stage = stage());
}

/// Note the item this thread works on until the returned guard drops.
pub(crate) fn item(item: impl FnOnce() -> String) -> ItemGuard {
	let mut noted: bool = false;
	with_current(|progress| {
		progress
			.lock()
			.items
			.insert(thread::current().id(), (item(), String::new()));
		noted = true;
	});
	ItemGuard { noted }
}

/// Note the inner counter of the item this thread works on.
pub(crate) fn detail(detail: impl FnOnce() -> String) {
	with_current(|progress| {
		if let Some((_, current)) = progress.lock().items.get_mut(&thread::current().id()) {
			*current = detail();
		}
	});
}

pub(crate) struct ItemGuard {
	noted: bool,
}

impl Drop for ItemGuard {
	fn drop(&mut self) {
		if self.noted {
			with_current(|progress| {
				progress.lock().items.remove(&thread::current().id());
			});
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn notes_reach_only_the_current_unit_and_items_end_with_their_guard() {
		stage(|| "ignored".to_string());
		let progress: Arc<UnitProgress> = Arc::default();
		let unit = attach(Some(Arc::clone(&progress)));
		stage(|| "observe".to_string());
		let guard = item(|| "definition a".to_string());
		detail(|| "chain case 3/9".to_string());
		assert_eq!(progress.describe(), "observe; definition a: chain case 3/9");
		drop(guard);
		assert_eq!(progress.describe(), "observe");
		drop(unit);
		assert!(current().is_none());
	}
}
