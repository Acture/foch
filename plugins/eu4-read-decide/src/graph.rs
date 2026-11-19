//! The static province graph, loaded from the extracted game-file asset.
//!
//! Map topology — positions, adjacency, baseline development, the
//! bookmark owner — never changes at runtime, so the reader takes it
//! from `eu4_provinces.json` (produced by the offline extraction;
//! sample: `{"provinces": {"1": {"x": .., "y": .., "development": ..,
//! "owner": "SWE", "neighbors": [..]}}}`) and reserves memory reads
//! for the dynamic fields.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The parsed static graph.
#[derive(Clone, Debug, Default)]
pub struct StaticGraph {
	/// Province id -> record, ordered.
	pub provinces: BTreeMap<u32, ProvinceRecord>,
	/// Directed edges as (src, dst) PROVINCE IDS; both directions listed.
	pub edges: Vec<(u32, u32)>,
	/// The same edges as NODE INDICES — the addressing the plane and
	/// the GNN consume. Same length and order as `edges`.
	pub index_edges: Vec<(u32, u32)>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProvinceRecord {
	pub x: f32,
	pub y: f32,
	pub development: u32,
	/// Owner tag at the extraction bookmark, e.g. `"SWE"`.
	pub owner: String,
	pub neighbors: Vec<u32>,
}

#[derive(Deserialize)]
struct FileRoot {
	provinces: BTreeMap<String, ProvinceRecord>,
}

impl StaticGraph {
	/// Parse the extraction JSON. Unknown province ids in neighbor
	/// lists are dropped (the extraction covers the largest connected
	/// component only).
	pub fn from_json(text: &str) -> Option<Self> {
		let root: FileRoot = serde_json::from_str(text).ok()?;
		let mut graph = Self::default();
		for (id, record) in root.provinces {
			let id: u32 = id.parse().ok()?;
			graph.provinces.insert(id, record);
		}
		for (&id, record) in &graph.provinces {
			for &neighbor in &record.neighbors {
				if graph.provinces.contains_key(&neighbor) && neighbor != id {
					let (a, b) = (graph.index_of(id), graph.index_of(neighbor));
					if let (Some(a), Some(b)) = (a, b) {
						graph.edges.push((id, neighbor));
						graph.index_edges.push((a as u32, b as u32));
					}
				}
			}
		}
		Some(graph)
	}

	/// Node index of a province in row order (first insertion wins on a
	/// missing id — callers pass validated frames only).
	pub fn index_of(&self, id: u32) -> Option<usize> {
		self.provinces.keys().position(|&key| key == id)
	}

	/// The province id at a node index.
	pub fn id_at(&self, index: usize) -> Option<u32> {
		self.provinces.keys().nth(index).copied()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_and_symmetrizes_the_extraction_format() {
		let text = r#"{"provinces": {
			"1": {"x": 10.0, "y": 20.0, "development": 13, "owner": "SWE", "neighbors": [2, 999]},
			"2": {"x": 30.0, "y": 40.0, "development": 8, "owner": "DAN", "neighbors": [1]}
		}}"#;
		let graph = StaticGraph::from_json(text).expect("parse");
		assert_eq!(graph.provinces.len(), 2);
		// The neighbor pointing outside the component is dropped; the
		// mutual pair survives as two directed edges, ids and indices.
		assert_eq!(graph.edges, vec![(1, 2), (2, 1)]);
		assert_eq!(graph.index_edges, vec![(0, 1), (1, 0)]);
		assert_eq!(graph.index_of(2), Some(1));
		assert_eq!(graph.id_at(0), Some(1));
	}

	#[test]
	fn malformed_documents_are_refused() {
		assert!(StaticGraph::from_json("").is_none());
		assert!(StaticGraph::from_json("{\"provinces\": {\"x\": {}}").is_none());
		assert!(
			StaticGraph::from_json(
				"{\"provinces\": {\"not-a-number\": \
			{\"x\":0.0,\"y\":0.0,\"development\":1,\"owner\":\"A\",\"neighbors\":[]}}}"
			)
			.is_none()
		);
	}
}
