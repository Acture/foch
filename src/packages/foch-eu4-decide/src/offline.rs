//! One monthly step as a pure function. Nothing here starts EU4.

use crate::attitude::{Choice, interest_window};
use crate::ideas::NameTable;
use crate::policy::{Policy, Situation, active_channels};
use crate::reward::{Horizons, adversarial_reward};

/// Outcomes of the four interests, in [`Interest::index`] order, plus vanilla.
#[derive(Clone, Copy, Debug)]
pub struct TargetOutcomes {
	pub tag: &'static str,
	pub vanilla: Horizons,
	pub interests: [Horizons; 4],
}

/// One country and the vanilla interest list, strongest first.
pub struct Month<'a> {
	pub ideas: &'a [&'a str],
	pub personalities: &'a [&'a str],
	pub attributes: &'a [&'a str],
	pub provinces: f32,
	pub development: f32,
	pub government_rank: f32,
	pub subject: bool,
	pub targets: &'a [TargetOutcomes],
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetScore {
	pub tag: &'static str,
	pub choice: Choice,
	/// Adversarial reward against vanilla. Staying with vanilla scores zero.
	pub reward: f32,
}

/// The value being predicted: one interest toward one country.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterestPrediction {
	pub target: &'static str,
	pub choice: Choice,
}

impl TargetScore {
	pub fn prediction(&self) -> InterestPrediction {
		InterestPrediction {
			target: self.tag,
			choice: self.choice,
		}
	}
}

/// Score at most sixteen interested countries. No process, no files, no hooks.
pub fn monthly(policy: &Policy, table: &NameTable, month: &Month<'_>) -> Vec<TargetScore> {
	let mut situation =
		Situation::from_names(table, month.ideas, month.personalities, month.attributes);
	situation.provinces = month.provinces;
	situation.development = month.development;
	situation.government_rank = month.government_rank;
	situation.subject = month.subject;
	let channels = active_channels(month.personalities, month.attributes);
	interest_window(month.targets)
		.iter()
		.map(|target| {
			situation.target = table.embed(target.tag);
			let choice = policy.decide(&situation, month.personalities, month.attributes);
			let reward = match choice {
				Choice::Vanilla => 0.0,
				Choice::Interest(interest) => channels
					.iter()
					.map(|channel| {
						adversarial_reward(
							*channel,
							&target.interests[interest.index()],
							&target.vanilla,
						)
					})
					.sum(),
			};
			TargetScore {
				tag: target.tag,
				choice,
				reward,
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::attitude::Interest;
	use crate::ideas::EMBED;
	use crate::reward::Yields;

	fn horizons(trade: f32) -> Horizons {
		Horizons {
			near: Yields {
				trade,
				..Yields::default()
			},
			far: Yields::default(),
		}
	}

	fn target(tag: &'static str) -> TargetOutcomes {
		let vanilla = horizons(10.0);
		let mut interests = [vanilla; 4];
		interests[Interest::FriendlyAlliance.index()] = horizons(14.0);
		TargetOutcomes {
			tag,
			vanilla,
			interests,
		}
	}

	const TAGS: [&str; 17] = [
		"T00", "T01", "T02", "T03", "T04", "T05", "T06", "T07", "T08", "T09", "T10", "T11", "T12",
		"T13", "T14", "T15", "T16",
	];

	#[test]
	fn a_month_uses_the_definition_the_hook_saw_last() {
		let mut loaded = crate::LoadedSnapshot::default();
		loaded.note_definition(crate::LoadedDatabase::Ideas, "economic_ideas");
		loaded.note_definition(crate::LoadedDatabase::Ideas, "trade_ideas");
		loaded.note_definition(
			crate::LoadedDatabase::RulerPersonalities,
			"entrepreneur_personality",
		);
		loaded.note_definition(
			crate::LoadedDatabase::GovernmentReforms,
			"merchant_republic",
		);
		let ideas: Vec<&str> = loaded.idea_groups().collect();
		let personalities: Vec<&str> = loaded.personalities().collect();
		let attributes: &[&str] = &[];
		assert_eq!(ideas, vec!["economic_ideas", "trade_ideas"]);
		let table = NameTable::default();
		let situation = Situation::from_names(&table, &ideas, &personalities, attributes);
		let mut policy = Policy::zeros();
		for column in 0..EMBED {
			policy.trade_head.rows[0][EMBED + column] = situation.personalities[column];
		}
		let targets: Vec<TargetOutcomes> = TAGS.into_iter().map(target).collect();
		let month = Month {
			ideas: &ideas,
			personalities: &personalities,
			attributes,
			provinces: 12.0,
			development: 140.0,
			government_rank: 2.0,
			subject: false,
			targets: &targets,
		};
		let scores = monthly(&policy, &table, &month);
		assert_eq!(scores.len(), 16);
		assert!(scores.iter().all(|score| {
			score.choice == Choice::Interest(Interest::FriendlyAlliance) && score.reward == 4.0
		}));
	}

	#[test]
	fn zero_weights_score_zero_against_vanilla() {
		let targets = [target("FRA")];
		let ideas = ["trade_ideas"];
		let personalities = ["entrepreneur_personality"];
		let month = Month {
			ideas: &ideas,
			personalities: &personalities,
			attributes: &[],
			provinces: 20.0,
			development: 300.0,
			government_rank: 3.0,
			subject: false,
			targets: &targets,
		};
		let scores = monthly(&Policy::zeros(), &NameTable::default(), &month);
		assert_eq!(
			scores,
			vec![TargetScore {
				tag: "FRA",
				choice: Choice::Vanilla,
				reward: 0.0,
			}]
		);
	}
}
