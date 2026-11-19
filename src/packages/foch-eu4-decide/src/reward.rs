//! Adversarial reward against the vanilla AI on the same horizons.

/// Which classification head supplies the reward scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Channel {
	Default,
	Trade,
	Expansion,
}

/// Changes over one horizon. Gold is ducats and manpower is men.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Yields {
	pub provinces_from_target: f32,
	pub own_provinces: f32,
	pub gold: f32,
	pub manpower: f32,
	pub trade: f32,
}

/// Twelve months and ten years after the interest was chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Horizons {
	pub near: Yields,
	pub far: Yields,
}

/// Model outcome minus the vanilla AI outcome.
///
/// The game AI is the adversary. A positive value means the chosen interest
/// beat what vanilla did over both horizons. It is not a policy that abstains.
pub fn adversarial_reward(channel: Channel, model: &Horizons, vanilla: &Horizons) -> f32 {
	score(channel, model) - score(channel, vanilla)
}

fn score(channel: Channel, horizons: &Horizons) -> f32 {
	scale(channel, &horizons.near) + scale(channel, &horizons.far)
}

fn scale(channel: Channel, yields: &Yields) -> f32 {
	match channel {
		Channel::Trade => yields.trade,
		Channel::Expansion => yields.provinces_from_target + yields.own_provinces,
		Channel::Default => {
			yields.provinces_from_target
				+ yields.own_provinces
				+ yields.gold / 100.0
				+ yields.manpower / 1000.0
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn trade_reward_is_the_gap_against_vanilla() {
		let vanilla = Horizons {
			near: Yields {
				trade: 10.0,
				gold: 100.0,
				..Yields::default()
			},
			far: Yields {
				trade: 5.0,
				..Yields::default()
			},
		};
		let model = Horizons {
			near: Yields {
				trade: 14.0,
				gold: 400.0,
				..Yields::default()
			},
			far: Yields {
				trade: 5.0,
				..Yields::default()
			},
		};
		assert_eq!(adversarial_reward(Channel::Trade, &model, &vanilla), 4.0);
		assert_eq!(adversarial_reward(Channel::Default, &model, &vanilla), 3.0);
	}

	#[test]
	fn expansion_ignores_gold_and_trade() {
		let vanilla = Horizons::default();
		let model = Horizons {
			near: Yields {
				gold: 200.0,
				manpower: 3000.0,
				own_provinces: 1.0,
				trade: 9.0,
				..Yields::default()
			},
			..Horizons::default()
		};
		assert_eq!(
			adversarial_reward(Channel::Expansion, &model, &vanilla),
			1.0
		);
		assert_eq!(
			adversarial_reward(Channel::Default, &model, &vanilla),
			1.0 + 2.0 + 3.0
		);
	}
}
