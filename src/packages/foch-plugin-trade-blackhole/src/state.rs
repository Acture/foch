//! Plugin state reported to the host through `status`/`report`.

use std::sync::OnceLock;

/// The process-wide state cell. `status`/`report` read it from any thread.
static CELL: OnceLock<StateCell> = OnceLock::new();

fn cell() -> &'static StateCell {
	CELL.get_or_init(StateCell::new)
}

/// Publish a state transition (init or the detour).
pub fn set(state: i32, reason: Reason) {
	cell().set(state, reason);
}

/// The current status snapshot for the host.
pub fn status() -> FochStatus {
	cell().snapshot()
}

/// Publish the terminal state a caught panic in the monthly step
/// produces; called from the detour after the unwind was caught.
pub fn set_failed_after_panic() {
	cell().set(state::FAILED, Reason::EngineError);
}

use foch_plugin_abi::{FochStatus, FochStr, state};

/// Plugin-defined reason codes for non-active states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
	None = 0,
	/// The running `eu4.exe` is not the reversed 1.37.5 Steam build.
	ExeMismatch = 1,
	/// Signatures for this build are not derived yet.
	OffsetsPending = 2,
	/// A signature did not match exactly once in the executable sections.
	PatternFailed = 3,
	/// A pattern matched but the data it anchors is not the expected shape.
	BadLayout = 4,
	/// A mutation or observer walk failed inside the game thread.
	EngineError = 5,
}

impl Reason {
	fn detail(self) -> &'static str {
		match self {
			Self::None => "",
			Self::ExeMismatch => "eu4.exe is not the pinned 1.37.5 Steam x64 build",
			Self::OffsetsPending => "signatures for this build are pending the RE spike",
			Self::PatternFailed => "a pinned signature did not resolve to one address",
			Self::BadLayout => "resolved data does not match the reversed layout",
			Self::EngineError => "the trade walk failed on the game thread",
		}
	}
}

/// The one state cell shared by `init`, `status` and the detour. `status`
/// is polled by the host while `INITIALIZING`; every other transition is
/// published through `report`.
pub struct StateCell {
	state: std::sync::atomic::AtomicI32,
	reason: std::sync::atomic::AtomicI32,
}

impl StateCell {
	pub const fn new() -> Self {
		Self {
			state: std::sync::atomic::AtomicI32::new(state::INITIALIZING),
			reason: std::sync::atomic::AtomicI32::new(Reason::None as i32),
		}
	}

	pub fn set(&self, state: i32, reason: Reason) {
		self.state
			.store(state, std::sync::atomic::Ordering::Release);
		self.reason
			.store(reason as i32, std::sync::atomic::Ordering::Release);
	}

	pub fn snapshot(&self) -> FochStatus {
		let reason = self.reason.load(std::sync::atomic::Ordering::Acquire);
		let reason = u32::try_from(reason)
			.ok()
			.and_then(|code| match code {
				0 => Some(Reason::None),
				1 => Some(Reason::ExeMismatch),
				2 => Some(Reason::OffsetsPending),
				3 => Some(Reason::PatternFailed),
				4 => Some(Reason::BadLayout),
				5 => Some(Reason::EngineError),
				_ => None,
			})
			.unwrap_or(Reason::EngineError);
		let detail = reason.detail();
		FochStatus {
			size: std::mem::size_of::<FochStatus>() as u32,
			state: self.state.load(std::sync::atomic::Ordering::Acquire),
			reason_code: reason as i32,
			detail: FochStr {
				ptr: detail.as_ptr(),
				len: detail.len(),
			},
		}
	}
}

impl Default for StateCell {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn snapshot_round_trips_every_reason() {
		let cell = StateCell::new();
		for reason in [
			Reason::None,
			Reason::ExeMismatch,
			Reason::OffsetsPending,
			Reason::PatternFailed,
			Reason::BadLayout,
			Reason::EngineError,
		] {
			cell.set(state::ACTIVE, reason);
			let status = cell.snapshot();
			assert_eq!(status.state, state::ACTIVE);
			assert_eq!(status.reason_code, reason as i32);
			if reason != Reason::None {
				assert!(!unsafe { status.detail.as_str().unwrap() }.is_empty());
			}
		}
	}

	#[test]
	fn an_unknown_reason_code_maps_to_engine_error() {
		let cell = StateCell::new();
		cell.reason.store(99, std::sync::atomic::Ordering::Release);
		assert_eq!(cell.snapshot().reason_code, Reason::EngineError as i32);
	}
}
