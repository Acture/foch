//! Four interests. This crate does not write hostile attitude or rival slots.

/// How many countries a monthly plan may score.
pub const INTEREST_LIMIT: usize = 16;

/// Learned interest toward one country the vanilla AI already noticed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interest {
	FriendlyAlliance,
	Passive,
	SchemeBetrayal,
	DirectBetrayal,
}

impl Interest {
	pub const fn index(self) -> usize {
		match self {
			Self::FriendlyAlliance => 0,
			Self::Passive => 1,
			Self::SchemeBetrayal => 2,
			Self::DirectBetrayal => 3,
		}
	}
}

/// `Vanilla` means the head did not pick an interest, so the game AI is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Choice {
	Vanilla,
	Interest(Interest),
}

/// The first [`INTEREST_LIMIT`] entries. The caller orders them by vanilla interest.
pub fn interest_window<T>(candidates: &[T]) -> &[T] {
	let end = candidates.len().min(INTEREST_LIMIT);
	&candidates[..end]
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn interest_window_stops_at_sixteen() {
		let values: Vec<u8> = (0..20).collect();
		assert_eq!(interest_window(&values).len(), 16);
		assert_eq!(interest_window(&values[..4]).len(), 4);
	}
}
