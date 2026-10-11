//! Map groups a province search narrows by, from the effective map files.

use crate::game::eu4::script::parser::{AstFile, AstStatement, AstValue};

/// Superregions with their regions, and regions with their areas, in file
/// order. Province triggers `superregion`, `region` and `area` test the same
/// names, and each name is also its own localisation key in the game.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Geography {
	pub superregions: Vec<(String, Vec<String>)>,
	pub regions: Vec<(String, Vec<String>)>,
}

impl Geography {
	/// Read the effective `map/superregion.txt` and `map/region.txt`.
	/// Superregion entries that are not regions, such as `restrict_charter`,
	/// are ignored, as are empty groups.
	pub fn from_map(superregion: &AstFile, region: &AstFile) -> Self {
		let regions = blocks(region)
			.map(|(name, items)| {
				let areas = items
					.iter()
					.find_map(|statement| match statement {
						AstStatement::Assignment {
							key,
							value: AstValue::Block { items, .. },
							..
						} if key == "areas" => Some(names(items)),
						_ => None,
					})
					.unwrap_or_default();
				(name, areas)
			})
			.filter(|(_, areas)| !areas.is_empty())
			.collect::<Vec<_>>();
		let superregions = blocks(superregion)
			.map(|(name, items)| {
				let members = names(items)
					.into_iter()
					.filter(|member| regions.iter().any(|(region, _)| region == member))
					.collect::<Vec<_>>();
				(name, members)
			})
			.filter(|(_, members)| !members.is_empty())
			.collect();
		Self {
			superregions,
			regions,
		}
	}

	pub fn is_empty(&self) -> bool {
		self.superregions.is_empty() && self.regions.is_empty()
	}
}

fn blocks(file: &AstFile) -> impl Iterator<Item = (String, &[AstStatement])> {
	file.statements
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Assignment {
				key,
				value: AstValue::Block { items, .. },
				..
			} => Some((key.clone(), items.as_slice())),
			_ => None,
		})
}

fn names(items: &[AstStatement]) -> Vec<String> {
	items
		.iter()
		.filter_map(|statement| match statement {
			AstStatement::Item {
				value: AstValue::Scalar { value, .. },
				..
			} => Some(value.as_text()),
			_ => None,
		})
		.collect()
}
