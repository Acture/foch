//! Classification heads over the four interests.
//!
//! Head choice follows feature names such as `entrepreneur_personality` or
//! `is_merchant_republic`, not the numeric modifiers inside those definitions.
//! A tied score keeps [`Choice::Vanilla`], so an untrained head does not invent
//! an attitude. The reward for a chosen interest is still adversarial against
//! the vanilla AI.

use crate::attitude::{Choice, Interest};
use crate::ideas::{EMBED, NameTable, mean_names, sum_names};
use crate::reward::Channel;

pub const PERSONALITY_LIMIT: usize = 3;
pub const ATTRIBUTE_LIMIT: usize = 8;
pub const FEATURES: usize = EMBED * 4 + 4 + 6;

/// Government kind copied at the monthly tick. Rank stays in the size fields.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GovernmentKind {
	#[default]
	Monarchy,
	Republic,
	Theocracy,
	Tribal,
	Native,
	Other,
}

/// Name features plus the country's size at the question.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Situation {
	pub provinces: f32,
	pub development: f32,
	pub government_rank: f32,
	pub subject: bool,
	pub government: GovernmentKind,
	pub ideas: [f32; EMBED],
	pub personalities: [f32; EMBED],
	pub attributes: [f32; EMBED],
	pub target: [f32; EMBED],
}

impl Default for Situation {
	fn default() -> Self {
		Self {
			provinces: 0.0,
			development: 0.0,
			government_rank: 1.0,
			subject: false,
			government: GovernmentKind::Monarchy,
			ideas: [0.0; EMBED],
			personalities: [0.0; EMBED],
			attributes: [0.0; EMBED],
			target: [0.0; EMBED],
		}
	}
}

impl Situation {
	/// Ideas are every adopted group name plus the national idea group name.
	/// Attributes are government attribute names, capped at eight.
	/// Personalities are the ruler's names, capped at three.
	pub fn from_names(
		table: &NameTable,
		ideas: &[&str],
		personalities: &[&str],
		attributes: &[&str],
	) -> Self {
		Self {
			ideas: mean_names(table, ideas),
			personalities: sum_names(table, personalities, PERSONALITY_LIMIT),
			attributes: mean_names(table, &attributes[..attributes.len().min(ATTRIBUTE_LIMIT)]),
			..Self::default()
		}
	}
}

pub fn encode(situation: &Situation) -> [f32; FEATURES] {
	let mut out = [0.0; FEATURES];
	out[..EMBED].copy_from_slice(&situation.ideas);
	out[EMBED..EMBED * 2].copy_from_slice(&situation.personalities);
	out[EMBED * 2..EMBED * 3].copy_from_slice(&situation.attributes);
	out[EMBED * 3..EMBED * 4].copy_from_slice(&situation.target);
	let numbers = EMBED * 4;
	out[numbers] = situation.provinces;
	out[numbers + 1] = situation.development;
	out[numbers + 2] = situation.government_rank;
	out[numbers + 3] = if situation.subject { 1.0 } else { 0.0 };
	let kind = match situation.government {
		GovernmentKind::Monarchy => 0,
		GovernmentKind::Republic => 1,
		GovernmentKind::Theocracy => 2,
		GovernmentKind::Tribal => 3,
		GovernmentKind::Native => 4,
		GovernmentKind::Other => 5,
	};
	out[numbers + 4 + kind] = 1.0;
	out
}

/// Four interest logits. A tie, including the all-zero untrained head, is vanilla.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scores {
	pub friendly: f32,
	pub passive: f32,
	pub scheme: f32,
	pub direct: f32,
}

impl Scores {
	pub fn choice(self) -> Choice {
		let ranked = [
			(self.friendly, Interest::FriendlyAlliance),
			(self.passive, Interest::Passive),
			(self.scheme, Interest::SchemeBetrayal),
			(self.direct, Interest::DirectBetrayal),
		];
		let best = ranked
			.into_iter()
			.map(|(score, _)| score)
			.fold(f32::NEG_INFINITY, f32::max);
		let winners: Vec<Interest> = ranked
			.into_iter()
			.filter(|(score, _)| *score == best)
			.map(|(_, interest)| interest)
			.collect();
		if winners.len() == 1 {
			Choice::Interest(winners[0])
		} else {
			Choice::Vanilla
		}
	}

	fn add(&mut self, other: Self) {
		self.friendly += other.friendly;
		self.passive += other.passive;
		self.scheme += other.scheme;
		self.direct += other.direct;
	}
}

/// One classification head. Zero weights score every interest the same.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadWeights {
	pub rows: [[f32; FEATURES]; 4],
}

impl HeadWeights {
	pub const fn baseline() -> Self {
		Self {
			rows: [[0.0; FEATURES]; 4],
		}
	}

	pub fn logits(&self, features: &[f32; FEATURES]) -> Scores {
		let score = |row: usize| -> f32 {
			self.rows[row]
				.iter()
				.zip(features)
				.map(|(weight, value)| weight * value)
				.sum()
		};
		Scores {
			friendly: score(0),
			passive: score(1),
			scheme: score(2),
			direct: score(3),
		}
	}
}

/// Default, trade, and expansion heads.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
	pub default_head: HeadWeights,
	pub trade_head: HeadWeights,
	pub expansion_head: HeadWeights,
}

impl Policy {
	pub const fn zeros() -> Self {
		Self {
			default_head: HeadWeights::baseline(),
			trade_head: HeadWeights::baseline(),
			expansion_head: HeadWeights::baseline(),
		}
	}

	pub fn decide(
		&self,
		situation: &Situation,
		personalities: &[&str],
		attributes: &[&str],
	) -> Choice {
		let features = encode(situation);
		let mut scores = Scores::default();
		for channel in active_channels(personalities, attributes) {
			let head = match channel {
				Channel::Default => &self.default_head,
				Channel::Trade => &self.trade_head,
				Channel::Expansion => &self.expansion_head,
			};
			scores.add(head.logits(&features));
		}
		scores.choice()
	}
}

/// A known name selects its reward head. Any other name, including unseen ones, uses default.
pub fn channel_of_name(name: &str) -> Channel {
	match name {
		"entrepreneur_personality" | "is_merchant_republic" => Channel::Trade,
		"conqueror_personality" | "bold_fighter_personality" => Channel::Expansion,
		_ => Channel::Default,
	}
}

/// Heads switched on by personality and government-attribute names.
pub fn active_channels(personalities: &[&str], attributes: &[&str]) -> Vec<Channel> {
	let mut channels = Vec::new();
	for name in personalities.iter().take(PERSONALITY_LIMIT).copied() {
		push_channel(&mut channels, channel_of_name(name));
	}
	for name in attributes.iter().take(ATTRIBUTE_LIMIT).copied() {
		push_channel(&mut channels, channel_of_name(name));
	}
	channels
}

fn push_channel(channels: &mut Vec<Channel>, channel: Channel) {
	if !channels.contains(&channel) {
		channels.push(channel);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn zero_weights_do_not_pick_an_interest() {
		let policy = Policy::zeros();
		assert_eq!(
			policy.decide(&Situation::default(), &["entrepreneur_personality"], &[]),
			Choice::Vanilla
		);
	}

	#[test]
	fn the_trade_head_reads_the_personality_name() {
		let mut policy = Policy::zeros();
		policy.trade_head.rows[0][EMBED] = 1.0;
		let mut situation = Situation::from_names(
			&NameTable::default(),
			&[],
			&["entrepreneur_personality"],
			&["is_merchant_republic"],
		);
		assert!(situation.personalities.iter().any(|value| *value != 0.0));
		assert!(situation.attributes.iter().any(|value| *value != 0.0));
		situation.personalities = [0.0; EMBED];
		situation.personalities[0] = 1.0;
		assert_eq!(
			policy.decide(
				&situation,
				&["entrepreneur_personality"],
				&["is_merchant_republic"]
			),
			Choice::Interest(Interest::FriendlyAlliance)
		);
	}

	#[test]
	fn channels_follow_names_not_unseen_spellings() {
		assert_eq!(
			active_channels(&["entrepreneur_personality"], &["is_merchant_republic"]),
			vec![Channel::Trade]
		);
		assert_eq!(
			active_channels(&["conqueror_personality"], &[]),
			vec![Channel::Expansion]
		);
		assert_eq!(
			active_channels(&["mod_unknown_personality"], &[]),
			vec![Channel::Default]
		);
	}
}
