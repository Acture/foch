//! The static province graph.
//!
//! Two sources feed the same [`StaticGraph`]:
//!
//! * [`StaticGraph::from_json`] reads an already-extracted document
//!   (`eu4_provinces.json`). That file is a cache, not a vanilla
//!   assumption.
//! * [`StaticGraph::from_map_files`] builds the graph from the files
//!   of the active playset after [`resolve_playset`] applies each
//!   layer's `replace_path`. Land neighbors come from `provinces.bmp`
//!   (border-colored pixels are skipped), straits from
//!   `adjacencies.csv`, positions from `positions.txt`, and the
//!   bookmark owner / development from `history/provinces`.
//!
//! Memory reads stay reserved for dynamic fields.

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

	/// Build the graph from a resolved playset file map. Keys are
	/// forward-slash paths (`map/definition.csv`). `None` when the
	/// definition is missing, empty, or a present `provinces.bmp`
	/// cannot be decoded.
	pub fn from_map_files(files: &BTreeMap<String, Vec<u8>>) -> Option<Self> {
		let definition = files.get("map/definition.csv")?;
		let text = String::from_utf8_lossy(definition);
		let defined = parse_definition(&text);
		if defined.is_empty() {
			return None;
		}
		let mut neighbors: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
		if let Some(bmp) = files.get("map/provinces.bmp") {
			let image = parse_bmp24(bmp)?;
			link_bitmap(&image, &defined, &mut neighbors);
		}
		if let Some(csv) = files.get("map/adjacencies.csv") {
			link_adjacencies(&String::from_utf8_lossy(csv), &defined, &mut neighbors);
		}
		let positions = files
			.get("map/positions.txt")
			.map(|bytes| parse_positions(&String::from_utf8_lossy(bytes)))
			.unwrap_or_default();
		let history = history_facts(files);

		let mut graph = Self::default();
		for id in defined.keys() {
			let (x, y) = positions.get(id).copied().unwrap_or((0.0, 0.0));
			let (owner, development) = history.get(id).cloned().unwrap_or_default();
			let mut adjacent = neighbors.remove(id).unwrap_or_default();
			adjacent.sort_unstable();
			adjacent.dedup();
			adjacent.retain(|neighbor| defined.contains_key(neighbor) && neighbor != id);
			graph.provinces.insert(
				*id,
				ProvinceRecord {
					x,
					y,
					development,
					owner,
					neighbors: adjacent,
				},
			);
		}
		let index_of: BTreeMap<u32, u32> = graph
			.provinces
			.keys()
			.enumerate()
			.map(|(index, id)| (*id, index as u32))
			.collect();
		for (&id, record) in &graph.provinces {
			for &neighbor in &record.neighbors {
				if let (Some(&src), Some(&dst)) = (index_of.get(&id), index_of.get(&neighbor)) {
					graph.edges.push((id, neighbor));
					graph.index_edges.push((src, dst));
				}
			}
		}
		Some(graph)
	}
}

/// One ordered playset layer. `files` keys are forward-slash paths
/// relative to that layer's root. `replace_paths` are the
/// `replace_path` values from its `descriptor.mod`.
#[derive(Clone, Debug, Default)]
pub struct PlaysetLayer {
	pub replace_paths: Vec<String>,
	pub files: BTreeMap<String, Vec<u8>>,
}

/// `replace_path` values from a descriptor. Later layers drop every
/// earlier file under these prefixes before their own files overlay.
pub fn replace_paths_from_descriptor(text: &str) -> Vec<String> {
	let mut paths = Vec::new();
	for raw in text.lines() {
		let line = raw.split('#').next().unwrap_or("").trim();
		let Some((key, value)) = line.split_once('=') else {
			continue;
		};
		if key.trim() != "replace_path" {
			continue;
		}
		let value = value.trim().trim_matches('"').trim().trim_matches('"');
		if !value.is_empty() {
			paths.push(value.trim_matches('/').replace('\\', "/"));
		}
	}
	paths
}

/// Apply layers in playset order. A `replace_path` of `map` drops
/// `map/...` from everything earlier, then the layer's own files win
/// path-by-path.
pub fn resolve_playset(layers: &[PlaysetLayer]) -> BTreeMap<String, Vec<u8>> {
	let mut resolved = BTreeMap::new();
	for layer in layers {
		for prefix in &layer.replace_paths {
			let folder = format!("{prefix}/");
			resolved
				.retain(|path: &String, _| path != prefix && !path.starts_with(folder.as_str()));
		}
		for (path, bytes) in &layer.files {
			resolved.insert(path.replace('\\', "/"), bytes.clone());
		}
	}
	resolved
}

/// Read one playset root: `descriptor.mod`, `map/`, `history/provinces/`,
/// and `common/terrain/`. Missing optional directories are skipped.
pub fn load_layer(root: &std::path::Path) -> Result<PlaysetLayer, String> {
	if !root.is_dir() {
		return Err(format!(
			"playset layer is not a directory: {}",
			root.display()
		));
	}
	let descriptor = root.join("descriptor.mod");
	let replace_paths = if descriptor.is_file() {
		let text = std::fs::read_to_string(&descriptor)
			.map_err(|err| format!("{}: {err}", descriptor.display()))?;
		replace_paths_from_descriptor(&text)
	} else {
		Vec::new()
	};
	let mut files = BTreeMap::new();
	for relative in ["map", "history/provinces", "common/terrain"] {
		let dir = root.join(relative);
		if dir.is_dir() {
			collect_files(&dir, root, &mut files)?;
		}
	}
	Ok(PlaysetLayer {
		replace_paths,
		files,
	})
}

fn collect_files(
	dir: &std::path::Path,
	root: &std::path::Path,
	files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
	for entry in std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))? {
		let entry = entry.map_err(|err| err.to_string())?;
		let path = entry.path();
		if path.is_dir() {
			collect_files(&path, root, files)?;
		} else if path.is_file() {
			let relative = path
				.strip_prefix(root)
				.map_err(|err| err.to_string())?
				.components()
				.map(|component| component.as_os_str().to_string_lossy())
				.collect::<Vec<_>>()
				.join("/");
			if relative.split('/').any(|part| part == "..") {
				continue;
			}
			let bytes = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
			files.insert(relative, bytes);
		}
	}
	Ok(())
}

fn parse_definition(text: &str) -> BTreeMap<u32, (u8, u8, u8)> {
	let mut defined = BTreeMap::new();
	for raw in text.lines() {
		let line = raw.trim().trim_start_matches('\u{feff}');
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let mut parts = line.split(';');
		let Some(id) = parts
			.next()
			.and_then(|part| part.trim().parse::<u32>().ok())
		else {
			continue;
		};
		let Some(red) = parts.next().and_then(|part| part.trim().parse::<u8>().ok()) else {
			continue;
		};
		let Some(green) = parts.next().and_then(|part| part.trim().parse::<u8>().ok()) else {
			continue;
		};
		let Some(blue) = parts.next().and_then(|part| part.trim().parse::<u8>().ok()) else {
			continue;
		};
		defined.insert(id, (red, green, blue));
	}
	defined
}

struct Bmp24 {
	width: usize,
	height: usize,
	/// Row-major, top-down RGB triples.
	pixels: Vec<u8>,
}

fn parse_bmp24(bytes: &[u8]) -> Option<Bmp24> {
	if bytes.len() < 54 || &bytes[0..2] != b"BM" {
		return None;
	}
	let offset = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
	let header = u32::from_le_bytes(bytes[14..18].try_into().ok()?);
	if header < 40 || bytes.len() < 14 + 40 {
		return None;
	}
	let width = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
	let height_raw = i32::from_le_bytes(bytes[22..26].try_into().ok()?);
	let planes = u16::from_le_bytes(bytes[26..28].try_into().ok()?);
	let bit_count = u16::from_le_bytes(bytes[28..30].try_into().ok()?);
	let compression = u32::from_le_bytes(bytes[30..34].try_into().ok()?);
	if width <= 0 || height_raw == 0 || planes != 1 || bit_count != 24 || compression != 0 {
		return None;
	}
	let top_down = height_raw < 0;
	let width = width as usize;
	let height = height_raw.unsigned_abs() as usize;
	if width.checked_mul(height)? > 16_000_000 {
		return None;
	}
	let stride = width.checked_mul(3)?.checked_add(3)? & !3;
	let needed = offset.checked_add(stride.checked_mul(height)?)?;
	if needed > bytes.len() {
		return None;
	}
	let mut pixels = Vec::with_capacity(width * height * 3);
	for y in 0..height {
		let file_y = if top_down { y } else { height - 1 - y };
		let row = offset + file_y * stride;
		for x in 0..width {
			let index = row + x * 3;
			pixels.push(bytes[index + 2]);
			pixels.push(bytes[index + 1]);
			pixels.push(bytes[index]);
		}
	}
	Some(Bmp24 {
		width,
		height,
		pixels,
	})
}

/// How many non-province pixels a neighbor scan may cross. EU4 draws a
/// dark border between provinces; those pixels are not in definition.csv.
const BORDER_GAP: usize = 4;

fn link_bitmap(
	image: &Bmp24,
	defined: &BTreeMap<u32, (u8, u8, u8)>,
	neighbors: &mut BTreeMap<u32, Vec<u32>>,
) {
	let mut color_of = std::collections::HashMap::<u32, u32>::new();
	for (id, (red, green, blue)) in defined {
		color_of.insert(
			u32::from(*red) << 16 | u32::from(*green) << 8 | u32::from(*blue),
			*id,
		);
	}
	let mut undirected = std::collections::BTreeSet::<(u32, u32)>::new();
	for y in 0..image.height {
		for x in 0..image.width {
			let Some(id) = color_of.get(&pixel(image, x, y)).copied() else {
				continue;
			};
			if let Some(other) = neighbor_id(image, &color_of, x, y, 1, 0, id) {
				undirected.insert((id.min(other), id.max(other)));
			}
			if let Some(other) = neighbor_id(image, &color_of, x, y, 0, 1, id) {
				undirected.insert((id.min(other), id.max(other)));
			}
		}
	}
	for (left, right) in undirected {
		if defined.contains_key(&left) && defined.contains_key(&right) {
			neighbors.entry(left).or_default().push(right);
			neighbors.entry(right).or_default().push(left);
		}
	}
}

fn pixel(image: &Bmp24, x: usize, y: usize) -> u32 {
	let index = (y * image.width + x) * 3;
	u32::from(image.pixels[index]) << 16
		| u32::from(image.pixels[index + 1]) << 8
		| u32::from(image.pixels[index + 2])
}

fn neighbor_id(
	image: &Bmp24,
	colors: &std::collections::HashMap<u32, u32>,
	x: usize,
	y: usize,
	step_x: usize,
	step_y: usize,
	id: u32,
) -> Option<u32> {
	let mut skipped = 0usize;
	let mut cursor_x = x;
	let mut cursor_y = y;
	loop {
		cursor_x = cursor_x.checked_add(step_x)?;
		cursor_y = cursor_y.checked_add(step_y)?;
		if cursor_x >= image.width || cursor_y >= image.height {
			return None;
		}
		match colors.get(&pixel(image, cursor_x, cursor_y)).copied() {
			Some(other) if other == id => return None,
			Some(other) => return Some(other),
			None => {
				skipped += 1;
				if skipped > BORDER_GAP {
					return None;
				}
			}
		}
	}
}

fn link_adjacencies(
	text: &str,
	defined: &BTreeMap<u32, (u8, u8, u8)>,
	neighbors: &mut BTreeMap<u32, Vec<u32>>,
) {
	for raw in text.lines() {
		let line = raw.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let mut parts = line.split(';');
		let Some(from) = parts
			.next()
			.and_then(|part| part.trim().parse::<u32>().ok())
		else {
			continue;
		};
		let Some(to) = parts
			.next()
			.and_then(|part| part.trim().parse::<u32>().ok())
		else {
			continue;
		};
		if from != to && defined.contains_key(&from) && defined.contains_key(&to) {
			neighbors.entry(from).or_default().push(to);
			neighbors.entry(to).or_default().push(from);
		}
	}
}

fn parse_positions(text: &str) -> BTreeMap<u32, (f32, f32)> {
	let mut out = BTreeMap::new();
	let mut current = None;
	let mut in_position = false;
	let mut found = 0u8;
	let mut x = 0.0f32;
	for raw in text.lines() {
		let line = raw.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		if !in_position && let Some((head, _)) = line.split_once('=') {
			let head = head.trim();
			if head.chars().all(|c| c.is_ascii_digit()) && line.contains('{') {
				current = head.parse().ok();
				found = 0;
			}
		}
		if !in_position && line.contains("position") && line.contains('{') {
			in_position = true;
		}
		if !in_position {
			continue;
		}
		for token in line.split_whitespace() {
			let token = token.trim_matches(|c| c == '{' || c == '}');
			if token.is_empty() {
				continue;
			}
			let Ok(value) = token.parse::<f32>() else {
				continue;
			};
			if found == 0 {
				x = value;
				found = 1;
			} else if found == 1 {
				if let Some(id) = current {
					out.insert(id, (x, value));
				}
				found = 2;
				in_position = false;
				break;
			}
		}
		if in_position && line.contains('}') && found >= 2 {
			in_position = false;
		}
	}
	out
}

fn history_facts(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<u32, (String, u32)> {
	let mut facts = BTreeMap::new();
	for (path, bytes) in files {
		if !path.starts_with("history/provinces/") || !path.ends_with(".txt") {
			continue;
		}
		let Some(id) = history_province_id(path) else {
			continue;
		};
		facts.insert(id, parse_history(&String::from_utf8_lossy(bytes)));
	}
	facts
}

fn history_province_id(path: &str) -> Option<u32> {
	let name = path.rsplit('/').next()?;
	let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
	if digits.is_empty() {
		None
	} else {
		digits.parse().ok()
	}
}

fn parse_history(text: &str) -> (String, u32) {
	let mut owner = String::new();
	let mut development = 0u32;
	for raw in text.lines() {
		let line = raw.split('#').next().unwrap_or("").trim();
		if line.is_empty() {
			continue;
		}
		if line.chars().next().is_some_and(|c| c.is_ascii_digit()) && line.contains('.') {
			break;
		}
		let Some((key, value)) = line.split_once('=') else {
			continue;
		};
		let value = value.trim().trim_matches('"');
		match key.trim() {
			"owner" => owner = value.to_string(),
			"base_tax" | "base_production" | "base_manpower" => {
				development = development.saturating_add(value.parse::<u32>().unwrap_or(0));
			}
			_ => {}
		}
	}
	(owner, development)
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

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

	fn layer(files: &[(&str, &str)]) -> PlaysetLayer {
		PlaysetLayer {
			replace_paths: Vec::new(),
			files: files
				.iter()
				.map(|(path, text)| ((*path).to_string(), text.as_bytes().to_vec()))
				.collect(),
		}
	}

	#[test]
	fn replace_path_drops_the_earlier_map_and_keeps_other_files() {
		let mut game = layer(&[("map/definition.csv", "1;1;0;0;Old;x\n")]);
		game.files.insert(
			"history/provinces/1-Old.txt".into(),
			b"owner = SWE\n".to_vec(),
		);
		let mut overhaul = layer(&[("map/definition.csv", "10;0;1;0;New;x\n")]);
		overhaul.replace_paths.push("map".into());
		let resolved = resolve_playset(&[game, overhaul]);
		assert!(resolved["map/definition.csv"].starts_with(b"10;"));
		assert!(resolved.contains_key("history/provinces/1-Old.txt"));
		assert_eq!(
			replace_paths_from_descriptor("replace_path=\"map\"\n"),
			vec!["map".to_string()]
		);
	}

	#[test]
	fn the_bitmap_and_history_build_adjacency_without_a_vanilla_json() {
		let definition = "province;red;green;blue;name;x\n1;255;0;0;Alpha;x\n2;0;0;255;Beta;x\n";
		let row = [(255, 0, 0), (0, 0, 0), (0, 0, 255)];
		let bmp = encode_bmp24(&[&row]);
		let positions = "1={\n\tposition={\n\t\t10.0 20.0 0.0 0.0\n\t}\n}\n2={\n\tposition={\n\t\t30.0 40.0\n\t}\n}\n";
		let history = "owner = SWE\nbase_tax = 5\nbase_production = 4\nbase_manpower = 3\n1444.11.11 = {\n\towner = DAN\n}\n";
		let adjacency = "From;To;Type;Through;start_x;start_y;stop_x;stop_y;Comment\n1;9;sea;3;-1;-1;-1;-1;missing\n";
		let mut files = BTreeMap::new();
		files.insert("map/definition.csv".into(), definition.into());
		files.insert("map/provinces.bmp".into(), bmp);
		files.insert("map/positions.txt".into(), positions.into());
		files.insert("map/adjacencies.csv".into(), adjacency.into());
		files.insert("history/provinces/1-Alpha.txt".into(), history.into());
		let graph = StaticGraph::from_map_files(&files).expect("map");
		assert_eq!(graph.provinces.len(), 2);
		assert_eq!(graph.provinces[&1].neighbors, vec![2]);
		assert_eq!(graph.provinces[&2].neighbors, vec![1]);
		assert_eq!(graph.provinces[&1].owner, "SWE");
		assert_eq!(graph.provinces[&1].development, 12);
		assert_eq!((graph.provinces[&1].x, graph.provinces[&1].y), (10.0, 20.0));
		assert!(graph.edges.contains(&(1, 2)) && graph.edges.contains(&(2, 1)));
		assert!(StaticGraph::from_map_files(&BTreeMap::new()).is_none());
	}

	fn encode_bmp24(rows: &[&[(u8, u8, u8)]]) -> Vec<u8> {
		let height = rows.len();
		let width = rows[0].len();
		let stride = (width * 3 + 3) & !3;
		let offset = 54usize;
		let pixels = stride * height;
		let mut out = vec![0u8; offset + pixels];
		out[0] = b'B';
		out[1] = b'M';
		out[10..14].copy_from_slice(&(offset as u32).to_le_bytes());
		out[14..18].copy_from_slice(&40u32.to_le_bytes());
		out[18..22].copy_from_slice(&(width as i32).to_le_bytes());
		out[22..26].copy_from_slice(&(height as i32).to_le_bytes());
		out[26..28].copy_from_slice(&1u16.to_le_bytes());
		out[28..30].copy_from_slice(&24u16.to_le_bytes());
		for (y, row) in rows.iter().enumerate() {
			let file_y = height - 1 - y;
			for (x, (red, green, blue)) in row.iter().enumerate() {
				let index = offset + file_y * stride + x * 3;
				out[index] = *blue;
				out[index + 1] = *green;
				out[index + 2] = *red;
			}
		}
		out
	}
}
